//! The Outplayed importer (doc 03 §7). An Outplayed library keeps League videos as
//! `<root>\League of Legends\League of Legends_<M-D-YYYY>_<h-m-s-ms>\*.mp4`; each becomes a
//! match in RiftReel's library *by reference*: a folder with a `match.json` that points
//! at the video where it is. Nothing under the Outplayed folder is ever written, moved or
//! deleted, and the cleanup leaves an import's videos alone unless the user allows it for
//! that import.
//!
//! A video's time comes from its name (the PC's local time), its length from the MP4 and
//! its size from the file. When Outplayed's localStorage backup is there
//! (`<root>\temp\outplayed.json`), its `MATCH_<sessionId>_<n>` records add the champion,
//! K/D/A, a win and markers from Outplayed's events, joined by file name. Every other key
//! in that file is skipped unread. A loss is never inferred: without a win the result
//! stays unknown. Importing again adds only the videos not imported yet.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use regex::Regex;
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    create_unique_dir, folder_name, read_dir, read_json, write_json, Library, MATCH_FILE,
    MATCH_SCHEMA, OUTPLAYED_SOURCE,
};
use crate::tracker::moments::{build_moments, DEFAULT_MAX_MOMENT_S};
use crate::tracker::normalize::{Marker, MarkerKind, MarkerLabel};

/// The folder of League videos in an Outplayed library.
pub const LEAGUE_DIR: &str = "League of Legends";
/// The imports so far, in the library's root.
pub const IMPORTS_FILE: &str = "imports.json";
const IMPORTS_SCHEMA: u32 = 1;
/// Outplayed's game id for League of Legends.
const LEAGUE_GAME_ID: i64 = 5426;
/// The longest pre- or post-roll a marker keeps (Outplayed's sliders stop at 180 s).
const MAX_WINDOW_S: f64 = 180.0;
const DAY_MS: i64 = 86_400_000;

/// Outplayed's localStorage backup in its library.
pub fn export_file(root: &Path) -> PathBuf {
    root.join("temp").join("outplayed.json")
}

/// A wall-clock time as Outplayed writes it in names: the PC's local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub ms: u32,
}

impl LocalTime {
    /// Read as UTC, in Unix ms.
    fn as_utc_ms(self) -> i64 {
        let seconds = i64::from(self.hour * 3600 + self.minute * 60 + self.second);
        days_from_civil(self.year, self.month, self.day) * DAY_MS
            + seconds * 1000
            + i64::from(self.ms)
    }
}

/// The time at the end of an Outplayed folder or file name,
/// `…_<M>-<D>-<YYYY>_<h>-<m>-<s>-<ms>` (an `.mp4` extension is fine).
pub fn name_time(name: &str) -> Option<LocalTime> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)_(\d{1,2})-(\d{1,2})-(\d{4})_(\d{1,2})-(\d{1,2})-(\d{1,2})-(\d{1,3})(?:\.mp4)?$",
        )
        .expect("valid regex")
    });
    let found = PATTERN.captures(name)?;
    let number = |i: usize| found[i].parse::<u32>().ok();
    let time = LocalTime {
        month: number(1)?,
        day: number(2)?,
        year: found[3].parse().ok()?,
        hour: number(4)?,
        minute: number(5)?,
        second: number(6)?,
        ms: number(7)?,
    };
    let valid = (1..=12).contains(&time.month)
        && (1..=days_in_month(time.year, time.month)).contains(&time.day)
        && time.hour < 24
        && time.minute < 60
        && time.second < 60;
    valid.then_some(time)
}

