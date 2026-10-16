//! Match history for the window: the library of doc 03 §7. Every finished match gets a
//! folder with its `match.json` (the tracker's [`MatchSummary`] plus `schema` and `id`)
//! and, when it was recorded, `match.mp4`. Recordings saved by `downplayed-dev record`
//! are listed too and replayed into the same summary, so the window reviews both the
//! same way.
//!
//! A recorded match's folder is created when recording starts, named from what is known
//! then (`<date>_<time>[_<queue>]`), and renamed to the full name when the match is
//! saved. Clips are added to it later, from the review (`plan_clip`, `add_clip`).
//!
//! The list comes from an index of the folders (`index`, `library.db` in the library's
//! root): the sidecars are the truth, the index a cache kept in step with them.

mod index;

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

use index::{ClipEntry, Entry, Index, Row, Stamp};
pub use index::{Facets, MatchQuery, Opened, Sort, Source};

use crate::capture::clips::{
    clip_label, plan_span, Clip, ClipPlan, CLIPS_DIR, KEYFRAME_S, THUMBS_DIR,
};
use crate::recorder::utc_parts;
use crate::replay::{replay_with, Fixture, LcuRecording};
use crate::tracker::normalize::MarkerLabel;
use crate::tracker::session::{MatchSummary, SessionConfig};

pub const MATCH_FILE: &str = "match.json";
pub const MATCH_SCHEMA: u32 = 1;
/// The full-match video in a match folder.
pub const VIDEO_FILE: &str = "match.mp4";
/// A match's picture in the list, inside its folder.
pub const MATCH_THUMBNAIL: &str = "thumbs/match.jpg";
const MATCH_THUMBNAIL_NAME: &str = "match.jpg";
/// The index, in the library's root (beside the matches folder).
pub const INDEX_FILE: &str = "library.db";

/// Recordings are addressed as `recording:<file stem>`.
const RECORDING_PREFIX: &str = "recording:";
/// What recordings were replayed with: replayed again when the marker settings change.
const RECORDINGS_CONFIG_KEY: &str = "recordings_config";
/// The list's first page looks at the folders again (for changes made outside
/// Downplayed) when the last look is older than this.
const QUERY_SYNC_AGE: Duration = Duration::from_secs(10);

/// One row of the match list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchCard {
    pub id: String,
    pub source: Source,
    /// When the match was tracked or the recording saved (Unix ms).
    pub when_ms: u64,
    pub champion: Option<String>,
    pub queue: Option<String>,
    pub game_mode: Option<String>,
    pub result: Option<String>,
    pub kills: Option<u64>,
    pub deaths: Option<u64>,
    pub assists: Option<u64>,
    pub duration_s: Option<f64>,
    pub moments: usize,
    /// Double kills or better.
    pub multikills: usize,
    pub clips: usize,
    pub favorite_clips: usize,
    /// The match was recorded (`match.mp4` is there).
    pub video: bool,
    /// Its picture, a full path for the asset protocol, once made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_path: Option<String>,
}

/// A page of the list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPage {
    /// How many matches pass the filters, in all.
    pub total: u64,
    pub offset: u32,
    pub items: Vec<MatchCard>,
}

/// What a sync of the index did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Sidecars read (new or changed).
    pub read: usize,
    /// Entries dropped (their folder or recording is gone).
    pub removed: usize,
    /// Entries in the index afterwards.
    pub total: u64,
    pub elapsed: Duration,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchFile<'a> {
    schema: u32,
    id: &'a str,
    #[serde(flatten)]
    summary: &'a MatchSummary,
}

/// A clip to cut out of a saved match: from its video, into its folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipJob {
    pub video: PathBuf,
    pub folder: PathBuf,
    pub plan: ClipPlan,
}

/// A match's thumbnail to make: one frame of its video.
#[derive(Debug, Clone, PartialEq)]
pub struct ThumbnailJob {
    pub video: PathBuf,
    pub at_s: f64,
    pub out: PathBuf,
}

/// A clip moved out of a match (to the Recycle Bin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// The recycling was cancelled or refused: nothing changed.
    Kept,
}

pub struct Library {
    matches_dir: PathBuf,
    recordings_dir: PathBuf,
    /// How recordings are replayed (the user's marker settings).
    config: SessionConfig,
    index: Index,
    /// Syncs go one at a time; when the last one finished.
    synced: Mutex<Option<Instant>>,
}

impl Library {
    /// The library of `matches_dir`, its index in the folder above (the library's root).
    pub fn new(matches_dir: PathBuf, recordings_dir: PathBuf, config: SessionConfig) -> Self {
        let index = match matches_dir.parent() {
            Some(root) if !root.as_os_str().is_empty() => Index::open(&root.join(INDEX_FILE)),
            _ => Index::in_memory(),
        };
        Self {
            matches_dir,
            recordings_dir,
            config,
            index,
            synced: Mutex::new(None),
        }
    }

    /// How the index came up: kept, made anew, or (unusable file) in memory.
    pub fn index_opened(&self) -> Opened {
        self.index.opened()
    }

    /// The index file, unless it lives in memory.
    pub fn index_path(&self) -> Option<&Path> {
        self.index.path()
    }

    /// Saves a finished match as `<date>_<time>_<queue>_<champion>/match.json` (UTC) and
    /// returns its id, the folder name. A recorded match keeps its folder (and video),
    /// renamed to that name.
    pub fn save(&self, summary: &MatchSummary) -> io::Result<String> {
        let base = folder_name(
            summary.started_wall_ms,
            [
                summary.queue.as_deref().or(summary.game_mode.as_deref()),
                summary.champion.as_deref(),
            ],
        );
        std::fs::create_dir_all(&self.matches_dir)?;
        let recorded = summary
            .recording
            .as_ref()
            .map(|r| r.folder.as_path())
            .filter(|folder| {
                folder.is_dir() && folder.parent() == Some(self.matches_dir.as_path())
            });
        let (id, dir) = match recorded {
            Some(folder) => adopt_folder(&self.matches_dir, folder, &base),
            None => create_unique_dir(&self.matches_dir, &base)?,
        };
        let file = MatchFile {
            schema: MATCH_SCHEMA,
            id: &id,
            summary,
        };
        write_json(&dir.join(MATCH_FILE), &file)?;
        self.refresh(&id);
        Ok(id)
    }

    /// Brings the index up to date with the folders: reads the sidecars that are new or
    /// changed since it last looked, replays recordings that changed (all of them if the
    /// marker settings did), and drops what is gone. With nothing changed it costs a
    /// directory listing and two stats per match.
    pub fn sync(&self) -> SyncReport {
        let mut synced = self.synced.lock().unwrap_or_else(PoisonError::into_inner);
        let report = self.sync_now();
        *synced = Some(Instant::now());
        report
    }

    /// Syncs unless one finished less than `age` ago (or is running: this waits for it).
    fn sync_if_older(&self, age: Duration) {
        let mut synced = self.synced.lock().unwrap_or_else(PoisonError::into_inner);
        if synced.is_some_and(|at| at.elapsed() < age) {
            return;
        }
        self.sync_now();
        *synced = Some(Instant::now());
    }

