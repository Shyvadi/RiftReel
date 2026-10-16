//! Match history for the window: the library of doc 03 §7. Every finished match gets a
//! folder with its `match.json` (the tracker's [`MatchSummary`] plus `schema` and `id`)
//! and, when it was recorded, `match.mp4`. Recordings saved by `downplayed-dev record`
//! are listed too and replayed into the same summary, so the window reviews both the
//! same way.
//!
//! A recorded match's folder is created when recording starts, named from what is known
//! then (`<date>_<time>[_<queue>]`), and renamed to the full name when the match is
//! saved.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::recorder::utc_parts;
use crate::replay::{replay_with, Fixture, LcuRecording};
use crate::tracker::session::{MatchSummary, SessionConfig};

pub const MATCH_FILE: &str = "match.json";
pub const MATCH_SCHEMA: u32 = 1;
/// The full-match video in a match folder.
pub const VIDEO_FILE: &str = "match.mp4";

/// Recordings are addressed as `recording:<file stem>`.
const RECORDING_PREFIX: &str = "recording:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Tracked live by Downplayed.
    Game,
    /// A fixture recorded by `downplayed-dev record`.
    Recording,
}

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
    /// The match was recorded (`match.mp4` is there).
    pub video: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchFile<'a> {
    schema: u32,
    id: &'a str,
    #[serde(flatten)]
    summary: &'a MatchSummary,
}

#[derive(Debug, Clone)]
pub struct Library {
    matches_dir: PathBuf,
    recordings_dir: PathBuf,
    /// How recordings are replayed (the user's marker settings).
    config: SessionConfig,
}

impl Library {
    pub fn new(matches_dir: PathBuf, recordings_dir: PathBuf, config: SessionConfig) -> Self {
        Self {
            matches_dir,
            recordings_dir,
            config,
        }
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
        let text = serde_json::to_string_pretty(&file).map_err(io::Error::other)?;
        let path = dir.join(MATCH_FILE);
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text + "\n")?;
        std::fs::rename(&temp, &path)?;
        Ok(id)
    }

    /// Every saved match and recording, newest first. Unreadable entries are skipped.
    pub fn list(&self) -> Vec<MatchCard> {
        let mut cards = Vec::new();
        for entry in read_dir(&self.matches_dir) {
            let Some(id) = entry
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if let Some(summary) = read_json(&entry.join(MATCH_FILE)) {
                let when = summary["startedWallMs"].as_u64().unwrap_or_default();
                let mut card = card(id, Source::Game, when, &summary);
                card.video = video_path(&entry, &summary).is_some();
                cards.push(card);
            }
        }
        for entry in read_dir(&self.recordings_dir) {
            if entry.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(summary) = self.replay_recording(&entry) {
                cards.push(card(
                    format!("{RECORDING_PREFIX}{stem}"),
                    Source::Recording,
                    modified_ms(&entry),
                    &summary,
                ));
            }
        }
        cards.sort_by(|a, b| b.when_ms.cmp(&a.when_ms));
        cards
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
        Some(summary)
    }

    fn replay_recording(&self, path: &Path) -> Option<Value> {
        let fixture = Fixture::load(path).ok()?;
        let lcu = LcuRecording::load_beside(path).unwrap_or_default();
        let report = replay_with(&fixture, &lcu, &self.config).ok()?;
        serde_json::to_value(&report.summary).ok()
    }
}

fn card(id: String, source: Source, when_ms: u64, summary: &Value) -> MatchCard {
    let text = |key: &str| summary[key].as_str().map(str::to_owned);
    MatchCard {
        id,
        source,
        when_ms,
        champion: text("champion"),
        queue: text("queue"),
        game_mode: text("gameMode"),
        result: text("result"),
        kills: summary["kda"]["kills"].as_u64(),
        deaths: summary["kda"]["deaths"].as_u64(),
        assists: summary["kda"]["assists"].as_u64(),
        duration_s: summary["durationS"].as_f64(),
        moments: summary["moments"].as_array().map_or(0, Vec::len),
        video: false,
    }
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

fn modified_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
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
            let review = library.get("m1").unwrap();
            assert_eq!(review.get("videoPath").is_some(), plays, "{file}");
            assert_eq!(library.list()[0].video, plays, "{file}");
        }
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
}