/// `time` on this PC's clock as Unix ms, by its time zone's rules (daylight saving
/// included). Elsewhere than Windows, read as UTC.
pub fn local_to_utc_ms(time: LocalTime) -> Option<i64> {
    imp::local_to_utc_ms(time)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 (Howard Hinnant's days_from_civil).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// An MP4's length in seconds: `moov`/`mvhd`, or `mvex`/`mehd` when a fragmented file's
/// `mvhd` has none. Only box headers are read; the media is skipped over.
pub fn mp4_duration(path: &Path) -> Option<f64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    movie_duration(&mut file, len)
}

fn movie_duration<R: Read + Seek>(file: &mut R, len: u64) -> Option<f64> {
    let (moov, moov_end) = find_box(file, 0, len, b"moov")?;
    let (mvhd, _) = find_box(file, moov, moov_end, b"mvhd")?;
    file.seek(SeekFrom::Start(mvhd)).ok()?;
    let version = read_array::<4, _>(file)?[0];
    let (timescale, mut duration) = if version == 1 {
        read_array::<16, _>(file)?;
        (read_u32(file)?, u64::from_be_bytes(read_array(file)?))
    } else {
        read_array::<8, _>(file)?;
        (read_u32(file)?, u64::from(read_u32(file)?))
    };
    // All ones means "unknown"; a fragmented file says it in `mehd`.
    if duration == 0 || duration == u64::MAX || duration == u64::from(u32::MAX) {
        let (mvex, mvex_end) = find_box(file, moov, moov_end, b"mvex")?;
        let (mehd, _) = find_box(file, mvex, mvex_end, b"mehd")?;
        file.seek(SeekFrom::Start(mehd)).ok()?;
        duration = if read_array::<4, _>(file)?[0] == 1 {
            u64::from_be_bytes(read_array(file)?)
        } else {
            u64::from(read_u32(file)?)
        };
    }
    let seconds = duration as f64 / f64::from(timescale);
    (timescale > 0 && seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// The body (start, end) of the first `kind` box in `[from, to)`.
fn find_box<R: Read + Seek>(
    file: &mut R,
    from: u64,
    to: u64,
    kind: &[u8; 4],
) -> Option<(u64, u64)> {
    let mut at = from;
    // A real file has a handful of top-level boxes; this only stops a broken one.
    for _ in 0..4096 {
        if at.checked_add(8)? > to {
            return None;
        }
        file.seek(SeekFrom::Start(at)).ok()?;
        let header = read_array::<8, _>(file)?;
        let (size, body) = match u32::from_be_bytes(header[..4].try_into().ok()?) {
            0 => (to - at, at + 8),
            1 => (u64::from_be_bytes(read_array(file)?), at + 16),
            size => (u64::from(size), at + 8),
        };
        if size < body - at || at.checked_add(size)? > to {
            return None;
        }
        if &header[4..] == kind {
            return Some((body, at + size));
        }
        at += size;
    }
    None
}

fn read_array<const N: usize, R: Read>(file: &mut R) -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

fn read_u32<R: Read>(file: &mut R) -> Option<u32> {
    read_array(file).map(u32::from_be_bytes)
}

/// What Outplayed recorded about one video, from its `MATCH_` record.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub champion: Option<String>,
    pub game_mode: Option<String>,
    /// Kills, deaths, assists.
    pub kda: Option<[u64; 3]>,
    /// `isVictory`, or a victory event. `false` means unknown, never a loss.
    pub won: bool,
    /// `fullMatch`, `replay` (a highlight) or `fullSession`.
    pub media: String,
    /// Seconds into this video.
    pub markers: Vec<Marker>,
}

/// The League records of a localStorage backup.
#[derive(Debug, Default)]
pub struct Export {
    pub records: usize,
    /// By video file name, lowercase.
    pub videos: HashMap<String, Recorded>,
}

/// Reads the `MATCH_<sessionId>_<n>` records of the localStorage backup at `path`.
pub fn read_export(path: &Path) -> io::Result<Export> {
    let text = std::fs::read_to_string(path)?;
    let values: MatchValues =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(io::Error::other)?;
    let mut export = Export::default();
    for record in values.0.into_iter().filter_map(decode) {
        if record["gameId"].as_i64() != Some(LEAGUE_GAME_ID) {
            continue;
        }
        export.records += 1;
        for (name, recorded) in videos_of(&record) {
            export.videos.insert(name, recorded);
        }
    }
    Ok(export)
}

/// The values of a backup's `MATCH_<sessionId>_<n>` keys. Every other key's value is
/// skipped without being kept: ad, analytics and account keys never leave the parser.
struct MatchValues(Vec<Value>);

impl<'de> Deserialize<'de> for MatchValues {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys;

        impl<'de> Visitor<'de> for Keys {
            type Value = MatchValues;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a localStorage backup (an object of keys)")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MatchValues, A::Error> {
                let mut values = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if is_match_key(&key) {
                        values.push(map.next_value::<Value>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(MatchValues(values))
            }
        }

        deserializer.deserialize_map(Keys)
    }
}

/// `MATCH_<sessionId>_<n>`, the session a run of hex digits (not `MATCH_ACTIONS_…` and
/// the like).
fn is_match_key(key: &str) -> bool {
    let Some((session, n)) = key
        .strip_prefix("MATCH_")
        .and_then(|rest| rest.rsplit_once('_'))
    else {
        return false;
    };
    (16..=64).contains(&session.len())
        && session.bytes().all(|b| b.is_ascii_hexdigit())
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
}

/// A record: JSON in a string, sometimes encoded twice.
fn decode(mut value: Value) -> Option<Value> {
    for _ in 0..3 {
        match value {
            Value::String(text) => value = serde_json::from_str(&text).ok()?,
            Value::Object(_) => return Some(value),
            _ => return None,
        }
    }
    value.is_object().then_some(value)
}

/// Each video of a League record, by file name (lowercase).
fn videos_of(record: &Value) -> Vec<(String, Recorded)> {
    let info = &record["info"];
    let medias = record["medias"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let victory_event = medias
        .iter()
        .flat_map(|media| media["events"].as_array().into_iter().flatten())
        .any(|event| event["type"] == "victory");
    let kda = [
        &info["killCount"],
        &info["deathCount"],
        &info["assistCount"],
    ]
    .map(count);
    let kda = match kda {
        [Some(kills), Some(deaths), Some(assists)] => Some([kills, deaths, assists]),
        _ => None,
    };
    medias
        .iter()
        .filter_map(|media| {
            let path = media["path"].as_str()?;
            let name = path.rsplit(['\\', '/']).next()?.to_lowercase();
            (!name.is_empty()).then(|| {
                let recorded = Recorded {
                    champion: info["selectedChampion"]
                        .as_str()
                        .filter(|c| !c.is_empty())
                        .map(str::to_owned),
                    game_mode: match info["gameMode"].as_str() {
                        Some("arena") => Some("CHERRY".into()),
                        Some("tft") => Some("TFT".into()),
                        // "lol" says no more than League.
                        _ => None,
                    },
                    kda,
                    won: info["isVictory"] == true || victory_event,
                    media: media["type"].as_str().unwrap_or_default().to_owned(),
                    markers: markers(
                        media["events"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or_default(),
                    ),
                };
                (name, recorded)
            })
        })
        .collect()
}

/// A count from a record: a number, or a number in a string.
fn count(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Markers from a video's events: kills (a multikill by its label), assists, deaths and
/// the victory, at `time` ms into the video with Outplayed's `timing` as the pre- and
/// post-roll. Other events (ultimates, arena rounds…) have no marker in RiftReel.
fn markers(events: &[Value]) -> Vec<Marker> {
    let mut markers: Vec<Marker> = events
        .iter()
        .filter_map(|event| {
            let (kind, label) = match event["type"].as_str()? {
                "kill" => (MarkerKind::Kill, kill_label(&event["data"])),
                "assist" => (MarkerKind::Assist, MarkerLabel::Assist),
                "death" => (MarkerKind::Death, MarkerLabel::Death),
                "victory" => (MarkerKind::Victory, MarkerLabel::Victory),
                _ => return None,
            };
            let at = event["time"].as_f64()? / 1000.0;
            if !at.is_finite() || at < 0.0 {
                return None;
            }
            let window = kind.default_window();
            let seconds = |value: &Value, default: f64| {
                value
                    .as_f64()
                    .map(|ms| ms / 1000.0)
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .map_or(default, |s| s.min(MAX_WINDOW_S))
            };
            Some(Marker {
                id: 0,
                kind,
                label,
                game_time_s: at,
                video_time_s: Some(at),
                pre_s: seconds(&event["timing"]["past"], window.pre_s),
                post_s: seconds(&event["timing"]["future"], window.post_s),
                stolen: false,
                by_team: false,
                riot_event_ids: Vec::new(),
            })
        })
        .collect();
    markers.sort_by(|a, b| a.game_time_s.total_cmp(&b.game_time_s));
    for (n, marker) in markers.iter_mut().enumerate() {
        marker.id = n as u32 + 1;
    }
    markers
}

/// A kill's label from its `data` (JSON, often in a string): `double_kill`, … or a kill.
fn kill_label(data: &Value) -> MarkerLabel {
    let data = match data {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    };
    match serde_json::from_value(data["label"].clone()) {
        Ok(
            label @ (MarkerLabel::DoubleKill
            | MarkerLabel::TripleKill
            | MarkerLabel::QuadraKill
            | MarkerLabel::PentaKill),
        ) => label,
        _ => MarkerLabel::Kill,
    }
}

/// A League video in an Outplayed library.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    /// Its path under the Outplayed root (`League of Legends\<folder>\<file>`), what a
    /// re-import knows it by.
    pub key: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// When it started (Unix ms): the time in its name, else its folder's, else the
    /// file's modification time.
    pub started_ms: i64,
    /// Neither name had a time.
    pub undated: bool,
}

/// The League videos of the Outplayed library at `root`: the `*.mp4` files one level down
/// in its `League of Legends` folder, oldest first.
pub fn find_videos(root: &Path) -> io::Result<Vec<Video>> {
    let league = root.join(LEAGUE_DIR);
    let mut videos = Vec::new();
    for folder in std::fs::read_dir(&league)?.flatten() {
        if !folder.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let folder_name = folder.file_name().to_string_lossy().into_owned();
        for entry in read_dir(&folder.path()) {
            let is_mp4 = entry
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
            // A path that isn't Unicode can't be written in `match.json` as it is.
            let Some(name) = entry
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|_| is_mp4 && entry.to_str().is_some())
            else {
                continue;
            };
            let Some(meta) = std::fs::metadata(&entry).ok().filter(|m| m.is_file()) else {
                continue;
            };
            let named = name_time(name)
                .or_else(|| name_time(&folder_name))
                .and_then(local_to_utc_ms);
            let modified = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            videos.push(Video {
                key: Path::new(LEAGUE_DIR)
                    .join(&folder_name)
                    .join(name)
                    .display()
                    .to_string(),
                bytes: meta.len(),
                started_ms: named.unwrap_or(modified),
                undated: named.is_none(),
                path: entry.clone(),
            });
        }
    }
    videos.sort_by(|a, b| a.started_ms.cmp(&b.started_ms).then(a.key.cmp(&b.key)));
    Ok(videos)
}

/// An import: the Outplayed library it came from and what the user allowed for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    pub id: String,
    pub root: PathBuf,
    /// The last time it imported.
    pub at_ms: i64,
    /// Matches it has added, in all.
    pub matches: usize,
    /// The cleanup may send these videos to the Recycle Bin (from the Outplayed folder).
    /// Off unless the user turns it on for this import.
    #[serde(default)]
    pub cleanup: bool,
}

/// `imports.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Imports {
    schema: u32,
    imports: Vec<Import>,
}

/// What importing an Outplayed library would add (the dry run), or added.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Survey {
    pub root: PathBuf,
    /// League videos found, and their bytes.
    pub videos: usize,
    pub bytes: u64,
    /// Imported before: skipped.
    pub already: usize,
    /// The new ones.
    pub new: usize,
    pub new_bytes: u64,
    /// The localStorage backup, when it is there, and its League records.
    pub export: Option<PathBuf>,
    pub records: usize,
    /// Of the new videos: joined to a record, the markers that brings, those with a
    /// champion, those won, and those whose result stays unknown.
    pub joined: usize,
    pub markers: usize,
    pub champions: usize,
    pub wins: usize,
    pub unknown_results: usize,
    /// Whose length the MP4 gave.
    pub lengths: usize,
    /// Dated by the file, their names having no time.
    pub undated: usize,
}