    fn sync_now(&self) -> SyncReport {
        let started = Instant::now();
        let mut entries = Vec::new();
        let mut removed = Vec::new();

        let indexed = self.index.stamps(Source::Game).unwrap_or_default();
        let mut seen = HashSet::new();
        for dir in read_dir(&self.matches_dir) {
            let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            let Some(stamp) = folder_stamp(&dir) else {
                continue;
            };
            if indexed.get(&id) == Some(&stamp) {
                seen.insert(id);
                continue;
            }
            match read_json(&dir.join(MATCH_FILE)) {
                Some(summary) => {
                    seen.insert(id.clone());
                    entries.push(game_entry(&id, &dir, &summary, stamp));
                }
                None => tracing::debug!(%id, "unreadable match.json; not listed"),
            }
        }
        removed.extend(indexed.into_keys().filter(|id| !seen.contains(id)));

        let config = format!("{:?}", self.config);
        let same_config = self
            .index
            .meta(RECORDINGS_CONFIG_KEY)
            .ok()
            .flatten()
            .as_deref()
            == Some(&config);
        let indexed = self.index.stamps(Source::Recording).unwrap_or_default();
        let mut seen = HashSet::new();
        for path in read_dir(&self.recordings_dir) {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let id = format!("{RECORDING_PREFIX}{stem}");
            let Some(stamp) = file_stamp(&path) else {
                continue;
            };
            if same_config && indexed.get(&id) == Some(&stamp) {
                seen.insert(id);
                continue;
            }
            if let Some(summary) = self.replay_recording(&path) {
                seen.insert(id.clone());
                entries.push(recording_entry(&id, &summary, stamp));
            }
        }
        removed.extend(indexed.into_keys().filter(|id| !seen.contains(id)));

        let mut report = SyncReport {
            read: entries.len(),
            removed: removed.len(),
            ..SyncReport::default()
        };
        if !entries.is_empty() || !removed.is_empty() {
            if let Err(error) = self.index.apply(&entries, &removed) {
                tracing::warn!(%error, "could not update the library index");
            }
        }
        if !same_config {
            let _ = self.index.set_meta(RECORDINGS_CONFIG_KEY, &config);
        }
        report.total = self.index.len().unwrap_or_default();
        report.elapsed = started.elapsed();
        report
    }