/// A new video, ready to import.
struct Planned {
    video: Video,
    duration_s: Option<f64>,
    recorded: Option<Recorded>,
}

impl Library {
    /// What importing the Outplayed library at `root` would add. Only reads: nothing is
    /// written, here or there.
    pub fn outplayed_survey(&self, root: &Path) -> io::Result<Survey> {
        self.plan_import(root).map(|(survey, _)| survey)
    }

    /// Imports the Outplayed library at `root`: a match folder for each video not
    /// imported yet, pointing at the video where it is. Nothing under `root` changes.
    pub fn import_outplayed(&self, root: &Path, now_ms: i64) -> io::Result<Survey> {
        let (survey, planned) = self.plan_import(root)?;
        let mut imports = self.read_imports();
        let at = match imports
            .imports
            .iter()
            .position(|i| same_root(&i.root, root))
        {
            Some(at) => at,
            None => {
                imports.imports.push(Import {
                    id: format!("outplayed-{}", imports.imports.len() + 1),
                    root: root.to_owned(),
                    at_ms: now_ms,
                    matches: 0,
                    cleanup: false,
                });
                imports.imports.len() - 1
            }
        };
        let id = imports.imports[at].id.clone();
        let mut added = 0;
        for planned in &planned {
            match self.write_import(&id, planned) {
                Ok(()) => added += 1,
                Err(error) => {
                    tracing::warn!(%error, "could not import an Outplayed video");
                }
            }
        }
        let import = &mut imports.imports[at];
        import.at_ms = now_ms;
        import.matches += added;
        self.write_imports(&mut imports)?;
        self.sync();
        Ok(survey)
    }