    /// Re-reads one match folder into the index (or drops it, if it's gone).
    fn refresh(&self, id: &str) {
        let Some(name) = safe_name(id) else {
            return;
        };
        let dir = self.matches_dir.join(name);
        let entry = folder_stamp(&dir).and_then(|stamp| {
            read_json(&dir.join(MATCH_FILE)).map(|summary| game_entry(id, &dir, &summary, stamp))
        });
        let result = match entry {
            Some(entry) => self.index.apply(&[entry], &[]),
            None => self.index.apply(&[], &[id.to_owned()]),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "could not update the library index");
        }
    }

    /// A page of matches and recordings, filtered and sorted. The first page brings the
    /// index up to date first, unless that was done a moment ago.
    pub fn query(&self, query: &MatchQuery) -> io::Result<MatchPage> {
        if query.offset == 0 {
            self.sync_if_older(QUERY_SYNC_AGE);
        }
        let (total, rows) = self.index.query(query).map_err(io::Error::other)?;
        Ok(MatchPage {
            total,
            offset: query.offset,
            items: rows.into_iter().map(|row| self.card(row)).collect(),
        })
    }

    /// The champions and queues the filters can offer.
    pub fn facets(&self) -> io::Result<Facets> {
        self.index.facets().map_err(io::Error::other)
    }

    /// Every saved match and recording, newest first.
    pub fn list(&self) -> Vec<MatchCard> {
        let mut cards = Vec::new();
        loop {
            let query = MatchQuery {
                offset: u32::try_from(cards.len()).unwrap_or(u32::MAX),
                limit: 500,
                ..MatchQuery::default()
            };
            match self.query(&query) {
                Ok(page) if !page.items.is_empty() => {
                    let total = page.total;
                    cards.extend(page.items);
                    if cards.len() as u64 >= total {
                        break;
                    }
                }
                _ => break,
            }
        }
        cards
    }

    fn card(&self, row: Row) -> MatchCard {
        let count = |n: i64| usize::try_from(n).unwrap_or_default();
        let number = |n: Option<i64>| n.and_then(|n| u64::try_from(n).ok());
        let thumbnail_path = row
            .thumbnail
            .as_deref()
            .filter(|_| row.source == Source::Game)
            .and_then(|file| member_file(&self.matches_dir.join(&row.id), THUMBS_DIR, file))
            .map(|path| path.display().to_string());
        MatchCard {
            source: row.source,
            when_ms: u64::try_from(row.when_ms).unwrap_or_default(),
            champion: row.champion,
            queue: row.queue,
            game_mode: row.game_mode,
            result: row.result,
            kills: number(row.kills),
            deaths: number(row.deaths),
            assists: number(row.assists),
            duration_s: row.duration_s,
            moments: count(row.moments),
            multikills: count(row.multikills),
            clips: count(row.clips),
            favorite_clips: count(row.favorite_clips),
            video: row.video,
            thumbnail_path,
            id: row.id,
        }
    }

    /// A match or recording as the window reviews it: the summary's fields plus `id`,
    /// `source`, `videoPath` (the recorded video, for the asset protocol) and, for
    /// recordings, `description`.
    pub fn get(&self, id: &str) -> Option<Value> {
        if let Some(stem) = id.strip_prefix(RECORDING_PREFIX) {
            let path = self
                .recordings_dir
                .join(format!("{}.json", safe_name(stem)?));
            let mut summary = self.replay_recording(&path)?;
            let description = Fixture::load(&path).ok()?.description;
            let object = summary.as_object_mut()?;
            object.insert("id".into(), id.into());
            object.insert("source".into(), "recording".into());
            object.insert("description".into(), description.into());
            return Some(summary);
        }
        let dir = self.matches_dir.join(safe_name(id)?);
        let mut summary = read_json(&dir.join(MATCH_FILE))?;
        let video = video_path(&dir, &summary);
        let object = summary.as_object_mut()?;
        object.insert("source".into(), "game".into());
        if let Some(video) = video {
            object.insert("videoPath".into(), video.display().to_string().into());
        }
        if let Some(clips) = object.get_mut("clips").and_then(Value::as_array_mut) {
            for clip in clips.iter_mut().filter_map(Value::as_object_mut) {
                add_paths(&dir, clip);
            }
        }
        Some(summary)
    }

    /// Plans a clip of `span` (video seconds) of a saved match: numbered after every clip
    /// it has, named after `label`, its thumbnail at `thumbnail_s` (the middle of the span
    /// by default).
    pub fn plan_clip(
        &self,
        id: &str,
        span: (f64, f64),
        label: &str,
        thumbnail_s: Option<f64>,
    ) -> io::Result<ClipJob> {
        let not_found = |what: &str| io::Error::new(io::ErrorKind::NotFound, what.to_owned());
        let dir = self
            .matches_dir
            .join(safe_name(id).ok_or_else(|| not_found("match not found"))?);
        let summary =
            read_json(&dir.join(MATCH_FILE)).ok_or_else(|| not_found("match not found"))?;
        let video =
            video_path(&dir, &summary).ok_or_else(|| not_found("this match has no video"))?;
        let recording = &summary["recording"];
        let video_s = match (recording["frames"].as_f64(), recording["fps"].as_f64()) {
            (Some(frames), Some(fps)) if fps > 0.0 => frames / fps,
            _ => f64::INFINITY,
        };
        let thumbnail = thumbnail_s.unwrap_or((span.0 + span.1) / 2.0);
        let plan = plan_span(
            next_clip_number(&dir, &summary),
            &clip_label(label),
            span,
            thumbnail,
            video_s,
            KEYFRAME_S,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "that part isn't in the recording, or is under a second",
            )
        })?;
        Ok(ClipJob {
            video,
            folder: dir,
            plan,
        })
    }

    /// Lists a new clip in its match's `match.json`. Returns it as [`get`](Self::get)
    /// shows clips, with its file paths.
    pub fn add_clip(&self, id: &str, clip: &Clip) -> io::Result<Value> {
        let not_found = || io::Error::new(io::ErrorKind::NotFound, "match not found");
        let dir = self.matches_dir.join(safe_name(id).ok_or_else(not_found)?);
        let path = dir.join(MATCH_FILE);
        let mut summary = read_json(&path).ok_or_else(not_found)?;
        let entry = serde_json::to_value(clip).map_err(io::Error::other)?;
        let object = summary.as_object_mut().ok_or_else(not_found)?;
        match object.get_mut("clips").and_then(Value::as_array_mut) {
            Some(clips) => clips.push(entry.clone()),
            None => {
                object.insert("clips".into(), Value::Array(vec![entry.clone()]));
            }
        }
        write_json(&path, &summary)?;
        self.refresh(id);
        let mut shown = entry;
        if let Some(fields) = shown.as_object_mut() {
            add_paths(&dir, fields);
        }
        Ok(shown)
    }

    /// Takes a clip out of a saved match: its file and thumbnail go to `recycle` (the
    /// Recycle Bin in the app; nothing is ever deleted outright), then it leaves the
    /// match's `clips` and the index. If `recycle` declines (cancelled, or the files
    /// can't be recycled) nothing changes. A clip whose files are already gone is just
    /// taken off the list.
    pub fn remove_clip(
        &self,
        id: &str,
        file: &str,
        recycle: impl FnOnce(&[PathBuf]) -> Result<bool, String>,
    ) -> Result<Removal, String> {
        let dir = self
            .matches_dir
            .join(safe_name(id).ok_or("match not found")?);
        let path = dir.join(MATCH_FILE);
        let mut summary = read_json(&path).ok_or("match not found")?;
        let clips = summary["clips"].as_array_mut().ok_or("clip not found")?;
        let at = clips
            .iter()
            .position(|clip| clip["file"].as_str() == Some(file))
            .ok_or("clip not found")?;
        let files: Vec<PathBuf> = [
            member_file(&dir, CLIPS_DIR, file),
            clips[at]["thumbnail"]
                .as_str()
                .and_then(|thumb| member_file(&dir, THUMBS_DIR, thumb)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !files.is_empty() && !recycle(&files)? {
            return Ok(Removal::Kept);
        }
        clips.remove(at);
        write_json(&path, &summary).map_err(|e| e.to_string())?;
        self.refresh(id);
        Ok(Removal::Removed)
    }

    /// What a match's list thumbnail is made from, if it has a video and no thumbnail
    /// yet: the main event of its best highlight, else a third of the way in.
    pub fn thumbnail_job(&self, id: &str) -> Option<ThumbnailJob> {
        let dir = self.matches_dir.join(safe_name(id)?);
        let out = match_thumbnail(&dir);
        if out.is_file() {
            return None;
        }
        let summary = read_json(&dir.join(MATCH_FILE))?;
        let video = video_path(&dir, &summary)?;
        Some(ThumbnailJob {
            video,
            at_s: thumbnail_moment(&summary),
            out,
        })
    }

    /// Records a match's thumbnail once made; returns its full path.
    pub fn set_match_thumbnail(&self, id: &str) -> Option<PathBuf> {
        let path = match_thumbnail(&self.matches_dir.join(safe_name(id)?));
        if !path.is_file() {
            return None;
        }
        match self.index.set_thumbnail(id, Some(MATCH_THUMBNAIL)) {
            Ok(_) => Some(path),
            Err(error) => {
                tracing::warn!(%error, "could not update the library index");
                None
            }
        }
    }

    /// A saved match's clip file, if the match lists it and it exists (for "show in
    /// Explorer").
    pub fn clip_file(&self, id: &str, file: &str) -> Option<PathBuf> {
        let dir = self.matches_dir.join(safe_name(id)?);
        let summary = read_json(&dir.join(MATCH_FILE))?;
        let listed = summary["clips"]
            .as_array()?
            .iter()
            .any(|clip| clip["file"].as_str() == Some(file));
        listed.then(|| member_file(&dir, CLIPS_DIR, file)).flatten()
    }

    /// Marks a clip as a favorite (or not) in its match's `match.json`. Returns `false`
    /// when the match doesn't list the clip.
    pub fn set_clip_favorite(&self, id: &str, file: &str, favorite: bool) -> io::Result<bool> {
        let not_found = || io::Error::new(io::ErrorKind::NotFound, "match not found");
        let dir = self.matches_dir.join(safe_name(id).ok_or_else(not_found)?);
        let path = dir.join(MATCH_FILE);
        let mut summary = read_json(&path).ok_or_else(not_found)?;
        let Some(clip) = summary["clips"].as_array_mut().and_then(|clips| {
            clips
                .iter_mut()
                .find(|clip| clip["file"].as_str() == Some(file))
        }) else {
            return Ok(false);
        };
        clip["favorite"] = favorite.into();
        write_json(&path, &summary)?;
        self.refresh(id);
        Ok(true)
    }

    fn replay_recording(&self, path: &Path) -> Option<Value> {
        let fixture = Fixture::load(path).ok()?;
        let lcu = LcuRecording::load_beside(path).unwrap_or_default();
        let report = replay_with(&fixture, &lcu, &self.config).ok()?;
        serde_json::to_value(&report.summary).ok()
    }
}

/// Where a match folder's list thumbnail is (`MATCH_THUMBNAIL`).
fn match_thumbnail(dir: &Path) -> PathBuf {
    dir.join(THUMBS_DIR).join(MATCH_THUMBNAIL_NAME)
}

/// A recording's stamp for the index: its modification time and size (no video).
/// `None` if the file isn't there.
fn file_stamp(file: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(file).ok().filter(|m| m.is_file())?;
    Some(Stamp {
        modified_ns: unix_ns(meta.modified().unwrap_or(UNIX_EPOCH)),
        len: i64::try_from(meta.len()).unwrap_or(i64::MAX),
        video_len: -1,
    })
}

/// A match folder's stamp from one listing of it (on Windows the listing carries each
/// file's size and time, so this costs one directory read rather than a stat per file).
/// `None` without a `match.json`.
fn folder_stamp(dir: &Path) -> Option<Stamp> {
    let mut sidecar = None;
    let mut video = -1;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        if name == MATCH_FILE || name == VIDEO_FILE {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let len = i64::try_from(meta.len()).unwrap_or(i64::MAX);
            if name == VIDEO_FILE {
                video = len;
            } else {
                sidecar = Some((unix_ns(meta.modified().unwrap_or(UNIX_EPOCH)), len));
            }
        }
    }
    let (modified_ns, len) = sidecar?;
    Some(Stamp {
        modified_ns,
        len,
        video_len: video,
    })
}

/// A saved match as the index keeps it.
fn game_entry(id: &str, dir: &Path, summary: &Value, stamp: Stamp) -> Entry {
    let clips = summary["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|clip| {
            Some(ClipEntry {
                file: clip["file"].as_str()?.to_owned(),
                label: clip["label"].as_str().unwrap_or_default().to_owned(),
                from_s: clip["fromS"].as_f64().unwrap_or_default(),
                to_s: clip["toS"].as_f64().unwrap_or_default(),
                favorite: clip["favorite"].as_bool().unwrap_or_default(),
            })
        })
        .collect();
    Entry {
        video: video_path(dir, summary).is_some(),
        thumbnail: match_thumbnail(dir)
            .is_file()
            .then(|| MATCH_THUMBNAIL.to_owned()),
        clips,
        ..entry(
            id,
            Source::Game,
            summary["startedWallMs"].as_i64(),
            summary,
            stamp,
        )
    }
}

/// A replayed recording as the index keeps it: dated by its file.
fn recording_entry(id: &str, summary: &Value, stamp: Stamp) -> Entry {
    let when_ms = stamp.modified_ns / 1_000_000;
    entry(id, Source::Recording, Some(when_ms), summary, stamp)
}

fn entry(id: &str, source: Source, when_ms: Option<i64>, summary: &Value, stamp: Stamp) -> Entry {
    let text = |key: &str| summary[key].as_str().map(str::to_owned);
    let count = |key: &str| summary[key].as_array().map_or(0, |list| list.len() as i64);
    let multikills = summary["markers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|marker| is_multikill(&marker["label"]))
        .count() as i64;
    Entry {
        id: id.to_owned(),
        source,
        when_ms: when_ms.unwrap_or_default(),
        champion: text("champion"),
        queue: text("queue"),
        game_mode: text("gameMode"),
        result: text("result"),
        kills: summary["kda"]["kills"].as_i64(),
        deaths: summary["kda"]["deaths"].as_i64(),
        assists: summary["kda"]["assists"].as_i64(),
        duration_s: summary["durationS"].as_f64(),
        moments: count("moments"),
        multikills,
        clips: Vec::new(),
        video: false,
        thumbnail: None,
        stamp,
    }
}

fn label(value: &Value) -> Option<MarkerLabel> {
    serde_json::from_value(value.clone()).ok()
}

fn is_multikill(value: &Value) -> bool {
    matches!(
        label(value),
        Some(
            MarkerLabel::DoubleKill
                | MarkerLabel::TripleKill
                | MarkerLabel::QuadraKill
                | MarkerLabel::PentaKill
        )
    )
}

/// Where a match's thumbnail is taken (video seconds): the event its best highlight is
/// named after, else a third of the way into the video, else 10 s in.
fn thumbnail_moment(summary: &Value) -> f64 {
    let markers = summary["markers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let best = summary["moments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|moment| Some((moment, label(&moment["label"])?)))
        .max_by_key(|(_, label)| label.priority());
    let main = best.and_then(|(moment, label)| {
        let members: Vec<&Value> = moment["markerIds"]
            .as_array()?
            .iter()
            .filter_map(|id| markers.iter().find(|m| m["id"] == *id))
            .collect();
        let named = members
            .iter()
            .find(|m| self::label(&m["label"]) == Some(label))
            .or(members.first())?;
        named["videoTimeS"].as_f64()
    });
    let recording = &summary["recording"];
    let third = match (recording["frames"].as_f64(), recording["fps"].as_f64()) {
        (Some(frames), Some(fps)) if fps > 0.0 && frames > 0.0 => Some(frames / fps / 3.0),
        _ => None,
    };
    main.filter(|s| s.is_finite() && *s >= 0.0)
        .or(third)
        .unwrap_or(10.0)
}

fn unix_ns(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// `<folder>/<name>` inside a match folder, if `relative` is exactly that (one plain
/// name under `folder`: never a path out of it) and the file exists.
fn member_file(dir: &Path, folder: &str, relative: &str) -> Option<PathBuf> {
    let name = safe_name(relative.strip_prefix(folder)?.strip_prefix('/')?)?;
    Some(dir.join(folder).join(name)).filter(|path| path.is_file())
}

/// A clip's file and thumbnail, where they are there, as full paths for the player.
fn add_paths(dir: &Path, clip: &mut Map<String, Value>) {
    let file = clip.get("file").and_then(Value::as_str);
    if let Some(path) = file.and_then(|f| member_file(dir, CLIPS_DIR, f)) {
        clip.insert("path".into(), path.display().to_string().into());
    }
    let thumbnail = clip.get("thumbnail").and_then(Value::as_str);
    if let Some(path) = thumbnail.and_then(|f| member_file(dir, THUMBS_DIR, f)) {
        clip.insert("thumbnailPath".into(), path.display().to_string().into());
    }
}

/// The number after every clip a match lists, and every clip or thumbnail file in its
/// folder (a new clip never meets an old file).
fn next_clip_number(dir: &Path, summary: &Value) -> usize {
    let number = |name: &str| name.get(..4).and_then(|n| n.parse::<usize>().ok());
    let listed = summary["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|clip| clip["file"].as_str())
        .filter_map(|file| file.strip_prefix(CLIPS_DIR)?.strip_prefix('/'))
        .filter_map(number)
        .collect::<Vec<_>>();
    let on_disk = [CLIPS_DIR, THUMBS_DIR]
        .into_iter()
        .flat_map(|folder| read_dir(&dir.join(folder)))
        .filter_map(|path| path.file_name()?.to_str().and_then(number));
    listed.into_iter().chain(on_disk).max().unwrap_or(0) + 1
}

/// Writes `match.json` (or any JSON) whole or not at all: a temporary file, renamed over.
fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text + "\n")?;
    std::fs::rename(&temp, path)
}

/// The match's video, if `match.json` names one (a plain file name: never a path out of
/// the folder) and it exists.
fn video_path(dir: &Path, summary: &Value) -> Option<PathBuf> {
    let file = safe_name(summary["recording"]["file"].as_str()?)?;
    Some(dir.join(file)).filter(|path| path.is_file())
}

/// `<date>_<time>` of `started_wall_ms` (UTC), then each known part, slugged.
fn folder_name<const N: usize>(started_wall_ms: u64, parts: [Option<&str>; N]) -> String {
    let started = UNIX_EPOCH + Duration::from_millis(started_wall_ms);
    let (date, hour, minute) = utc_parts(started);
    let second = started_wall_ms / 1000 % 60;
    let mut name = format!("{date}_{hour:02}-{minute:02}-{second:02}");
    for part in parts.into_iter().flatten() {
        let part = slug(part);
        if !part.is_empty() {
            name.push('_');
            name.push_str(&part);
        }
    }
    name
}

/// Creates `<dir>/<base>`, or `<base>-2`, `-3`… if taken. Never reuses a folder.
fn create_unique_dir(dir: &Path, base: &str) -> io::Result<(String, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    for n in 1.. {
        let id = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}-{n}")
        };
        let path = dir.join(&id);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok((id, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("an unused folder name always exists")
}

/// A folder for a match that is starting to record: `<date>_<time>[_<queue>]` in the
/// matches folder (the champion isn't known yet; [`Library::save`] adds it).
pub fn reserve_folder(
    matches_dir: &Path,
    started_wall_ms: u64,
    queue: Option<&str>,
) -> io::Result<PathBuf> {
    create_unique_dir(matches_dir, &folder_name(started_wall_ms, [queue])).map(|(_, path)| path)
}

/// Renames a recorded match's folder to `base` (or `base-2`…). If Windows refuses (a
/// file in it still open), the folder keeps its recorded name; the id is whichever
/// name it ends up with.
fn adopt_folder(matches_dir: &Path, folder: &Path, base: &str) -> (String, PathBuf) {
    let current = folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    if current == base {
        return (current, folder.to_owned());
    }
    for attempt in 0..5 {
        let target = (1..)
            .map(|n| {
                if n == 1 {
                    base.to_owned()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|id| !matches_dir.join(id).exists())
            .expect("an unused folder name always exists");
        match std::fs::rename(folder, matches_dir.join(&target)) {
            Ok(()) => return (target.clone(), matches_dir.join(target)),
            Err(e) if attempt < 4 => {
                tracing::debug!(error = %e, "match folder busy; retrying the rename");
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                tracing::warn!(error = %e, folder = %current, "could not rename the match folder; keeping its name")
            }
        }
    }
    (current, folder.to_owned())
}

/// A file or folder name from the window: one plain path component, nothing else.
fn safe_name(name: &str) -> Option<&str> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && name.chars().all(|c| !c.is_control());
    plain.then_some(name)
}

/// `Ranked Solo/Duo` → `Ranked-Solo-Duo`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    // Windows tools like to add a byte-order mark.
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::replay;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("downplayed-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/lol")
    }

    fn summary(name: &str) -> MatchSummary {
        let fixture = Fixture::load(fixtures().join(format!("{name}.json"))).unwrap();
        replay(&fixture, &SessionConfig::default()).unwrap().summary
    }

    #[test]
    fn saved_matches_list_and_load_back() {
        let root = temp_dir("save");
        let library = Library::new(
            root.join("matches"),
            root.join("recordings"),
            SessionConfig::default(),
        );
        let mut win = summary("ranked_win");
        win.started_wall_ms = 1_792_080_720_000; // 2026-10-15 16:12:00 UTC
        let mut loss = summary("aram_loss");
        loss.started_wall_ms = win.started_wall_ms + 3_600_000;

        let id = library.save(&win).unwrap();
        assert_eq!(id, "2026-10-15_16-12-00_CLASSIC_Shyvana");
        assert_eq!(
            library.save(&win).unwrap(),
            format!("{id}-2"),
            "never overwrites"
        );
        let loss_id = library.save(&loss).unwrap();

        let cards = library.list();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].id, loss_id, "newest first");
        assert_eq!(cards[0].result.as_deref(), Some("lose"));
        let card = cards.iter().find(|c| c.id == id).unwrap();
        assert_eq!(card.source, Source::Game);
        assert_eq!(card.champion.as_deref(), Some("Shyvana"));
        assert_eq!(
            (card.kills, card.deaths, card.assists),
            (Some(4), Some(2), Some(2))
        );
        assert_eq!(card.moments, win.moments.len());

        let loaded = library.get(&id).unwrap();
        assert_eq!(loaded["schema"], MATCH_SCHEMA);
        assert_eq!(loaded["id"], id.as_str());
        assert_eq!(loaded["source"], "game");
        assert_eq!(
            loaded["markers"].as_array().unwrap().len(),
            win.markers.len()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recordings_are_replayed_with_their_league_client_files() {
        let root = temp_dir("recordings");
        let recordings = root.join("recordings");
        std::fs::create_dir_all(recordings.join("lcu")).unwrap();
        std::fs::copy(
            fixtures().join("ranked_win.json"),
            recordings.join("ranked_win.json"),
        )
        .unwrap();
        for suffix in ["session", "eog"] {
            std::fs::copy(
                fixtures().join(format!("lcu/ranked_win.{suffix}.json")),
                recordings.join(format!("lcu/ranked_win.{suffix}.json")),
            )
            .unwrap();
        }
        let library = Library::new(root.join("matches"), recordings, SessionConfig::default());
        let cards = library.list();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "recording:ranked_win");
        assert_eq!(cards[0].source, Source::Recording);
        assert_eq!(cards[0].queue.as_deref(), Some("Ranked Solo/Duo"));

        let review = library.get("recording:ranked_win").unwrap();
        assert_eq!(review["resultSource"], "end_of_game_stats");
        assert_eq!(review["source"], "recording");
        assert!(review["description"]
            .as_str()
            .unwrap()
            .contains("synthetic"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recording_folders_are_reserved_and_never_reused() {
        let root = temp_dir("reserve");
        let matches = root.join("matches");
        let started = 1_792_080_720_000; // 2026-10-15 16:12:00 UTC
        let name = |path: PathBuf| path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name(reserve_folder(&matches, started, Some("Ranked Solo/Duo")).unwrap()),
            "2026-10-15_16-12-00_Ranked-Solo-Duo"
        );
        assert_eq!(
            name(reserve_folder(&matches, started, Some("Ranked Solo/Duo")).unwrap()),
            "2026-10-15_16-12-00_Ranked-Solo-Duo-2"
        );
        assert_eq!(
            name(reserve_folder(&matches, started, None).unwrap()),
            "2026-10-15_16-12-00"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn video_paths_never_leave_the_match_folder() {
        let root = temp_dir("video");
        let matches = root.join("matches");
        std::fs::create_dir_all(matches.join("m1")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret").join("x.mp4"), b"").unwrap();
        std::fs::write(matches.join("m1").join(VIDEO_FILE), b"").unwrap();
        let library = Library::new(
            matches.clone(),
            root.join("recordings"),
            SessionConfig::default(),
        );
        for (file, plays) in [
            ("match.mp4", true),
            (r"..\..\secret\x.mp4", false),
            ("../../secret/x.mp4", false),
            (r"C:\secret\x.mp4", false),
            ("missing.mp4", false),
        ] {
            let json = serde_json::json!({ "recording": { "file": file } }).to_string();
            std::fs::write(matches.join("m1").join(MATCH_FILE), json).unwrap();
            library.sync();
            let review = library.get("m1").unwrap();
            assert_eq!(review.get("videoPath").is_some(), plays, "{file}");
            assert_eq!(library.list()[0].video, plays, "{file}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clips_are_listed_with_their_files_and_can_be_favourited() {
        let root = temp_dir("clips");
        let matches = root.join("matches");
        let dir = matches.join("m1");
        std::fs::create_dir_all(dir.join(CLIPS_DIR)).unwrap();
        std::fs::create_dir_all(dir.join(THUMBS_DIR)).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret").join("x.mp4"), b"").unwrap();
        std::fs::write(dir.join(CLIPS_DIR).join("0001_kill.mp4"), b"").unwrap();
        std::fs::write(dir.join(THUMBS_DIR).join("0001.jpg"), b"").unwrap();
        let json = serde_json::json!({ "clips": [
            { "file": "clips/0001_kill.mp4", "fromS": 88, "toS": 108, "label": "kill", "thumbnail": "thumbs/0001.jpg" },
            { "file": "clips/../../secret/x.mp4", "fromS": 0, "toS": 1, "label": "x" },
            { "file": "clips/0002_missing.mp4", "fromS": 0, "toS": 1, "label": "y" }
        ]});
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        let library = Library::new(matches, root.join("recordings"), SessionConfig::default());

        let review = library.get("m1").unwrap();
        let clips = review["clips"].as_array().unwrap();
        let clip = dir.join(CLIPS_DIR).join("0001_kill.mp4");
        assert_eq!(clips[0]["path"], clip.display().to_string());
        assert_eq!(
            clips[0]["thumbnailPath"],
            dir.join(THUMBS_DIR).join("0001.jpg").display().to_string()
        );
        assert!(clips[1].get("path").is_none(), "never outside clips/");
        assert!(clips[2].get("path").is_none(), "the file is missing");

        assert!(library
            .set_clip_favorite("m1", "clips/0001_kill.mp4", true)
            .unwrap());
        assert_eq!(library.get("m1").unwrap()["clips"][0]["favorite"], true);
        assert!(!library
            .set_clip_favorite("m1", "clips/none.mp4", true)
            .unwrap());
        assert!(library
            .set_clip_favorite("../m1", "clips/0001_kill.mp4", true)
            .is_err());

        assert_eq!(library.clip_file("m1", "clips/0001_kill.mp4"), Some(clip));
        assert_eq!(library.clip_file("m1", "clips/../../secret/x.mp4"), None);
        assert_eq!(library.clip_file("m1", "clips/unlisted.mp4"), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clips_made_in_the_review_are_numbered_after_the_rest_and_listed() {
        let root = temp_dir("addclip");
        let matches = root.join("matches");
        let dir = matches.join("m1");
        std::fs::create_dir_all(dir.join(CLIPS_DIR)).unwrap();
        std::fs::write(dir.join(VIDEO_FILE), b"video").unwrap();
        // Clip 1 is listed; clip 3's file was left behind by something else.
        std::fs::write(dir.join(CLIPS_DIR).join("0003_old.mp4"), b"").unwrap();
        let ten_minutes = serde_json::json!({ "file": "match.mp4", "frames": 36_000, "fps": 60 });
        let json = serde_json::json!({
            "recording": ten_minutes,
            "clips": [{ "file": "clips/0001_kill.mp4", "fromS": 88, "toS": 108, "label": "kill" }]
        });
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        let library = Library::new(
            matches.clone(),
            root.join("recordings"),
            SessionConfig::default(),
        );

        let job = library
            .plan_clip("m1", (100.4, 125.0), "Double kill", Some(112.0))
            .unwrap();
        assert_eq!(
            (job.video.as_path(), job.folder.as_path()),
            (dir.join(VIDEO_FILE).as_path(), dir.as_path())
        );
        assert_eq!(job.plan.file, "clips/0004_double_kill.mp4");
        assert_eq!(
            (job.plan.from_s, job.plan.to_s, job.plan.thumbnail_at_s),
            (100.0, 125.0, 112.0),
            "from the keyframe before"
        );
        // Kept within the ten minutes recorded; the thumbnail in the middle by default.
        let late = library.plan_clip("m1", (590.0, 640.0), "", None).unwrap();
        assert_eq!((late.plan.to_s, late.plan.label.as_str()), (600.0, "clip"));
        assert!((late.plan.thumbnail_at_s - 599.95).abs() < 1e-9);
        let outside = library
            .plan_clip("m1", (700.0, 720.0), "kill", None)
            .unwrap_err();
        assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);
        assert!(library
            .plan_clip("../m1", (1.0, 5.0), "kill", None)
            .is_err());

        std::fs::write(dir.join(&job.plan.file), b"clip").unwrap();
        let shown = library.add_clip("m1", &job.plan.clip(false)).unwrap();
        assert_eq!(shown["file"], "clips/0004_double_kill.mp4");
        assert_eq!(
            shown["path"],
            dir.join(CLIPS_DIR)
                .join("0004_double_kill.mp4")
                .display()
                .to_string()
        );
        assert!(shown.get("thumbnailPath").is_none());
        let review = library.get("m1").unwrap();
        assert_eq!(review["clips"].as_array().unwrap().len(), 2);
        assert_eq!(review["clips"][1]["fromS"], 100.0);
        assert_eq!(review["recording"], ten_minutes, "the rest is kept");

        // A match without clips yet, and one without its video.
        let fresh = matches.join("m2");
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join(VIDEO_FILE), b"video").unwrap();
        let json =
            serde_json::json!({ "recording": { "file": "match.mp4", "frames": 600, "fps": 60 } });
        std::fs::write(fresh.join(MATCH_FILE), json.to_string()).unwrap();
        let first = library.plan_clip("m2", (1.0, 9.0), "assist", None).unwrap();
        assert_eq!(first.plan.file, "clips/0001_assist.mp4");
        library.add_clip("m2", &first.plan.clip(false)).unwrap();
        assert_eq!(
            library.get("m2").unwrap()["clips"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_file(fresh.join(VIDEO_FILE)).unwrap();
        let gone = library
            .plan_clip("m2", (1.0, 9.0), "assist", None)
            .unwrap_err();
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ids_cannot_leave_the_library() {
        let root = temp_dir("ids");
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret").join(MATCH_FILE), "{}").unwrap();
        let library = Library::new(
            root.join("matches"),
            root.join("recordings"),
            SessionConfig::default(),
        );
        for id in [
            "../secret",
            "..\\secret",
            "recording:../x",
            "recording:..\\x",
            "C:\\x",
            "..",
            "",
        ] {
            assert!(library.get(id).is_none(), "{id:?}");
        }
        assert_eq!(slug("Ranked Solo/Duo"), "Ranked-Solo-Duo");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A match folder with a `match.json` like the tracker writes, trimmed to what the
    /// index reads.
    #[allow(clippy::too_many_arguments)]
    fn write_match(
        matches: &Path,
        id: &str,
        when_ms: i64,
        champion: &str,
        queue: Option<&str>,
        result: Option<&str>,
        kills: i64,
        duration_s: f64,
        labels: &[&str],
        clips: usize,
        video: bool,
    ) {
        let dir = matches.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let markers: Vec<Value> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let at = 100.0 * (i + 1) as f64;
                serde_json::json!({ "id": i + 1, "label": label, "gameTimeS": at,
                    "videoTimeS": at - 40.0, "preS": 12, "postS": 8 })
            })
            .collect();
        let moments: Vec<Value> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| serde_json::json!({ "label": label, "markerIds": [i + 1] }))
            .collect();
        let clips: Vec<Value> = (1..=clips)
            .map(|n| {
                serde_json::json!({ "file": format!("clips/{n:04}_kill.mp4"), "fromS": 10,
                    "toS": 30, "label": "kill", "favorite": n == 1 })
            })
            .collect();
        let mut json = serde_json::json!({
            "schema": 1, "id": id, "startedWallMs": when_ms, "champion": champion,
            "gameMode": "CLASSIC", "kda": { "kills": kills, "deaths": 2, "assists": 3 },
            "durationS": duration_s, "markers": markers, "moments": moments, "clips": clips,
            "recording": { "file": "match.mp4", "frames": (duration_s * 60.0) as i64, "fps": 60 }
        });
        if let Some(queue) = queue {
            json["queue"] = queue.into();
        }
        if let Some(result) = result {
            json["result"] = result.into();
        }
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        if video {
            std::fs::write(dir.join(VIDEO_FILE), b"video").unwrap();
        }
    }

    fn ids(page: &MatchPage) -> Vec<&str> {
        page.items.iter().map(|card| card.id.as_str()).collect()
    }

    fn card_ids(cards: &[MatchCard]) -> Vec<&str> {
        cards.iter().map(|card| card.id.as_str()).collect()
    }

    #[test]
    fn the_index_is_built_from_the_sidecars_and_kept_in_step_with_them() {
        let root = temp_dir("index");
        let matches = root.join("matches");
        let day = 86_400_000;
        let base = 1_792_080_720_000;
        let ranked = Some("Ranked Solo/Duo");
        let multikill = ["kill", "double_kill"];
        write_match(
            &matches,
            "a",
            base,
            "Shyvana",
            ranked,
            Some("win"),
            7,
            1800.0,
            &multikill,
            2,
            true,
        );
        write_match(
            &matches,
            "b",
            base + day,
            "Ahri",
            None,
            Some("lose"),
            2,
            1500.0,
            &["death"],
            0,
            false,
        );
        write_match(
            &matches,
            "c",
            base + 2 * day,
            "Jinx",
            Some("ARAM"),
            None,
            12,
            1100.0,
            &[],
            0,
            true,
        );
        std::fs::create_dir_all(matches.join("recording-in-progress")).unwrap();
        let open = || {
            Library::new(
                matches.clone(),
                root.join("recordings"),
                SessionConfig::default(),
            )
        };
        let library = open();
        assert_eq!(library.index_opened(), Opened::New);
        assert_eq!(library.index_path(), Some(root.join(INDEX_FILE).as_path()));

        let first = library.sync();
        assert_eq!(
            (first.read, first.removed, first.total),
            (3, 0, 3),
            "a folder without match.json isn't a match"
        );
        let cards = library.list();
        assert_eq!(card_ids(&cards), ["c", "b", "a"], "newest first");
        let a = &cards[2];
        assert_eq!(
            (a.multikills, a.clips, a.favorite_clips, a.video),
            (1, 2, 1, true)
        );
        assert_eq!(
            (a.kills, a.moments, a.queue.as_deref()),
            (Some(7), 2, ranked)
        );
        assert_eq!(
            (cards[1].video, cards[1].result.as_deref()),
            (false, Some("lose"))
        );
        assert_eq!(library.sync().read, 0, "nothing changed, nothing read");

        // Changed, removed and added outside the app: the next sync follows.
        write_match(
            &matches,
            "b",
            base + day,
            "Ahri",
            None,
            Some("win"),
            9,
            1500.0,
            &["death"],
            0,
            false,
        );
        std::fs::remove_dir_all(matches.join("c")).unwrap();
        write_match(
            &matches,
            "d",
            base + 3 * day,
            "Lux",
            None,
            Some("win"),
            1,
            900.0,
            &[],
            0,
            false,
        );
        let again = library.sync();
        assert_eq!((again.read, again.removed, again.total), (2, 1, 3));
        let cards = library.list();
        assert_eq!(card_ids(&cards), ["d", "b", "a"]);
        assert_eq!(
            (cards[1].result.as_deref(), cards[1].kills),
            (Some("win"), Some(9))
        );
        // A video that appears counts as a change too.
        std::fs::write(matches.join("b").join(VIDEO_FILE), b"video").unwrap();
        assert_eq!(library.sync().read, 1);
        assert!(library.list()[1].video);
        drop(library);

        // The next start keeps the index and reads nothing.
        let library = open();
        assert_eq!(library.index_opened(), Opened::Existing);
        assert_eq!(library.sync().read, 0);
        assert_eq!(library.list().len(), 3);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_damaged_or_outdated_index_is_rebuilt_from_the_sidecars() {
        let root = temp_dir("damaged");
        let matches = root.join("matches");
        for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
            let when = 1_792_080_720_000 + i as i64;
            write_match(
                &matches,
                id,
                when,
                "Shyvana",
                None,
                Some("win"),
                1,
                600.0,
                &[],
                0,
                false,
            );
        }
        let open = || {
            Library::new(
                matches.clone(),
                root.join("recordings"),
                SessionConfig::default(),
            )
        };
        assert_eq!(open().list().len(), 3);

        // Garbage where the index was.
        std::fs::write(
            root.join(INDEX_FILE),
            b"not a SQLite database, not even close",
        )
        .unwrap();
        let library = open();
        assert_eq!(library.index_opened(), Opened::New);
        assert_eq!(library.sync().read, 3);
        assert_eq!(library.list().len(), 3);
        drop(library);

        // An index from another schema.
        let conn = rusqlite::Connection::open(root.join(INDEX_FILE)).unwrap();
        conn.execute("UPDATE meta SET value = '99' WHERE key = 'schema'", [])
            .unwrap();
        drop(conn);
        let library = open();
        assert_eq!(library.index_opened(), Opened::New);
        assert_eq!(library.list().len(), 3);
        drop(library);

        // A truncated file.
        let bytes = std::fs::read(root.join(INDEX_FILE)).unwrap();
        std::fs::write(root.join(INDEX_FILE), &bytes[..bytes.len() / 3]).unwrap();
        let library = open();
        assert_ne!(library.index_opened(), Opened::Existing);
        assert_eq!(library.list().len(), 3);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_list_pages_filters_and_sorts() {
        let root = temp_dir("query");
        let matches = root.join("matches");
        let base = 1_792_080_720_000_i64;
        let hour = 3_600_000;
        let champions = ["Ahri", "Jinx", "Shyvana", "Lux", "Garen"];
        for i in 0..25_i64 {
            let result = match i % 3 {
                0 => Some("win"),
                1 => Some("lose"),
                _ => None,
            };
            let queue = (i % 2 == 0).then_some("Ranked Solo/Duo");
            let labels: &[&str] = if i % 5 == 0 {
                &["kill", "triple_kill"]
            } else {
                &["kill"]
            };
            let length = 600.0 + (i * 37 % 25) as f64 * 60.0;
            let clips = usize::from(i % 4 == 0);
            let champion = champions[(i % 5) as usize];
            let id = format!("m{i:02}");
            write_match(
                &matches,
                &id,
                base + i * hour,
                champion,
                queue,
                result,
                i,
                length,
                labels,
                clips,
                false,
            );
        }
        let library = Library::new(matches, root.join("recordings"), SessionConfig::default());
        let page = |query: MatchQuery| library.query(&query).unwrap();
        let all = MatchQuery::default;

        let first = page(MatchQuery { limit: 10, ..all() });
        assert_eq!((first.total, first.items.len(), first.offset), (25, 10, 0));
        assert_eq!(first.items[0].id, "m24", "newest first");
        let last = page(MatchQuery {
            offset: 20,
            limit: 10,
            ..all()
        });
        assert_eq!(ids(&last), ["m04", "m03", "m02", "m01", "m00"]);
        assert_eq!(
            page(all()).items.len(),
            25,
            "the default page holds them all"
        );

        let jinx = page(MatchQuery {
            champion: Some("Jinx".into()),
            ..all()
        });
        assert_eq!(ids(&jinx), ["m21", "m16", "m11", "m06", "m01"]);
        let ranked = page(MatchQuery {
            queue: Some("Ranked Solo/Duo".into()),
            ..all()
        });
        assert_eq!(ranked.total, 13);
        let classic = page(MatchQuery {
            queue: Some("CLASSIC".into()),
            ..all()
        });
        assert_eq!(
            classic.total, 12,
            "the game mode stands in for an unknown queue"
        );
        let wins = page(MatchQuery {
            result: Some("win".into()),
            ..all()
        });
        assert_eq!(wins.total, 9);
        let unknown = page(MatchQuery {
            result: Some("unknown".into()),
            ..all()
        });
        assert_eq!(unknown.total, 8);
        let clipped = page(MatchQuery {
            has_clips: true,
            ..all()
        });
        assert_eq!(
            ids(&clipped),
            ["m24", "m20", "m16", "m12", "m08", "m04", "m00"]
        );
        let multikills = page(MatchQuery {
            has_multikill: true,
            ..all()
        });
        assert_eq!(ids(&multikills), ["m20", "m15", "m10", "m05", "m00"]);
        let window = page(MatchQuery {
            from_ms: Some(base + 10 * hour),
            to_ms: Some(base + 13 * hour),
            ..all()
        });
        assert_eq!(ids(&window), ["m12", "m11", "m10"]);
        let combined = page(MatchQuery {
            champion: Some("Ahri".into()),
            result: Some("win".into()),
            has_multikill: true,
            ..all()
        });
        assert_eq!(ids(&combined), ["m15", "m00"]);

        let oldest = page(MatchQuery {
            sort: Sort::Oldest,
            limit: 2,
            ..all()
        });
        assert_eq!(oldest.items[0].id, "m00");
        let kills = page(MatchQuery {
            sort: Sort::MostKills,
            limit: 1,
            ..all()
        });
        assert_eq!(kills.items[0].id, "m24");
        let longest = page(MatchQuery {
            sort: Sort::Longest,
            ..all()
        });
        let lengths: Vec<f64> = longest
            .items
            .iter()
            .map(|c| c.duration_s.unwrap())
            .collect();
        assert!(lengths.windows(2).all(|w| w[0] >= w[1]), "{lengths:?}");
        // Text from the window is only ever a value, never SQL.
        let odd = page(MatchQuery {
            champion: Some("x' OR '1'='1".into()),
            ..all()
        });
        assert_eq!(odd.total, 0);

        let facets = library.facets().unwrap();
        assert_eq!(
            facets.champions,
            ["Ahri", "Garen", "Jinx", "Lux", "Shyvana"]
        );
        assert_eq!(facets.queues, ["CLASSIC", "Ranked Solo/Duo"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn removing_a_clip_recycles_its_files_and_updates_the_match_and_the_index() {
        let root = temp_dir("remove");
        let matches = root.join("matches");
        write_match(
            &matches,
            "m1",
            1_792_080_720_000,
            "Shyvana",
            None,
            Some("win"),
            3,
            900.0,
            &["kill"],
            2,
            true,
        );
        let dir = matches.join("m1");
        std::fs::create_dir_all(dir.join(CLIPS_DIR)).unwrap();
        std::fs::create_dir_all(dir.join(THUMBS_DIR)).unwrap();
        for n in [1, 2] {
            std::fs::write(dir.join(format!("clips/{n:04}_kill.mp4")), b"clip").unwrap();
        }
        // Clip 1 has a thumbnail.
        let mut json = read_json(&dir.join(MATCH_FILE)).unwrap();
        json["clips"][0]["thumbnail"] = "thumbs/0001.jpg".into();
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        std::fs::write(dir.join("thumbs/0001.jpg"), b"jpeg").unwrap();
        let library = Library::new(matches, root.join("recordings"), SessionConfig::default());
        assert_eq!(library.list()[0].clips, 2);
        let first = "clips/0001_kill.mp4";

        // Declined (the recycling was cancelled) or failed: nothing changes.
        let declined = library.remove_clip("m1", first, |_| Ok(false)).unwrap();
        assert_eq!(declined, Removal::Kept);
        let failed = library.remove_clip("m1", first, |_| Err("the shell said no".into()));
        assert_eq!(failed, Err("the shell said no".to_owned()));
        assert_eq!(
            library.get("m1").unwrap()["clips"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(dir.join(first).is_file());

        // Recycled: both files go to the bin, the clip leaves the match and the index.
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let mut asked = Vec::new();
        let removed = library
            .remove_clip("m1", first, |files| {
                for file in files {
                    asked.push(file.clone());
                    std::fs::rename(file, bin.join(file.file_name().unwrap())).unwrap();
                }
                Ok(true)
            })
            .unwrap();
        assert_eq!(removed, Removal::Removed);
        let expected = [
            dir.join("clips").join("0001_kill.mp4"),
            dir.join("thumbs").join("0001.jpg"),
        ];
        assert_eq!(asked, expected);
        assert!(bin.join("0001_kill.mp4").is_file() && bin.join("0001.jpg").is_file());
        let clips = library.get("m1").unwrap()["clips"].clone();
        assert_eq!(clips.as_array().unwrap().len(), 1);
        assert_eq!(clips[0]["file"], "clips/0002_kill.mp4");
        let card = &library.list()[0];
        assert_eq!((card.clips, card.favorite_clips), (1, 0));

        // A clip whose file is already gone just leaves the list: nothing to recycle.
        std::fs::remove_file(dir.join("clips/0002_kill.mp4")).unwrap();
        let gone = library
            .remove_clip("m1", "clips/0002_kill.mp4", |_| {
                panic!("nothing to recycle")
            })
            .unwrap();
        assert_eq!(gone, Removal::Removed);
        assert_eq!(library.list()[0].clips, 0);
        let again = library.remove_clip("m1", "clips/0002_kill.mp4", |_| Ok(true));
        assert!(again.is_err(), "not listed any more");
        assert!(library.remove_clip("../m1", first, |_| Ok(true)).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_match_thumbnail_shows_its_best_highlight() {
        let root = temp_dir("thumbnail");
        let matches = root.join("matches");
        let when = 1_792_080_720_000;
        let labels = ["kill", "triple_kill", "death"];
        write_match(
            &matches,
            "m1",
            when,
            "Shyvana",
            None,
            Some("win"),
            3,
            900.0,
            &labels,
            0,
            true,
        );
        write_match(
            &matches,
            "m2",
            when + 1,
            "Ahri",
            None,
            Some("win"),
            3,
            900.0,
            &[],
            0,
            true,
        );
        write_match(
            &matches,
            "m3",
            when + 2,
            "Lux",
            None,
            Some("win"),
            3,
            900.0,
            &["kill"],
            0,
            false,
        );
        let library = Library::new(
            matches.clone(),
            root.join("recordings"),
            SessionConfig::default(),
        );
        let job = library.thumbnail_job("m1").unwrap();
        assert_eq!(job.video, matches.join("m1").join(VIDEO_FILE));
        assert_eq!(job.out, matches.join("m1").join("thumbs").join("match.jpg"));
        assert_eq!(
            job.at_s, 160.0,
            "the triple kill (marker 2), not the kill or the death"
        );
        assert_eq!(
            library.thumbnail_job("m2").unwrap().at_s,
            300.0,
            "a third of the way in"
        );
        assert!(library.thumbnail_job("m3").is_none(), "no video");

        assert!(library
            .list()
            .iter()
            .all(|card| card.thumbnail_path.is_none()));
        assert_eq!(library.set_match_thumbnail("m1"), None, "not made yet");
        std::fs::create_dir_all(job.out.parent().unwrap()).unwrap();
        std::fs::write(&job.out, b"jpeg").unwrap();
        assert_eq!(library.set_match_thumbnail("m1"), Some(job.out.clone()));
        let card = library.list().into_iter().find(|c| c.id == "m1").unwrap();
        assert_eq!(card.thumbnail_path, Some(job.out.display().to_string()));
        assert!(library.thumbnail_job("m1").is_none(), "made already");
        std::fs::remove_dir_all(&root).ok();
    }
}