    /// The imports so far.
    pub fn imports(&self) -> Vec<Import> {
        self.read_imports().imports
    }

    /// Lets the cleanup remove an import's videos (to the Recycle Bin), or not.
    pub fn set_import_cleanup(&self, id: &str, allowed: bool) -> io::Result<()> {
        let mut imports = self.read_imports();
        let import = imports
            .imports
            .iter_mut()
            .find(|import| import.id == id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such import"))?;
        import.cleanup = allowed;
        self.write_imports(&mut imports)
    }

    fn plan_import(&self, root: &Path) -> io::Result<(Survey, Vec<Planned>)> {
        let videos = find_videos(root)?;
        let export_path = export_file(root);
        let export = if export_path.is_file() {
            match read_export(&export_path) {
                Ok(export) => Some(export),
                Err(error) => {
                    tracing::warn!(%error, "could not read Outplayed's localStorage backup");
                    None
                }
            }
        } else {
            None
        };
        let imported = self.imported_keys();
        let mut survey = Survey {
            root: root.to_owned(),
            videos: videos.len(),
            bytes: videos.iter().map(|v| v.bytes).sum(),
            export: export.is_some().then_some(export_path),
            records: export.as_ref().map_or(0, |e| e.records),
            ..Survey::default()
        };
        let mut planned = Vec::new();
        for video in videos {
            if imported.contains(&video.key.to_lowercase()) {
                survey.already += 1;
                continue;
            }
            let name = video.path.file_name().unwrap_or_default().to_string_lossy();
            let recorded = export
                .as_ref()
                .and_then(|e| e.videos.get(&name.to_lowercase()))
                .cloned();
            let duration_s = mp4_duration(&video.path);
            survey.new += 1;
            survey.new_bytes += video.bytes;
            survey.lengths += usize::from(duration_s.is_some());
            survey.undated += usize::from(video.undated);
            match &recorded {
                Some(recorded) => {
                    survey.joined += 1;
                    survey.markers += recorded.markers.len();
                    survey.champions += usize::from(recorded.champion.is_some());
                    survey.wins += usize::from(recorded.won);
                    survey.unknown_results += usize::from(!recorded.won);
                }
                None => survey.unknown_results += 1,
            }
            planned.push(Planned {
                video,
                duration_s,
                recorded,
            });
        }
        Ok((survey, planned))
    }

    /// One imported video's folder and `match.json`.
    fn write_import(&self, import_id: &str, planned: &Planned) -> io::Result<()> {
        let video = &planned.video;
        let recorded = planned.recorded.as_ref();
        let champion = recorded.and_then(|r| r.champion.as_deref());
        let base = folder_name(
            u64::try_from(video.started_ms).unwrap_or_default(),
            [Some("Outplayed"), champion],
        );
        let (id, dir) = create_unique_dir(&self.matches_dir, &base)?;
        let markers = recorded.map(|r| r.markers.clone()).unwrap_or_default();
        let moments = build_moments(&markers, DEFAULT_MAX_MOMENT_S);
        let mut summary = json!({
            "schema": MATCH_SCHEMA,
            "id": id,
            "source": OUTPLAYED_SOURCE,
            "importId": import_id,
            "startedWallMs": video.started_ms,
            "markers": markers,
            "moments": moments,
            "clips": [],
            "recording": { "path": video.path.display().to_string(), "bytes": video.bytes },
            "outplayed": { "file": video.key },
        });
        if let Some(seconds) = planned.duration_s {
            summary["durationS"] = seconds.into();
            summary["recording"]["durationS"] = seconds.into();
        }
        if let Some(recorded) = recorded {
            if let Some(champion) = &recorded.champion {
                summary["champion"] = champion.as_str().into();
            }
            if let Some(mode) = &recorded.game_mode {
                summary["gameMode"] = mode.as_str().into();
            }
            if let Some([kills, deaths, assists]) = recorded.kda {
                summary["kda"] = json!({ "kills": kills, "deaths": deaths, "assists": assists });
            }
            if recorded.won {
                summary["result"] = "win".into();
            }
            summary["outplayed"]["media"] = recorded.media.as_str().into();
        }
        let written = write_json(&dir.join(MATCH_FILE), &summary);
        if written.is_err() {
            let _ = std::fs::remove_dir(&dir);
        }
        written
    }

    /// The videos imported so far, by their key (lowercase).
    fn imported_keys(&self) -> HashSet<String> {
        read_dir(&self.matches_dir)
            .into_iter()
            .filter_map(|dir| read_json(&dir.join(MATCH_FILE)))
            .filter(|summary| summary["source"] == OUTPLAYED_SOURCE)
            .filter_map(|summary| Some(summary["outplayed"]["file"].as_str()?.to_lowercase()))
            .collect()
    }

    fn imports_path(&self) -> Option<PathBuf> {
        let root = self.matches_dir.parent()?;
        (!root.as_os_str().is_empty()).then(|| root.join(IMPORTS_FILE))
    }

    fn read_imports(&self) -> Imports {
        self.imports_path()
            .and_then(|path| read_json(&path))
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    fn write_imports(&self, imports: &mut Imports) -> io::Result<()> {
        let path = self
            .imports_path()
            .ok_or_else(|| io::Error::other("the library has no root folder"))?;
        imports.schema = IMPORTS_SCHEMA;
        write_json(&path, imports)
    }
}

/// The same folder, as Windows compares names (without regard to case).
fn same_root(a: &Path, b: &Path) -> bool {
    let plain = |path: &Path| {
        path.display()
            .to_string()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    plain(a) == plain(b)
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::TzSpecificLocalTimeToSystemTime;

    use super::LocalTime;

    pub fn local_to_utc_ms(time: LocalTime) -> Option<i64> {
        let local = SYSTEMTIME {
            wYear: u16::try_from(time.year).ok()?,
            wMonth: time.month as u16,
            wDayOfWeek: 0,
            wDay: time.day as u16,
            wHour: time.hour as u16,
            wMinute: time.minute as u16,
            wSecond: time.second as u16,
            wMilliseconds: time.ms as u16,
        };
        let mut utc = SYSTEMTIME::default();
        // SAFETY: both pointers are live SYSTEMTIMEs; no time zone means the PC's own.
        unsafe { TzSpecificLocalTimeToSystemTime(None, &local, &mut utc) }.ok()?;
        Some(
            LocalTime {
                year: i32::from(utc.wYear),
                month: u32::from(utc.wMonth),
                day: u32::from(utc.wDay),
                hour: u32::from(utc.wHour),
                minute: u32::from(utc.wMinute),
                second: u32::from(utc.wSecond),
                ms: u32::from(utc.wMilliseconds),
            }
            .as_utc_ms(),
        )
    }
}

#[cfg(not(windows))]
mod imp {
    use super::LocalTime;

    pub fn local_to_utc_ms(time: LocalTime) -> Option<i64> {
        Some(time.as_utc_ms())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::SystemTime;

    use super::*;
    use crate::library::janitor::Rules;
    use crate::library::Source;
    use crate::tracker::session::SessionConfig;

    #[test]
    fn names_carry_the_local_time() {
        let time = name_time("League of Legends_10-5-2025_21-3-7-45.mp4").unwrap();
        assert_eq!(
            time,
            LocalTime {
                year: 2025,
                month: 10,
                day: 5,
                hour: 21,
                minute: 3,
                second: 7,
                ms: 45
            }
        );
        assert_eq!(
            name_time("League of Legends_04-17-2025_9-15-00-123"),
            Some(LocalTime {
                year: 2025,
                month: 4,
                day: 17,
                hour: 9,
                minute: 15,
                second: 0,
                ms: 123
            }),
            "a folder name, padded or not"
        );
        assert!(name_time("League of Legends_10-5-2025_21-3-7-45.MP4").is_some());
        for bad in [
            "League of Legends_2-30-2025_21-3-7-45.mp4",
            "League of Legends_13-1-2025_21-3-7-45.mp4",
            "League of Legends_1-1-2025_24-0-0-0.mp4",
            "League of Legends_1-1-2025_21-3-7.mp4",
            "League of Legends_1-1-2025_21-3-7-45.mkv",
            "match.mp4",
        ] {
            assert_eq!(name_time(bad), None, "{bad}");
        }
        assert!(
            name_time("League of Legends_2-29-2024_0-0-0-0.mp4").is_some(),
            "a leap day"
        );
    }

    #[test]
    fn local_times_become_unix_milliseconds() {
        let time = LocalTime {
            year: 2026,
            month: 10,
            day: 15,
            hour: 16,
            minute: 12,
            second: 0,
            ms: 5,
        };
        assert_eq!(time.as_utc_ms(), 1_792_080_720_005);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        // On this PC's clock: whole quarter hours from UTC, within a day.
        let local = local_to_utc_ms(time).unwrap();
        let offset = local - time.as_utc_ms();
        assert_eq!(offset % (15 * 60_000), 0, "{offset}");
        assert!(offset.abs() <= 14 * 3_600_000, "{offset}");
    }

    fn push_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend((body.len() as u32 + 8).to_be_bytes());
        out.extend(kind);
        out.extend(body);
    }

    /// A small MP4 with the movie header after the media, as recorders write it.
    fn mp4(seconds: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_box(&mut bytes, b"ftyp", b"isom\0\0\x02\0isomiso2mp41");
        push_box(&mut bytes, b"mdat", &[7u8; 256]);
        let mut mvhd = vec![0u8; 4];
        mvhd.extend([0u8; 8]);
        mvhd.extend(1000u32.to_be_bytes());
        mvhd.extend((seconds * 1000).to_be_bytes());
        mvhd.extend([0u8; 80]);
        let mut moov = Vec::new();
        push_box(&mut moov, b"mvhd", &mvhd);
        push_box(&mut bytes, b"moov", &moov);
        bytes
    }

    fn length(bytes: &[u8]) -> Option<f64> {
        movie_duration(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn mp4_lengths_come_from_the_movie_header() {
        assert_eq!(length(&mp4(1834)), Some(1834.0));

        // Version 1, after a media box with a 64-bit size.
        let mut big = Vec::new();
        push_box(&mut big, b"ftyp", b"isom");
        big.extend(1u32.to_be_bytes());
        big.extend(b"mdat");
        big.extend(32u64.to_be_bytes());
        big.extend([0u8; 16]);
        let mut mvhd = vec![1u8, 0, 0, 0];
        mvhd.extend([0u8; 16]);
        mvhd.extend(90_000u32.to_be_bytes());
        mvhd.extend((90_000u64 * 600 + 45_000).to_be_bytes());
        let mut moov = Vec::new();
        push_box(&mut moov, b"mvhd", &mvhd);
        push_box(&mut big, b"moov", &moov);
        assert_eq!(length(&big), Some(600.5));

        // Fragmented: no length in mvhd, the fragments' in mehd.
        let mut fragmented = Vec::new();
        let mut mvhd = vec![0u8; 4];
        mvhd.extend([0u8; 8]);
        mvhd.extend(1000u32.to_be_bytes());
        mvhd.extend(0u32.to_be_bytes());
        let mut mehd = vec![0u8; 4];
        mehd.extend(42_000u32.to_be_bytes());
        let mut mvex = Vec::new();
        push_box(&mut mvex, b"mehd", &mehd);
        let mut moov = Vec::new();
        push_box(&mut moov, b"mvhd", &mvhd);
        push_box(&mut moov, b"mvex", &mvex);
        push_box(&mut fragmented, b"moov", &moov);
        assert_eq!(length(&fragmented), Some(42.0));

        // Truncated, or no movie header at all.
        let whole = mp4(60);
        assert_eq!(length(&whole[..whole.len() - 40]), None);
        assert_eq!(length(b"not an mp4 at all, just text"), None);
        let mut liar = Vec::new();
        liar.extend(4000u32.to_be_bytes());
        liar.extend(b"moov");
        assert_eq!(length(&liar), None, "a box bigger than the file");
    }

    const SESSION: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f9";

    /// A localStorage backup like Outplayed's: League records among other keys.
    fn export() -> String {
        let events = json!([
            { "type": "kill", "time": 30_000, "timing": { "past": 12_000, "future": 8_000 },
              "data": "{\"count\":\"1\",\"label\":\"kill\",\"totalKills\":\"1\"}" },
            { "type": "kill", "time": 34_000, "timing": { "past": 12_000, "future": 8_000 },
              "data": "{\"count\":\"1\",\"label\":\"double_kill\",\"totalKills\":\"2\"}" },
            { "type": "ultimate", "time": 29_000, "timing": { "past": 5_000, "future": 5_000 } },
            { "type": "assist", "time": 90_000, "timing": { "past": 12_000, "future": 8_000 } },
            { "type": "death", "time": 200_000, "timing": { "past": 12_000, "future": 3_000 } },
            { "type": "victory", "time": 1_700_000, "timing": { "past": 10_000, "future": 1_000 } },
        ]);
        let win = json!({
            "id": "x", "gameId": 5426, "sessionId": SESSION,
            "info": { "gameMode": "lol", "selectedChampion": "Shyvana", "killCount": 7,
                      "deathCount": "2", "assistCount": 9, "isVictory": false },
            "medias": [{ "type": "fullMatch", "startTime": 0, "endTime": 1800,
                "path": "C:\\Users\\x\\Videos\\Outplayed\\League of Legends_10-5-2025_21-3-7-45.mp4",
                "events": events }],
        });
        let unknown = json!({
            "gameId": 5426,
            "info": { "gameMode": "arena", "selectedChampion": "Ahri", "isVictory": false },
            "medias": [{ "type": "replay",
                "path": "Media/Misc/League of Legends_10-6-2025_20-0-0-0.MP4", "events": [] }],
        });
        let other_game = json!({ "gameId": 21640, "medias": [{ "path": "x/other.mp4" }] });
        // Each value is JSON in a string; Outplayed's own are often encoded twice.
        let twice = |value: &Value| Value::String(Value::String(value.to_string()).to_string());
        json!({
            format!("MATCH_{SESSION}_1"): twice(&win),
            format!("MATCH_{SESSION}_2"): unknown.to_string(),
            format!("MATCH_{SESSION}_3"): other_game.to_string(),
            format!("MATCH_ACTIONS_{SESSION}_1"):
                "{\"gameId\":5426,\"medias\":[{\"path\":\"z/actions.mp4\"}]}",
            "owads_token": { "secret": "never kept", "nested": [[1, 2], { "deep": true }] },
            "hashedEmails": ["abc"],
            "MATCH_LOCK": "1",
        })
        .to_string()
    }

    #[test]
    fn only_match_records_are_read_and_joined_by_file_name() {
        let values: MatchValues = serde_json::from_str(&export()).unwrap();
        assert_eq!(values.0.len(), 3, "MATCH_<session>_<n> only");
        assert!(is_match_key(&format!("MATCH_{SESSION}_12")));
        for key in [
            format!("MATCH_ACTIONS_{SESSION}_1"),
            "MATCH_LOCK".to_owned(),
            format!("MATCH_{SESSION}_"),
            "MATCH_zz_1".to_owned(),
            format!("match_{SESSION}_1"),
        ] {
            assert!(!is_match_key(&key), "{key}");
        }

        let dir =
            std::env::temp_dir().join(format!("riftreel-outplayed-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("outplayed.json");
        std::fs::write(&file, export()).unwrap();
        let export = read_export(&file).unwrap();
        assert_eq!(export.records, 2, "League records only");
        assert_eq!(export.videos.len(), 2);

        let win = &export.videos["league of legends_10-5-2025_21-3-7-45.mp4"];
        assert_eq!(win.champion.as_deref(), Some("Shyvana"));
        assert_eq!(win.kda, Some([7, 2, 9]), "a count may come as a string");
        assert!(
            win.won,
            "a victory event counts even when isVictory says no"
        );
        assert_eq!(win.media, "fullMatch");
        assert_eq!(win.game_mode, None);
        let labels: Vec<(u32, MarkerLabel, f64)> = win
            .markers
            .iter()
            .map(|m| (m.id, m.label, m.game_time_s))
            .collect();
        assert_eq!(
            labels,
            [
                (1, MarkerLabel::Kill, 30.0),
                (2, MarkerLabel::DoubleKill, 34.0),
                (3, MarkerLabel::Assist, 90.0),
                (4, MarkerLabel::Death, 200.0),
                (5, MarkerLabel::Victory, 1700.0),
            ],
            "no marker for an ultimate"
        );
        let death = &win.markers[3];
        assert_eq!((death.pre_s, death.post_s), (12.0, 3.0));
        assert_eq!(death.video_time_s, Some(200.0));

        let unknown = &export.videos["league of legends_10-6-2025_20-0-0-0.mp4"];
        assert!(!unknown.won, "isVictory false is unknown, not a loss");
        assert_eq!(unknown.kda, None);
        assert_eq!(unknown.game_mode.as_deref(), Some("CHERRY"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every file under `dir`: path, size and modification time.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut files = Vec::new();
        for path in read_dir(dir) {
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                let meta = std::fs::metadata(&path).unwrap();
                files.push((path, meta.len(), meta.modified().unwrap()));
            }
        }
        files.sort();
        files
    }

    /// An Outplayed library: three League videos in two folders, a stray file, a folder
    /// with no video, and the localStorage backup.
    fn outplayed(root: &Path) {
        let league = root.join(LEAGUE_DIR);
        let session = league.join("League of Legends_10-5-2025_21-0-0-0");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("League of Legends_10-5-2025_21-3-7-45.mp4"),
            mp4(1800),
        )
        .unwrap();
        std::fs::write(
            session.join("League of Legends_10-5-2025_21-40-0-0.mp4"),
            mp4(1500),
        )
        .unwrap();
        std::fs::write(session.join("thumb.sfk0"), b"x").unwrap();
        let other = league.join("League of Legends_10-6-2025_20-0-0-0");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("League of Legends_10-6-2025_20-0-0-0.mp4"),
            mp4(15),
        )
        .unwrap();
        std::fs::create_dir_all(league.join("League of Legends_10-7-2025_1-0-0-0")).unwrap();
        std::fs::create_dir_all(root.join("temp")).unwrap();
        std::fs::write(export_file(root), export()).unwrap();
    }

    fn library(base: &Path) -> Library {
        Library::new(
            base.join("library").join("matches"),
            base.join("library").join("recordings"),
            SessionConfig::default(),
        )
    }

    #[test]
    fn videos_are_imported_by_reference_once_and_never_touched() {
        let base = std::env::temp_dir().join(format!("riftreel-outplayed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("Outplayed");
        outplayed(&root);
        let before = snapshot(&root);
        let library = library(&base);

        // The dry run writes nothing anywhere.
        let survey = library.outplayed_survey(&root).unwrap();
        assert_eq!((survey.videos, survey.new, survey.already), (3, 3, 0));
        assert_eq!((survey.records, survey.joined, survey.markers), (2, 2, 5));
        assert_eq!(
            (survey.champions, survey.wins, survey.unknown_results),
            (2, 1, 2)
        );
        assert_eq!((survey.lengths, survey.undated), (3, 0));
        assert_eq!(survey.export, Some(export_file(&root)));
        assert!(
            !base.join("library").join("matches").exists(),
            "a dry run makes nothing"
        );

        let imported = library.import_outplayed(&root, 1_800_000_000_000).unwrap();
        assert_eq!(imported.new, 3);
        assert_eq!(
            snapshot(&root),
            before,
            "nothing under the Outplayed folder changed"
        );

        let cards = library.list();
        assert_eq!(cards.len(), 3);
        assert!(cards
            .iter()
            .all(|c| c.source == Source::Outplayed && c.video));
        let shyvana = cards
            .iter()
            .find(|c| c.champion.as_deref() == Some("Shyvana"))
            .unwrap();
        assert_eq!(shyvana.result.as_deref(), Some("win"));
        assert_eq!((shyvana.kills, shyvana.multikills), (Some(7), 1));
        assert_eq!(shyvana.duration_s, Some(1800.0));
        let ahri = cards
            .iter()
            .find(|c| c.champion.as_deref() == Some("Ahri"))
            .unwrap();
        assert_eq!(ahri.result, None, "unknown, not a loss");
        let plain = cards.iter().find(|c| c.champion.is_none()).unwrap();
        assert_eq!((plain.duration_s, plain.moments), (Some(1500.0), 0));

        let review = library.get(&shyvana.id).unwrap();
        let original = root
            .join(LEAGUE_DIR)
            .join("League of Legends_10-5-2025_21-0-0-0")
            .join("League of Legends_10-5-2025_21-3-7-45.mp4");
        assert_eq!(review["source"], "outplayed");
        assert_eq!(review["videoPath"], original.display().to_string());
        assert_eq!(review["recording"]["durationS"], 1800.0);
        assert_eq!(review["moments"].as_array().unwrap().len(), 4);
        // It started at 21:03:07.045 on the PC's clock.
        let local = name_time("_10-5-2025_21-3-7-45").unwrap();
        assert_eq!(review["startedWallMs"], local_to_utc_ms(local).unwrap());
        // A clip is planned from the video where it is, into the match's own folder.
        let job = library
            .plan_clip(&shyvana.id, (18.0, 42.0), "double_kill", None)
            .unwrap();
        assert_eq!(job.video, original);
        assert!(job.folder.starts_with(base.join("library")));

        // Again: nothing new, nothing doubled.
        let again = library.import_outplayed(&root, 1_800_000_100_000).unwrap();
        assert_eq!((again.videos, again.already, again.new), (3, 3, 0));
        assert_eq!(library.list().len(), 3);
        let imports = library.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!((imports[0].matches, imports[0].cleanup), (3, false));
        assert_eq!(snapshot(&root), before);

        // A new video in Outplayed: only it comes in.
        let other = root
            .join(LEAGUE_DIR)
            .join("League of Legends_10-6-2025_20-0-0-0");
        std::fs::write(
            other.join("League of Legends_10-6-2025_21-0-0-0.mp4"),
            mp4(20),
        )
        .unwrap();
        let third = library.import_outplayed(&root, 1_800_000_200_000).unwrap();
        assert_eq!((third.already, third.new), (3, 1));
        assert_eq!(library.imports()[0].matches, 4);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_cleanup_leaves_imports_alone_unless_allowed_for_them() {
        let base =
            std::env::temp_dir().join(format!("riftreel-outplayed-janitor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("Outplayed");
        outplayed(&root);
        let library = library(&base);
        library.import_outplayed(&root, 1_800_000_000_000).unwrap();
        // The videos are from 2025: far past 30 days.
        let rules = Rules {
            keep_days: 30,
            max_full_bytes: 0,
            clips_warn_bytes: 0,
        };
        let now = 1_800_000_000_000;
        let plan = library.cleanup_overview(&rules, now).plan;
        assert!(plan.removals.is_empty());
        assert_eq!(plan.unmanaged_matches, 3);
        let report = library.cleanup(&rules, now, |_| panic!("never touched"), || true);
        assert!(report.removed.is_empty());

        let id = library.imports()[0].id.clone();
        library.set_import_cleanup(&id, true).unwrap();
        let allowed = library.cleanup_overview(&rules, now).plan;
        assert_eq!((allowed.removals.len(), allowed.unmanaged_matches), (3, 0));
        assert!(allowed.removals.iter().all(|r| r.path.starts_with(&root)));

        // A match whose video isn't in the imported folder any more stays alone.
        let moved = base.join("elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(moved.join("x.mp4"), mp4(10)).unwrap();
        let first = &allowed.removals[0].id;
        let file = base
            .join("library")
            .join("matches")
            .join(first)
            .join(MATCH_FILE);
        let mut summary = read_json(&file).unwrap();
        summary["recording"]["path"] = moved.join("x.mp4").display().to_string().into();
        std::fs::write(&file, summary.to_string()).unwrap();
        assert_eq!(library.cleanup_overview(&rules, now).plan.removals.len(), 2);

        library.set_import_cleanup(&id, false).unwrap();
        assert!(library
            .cleanup_overview(&rules, now)
            .plan
            .removals
            .is_empty());
        assert!(library.set_import_cleanup("outplayed-9", true).is_err());
        std::fs::remove_dir_all(&base).ok();
    }
}
