//! Match history for the window: the library of doc 03 §7 before there is video. Every
//! finished match gets a folder with its `match.json` (the tracker's [`MatchSummary`]
//! plus `schema` and `id`). Recordings saved by `downplayed-dev record` are listed too
//! and replayed into the same summary, so the window reviews both the same way.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::recorder::utc_parts;
use crate::replay::{replay_with, Fixture, LcuRecording};
use crate::tracker::session::{MatchSummary, SessionConfig};

pub const MATCH_FILE: &str = "match.json";
pub const MATCH_SCHEMA: u32 = 1;

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
    /// returns its id, the folder name.
    pub fn save(&self, summary: &MatchSummary) -> io::Result<String> {
        let started = UNIX_EPOCH + std::time::Duration::from_millis(summary.started_wall_ms);
        let (date, hour, minute) = utc_parts(started);
        let second = summary.started_wall_ms / 1000 % 60;
        let mut base = format!("{date}_{hour:02}-{minute:02}-{second:02}");
        for part in [
            summary.queue.as_deref().or(summary.game_mode.as_deref()),
            summary.champion.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            base.push('_');
            base.push_str(&slug(part));
        }
        std::fs::create_dir_all(&self.matches_dir)?;
        let mut id = base.clone();
        let mut n = 1;
        while self.matches_dir.join(&id).exists() {
            n += 1;
            id = format!("{base}-{n}");
        }
        let dir = self.matches_dir.join(&id);
        std::fs::create_dir_all(&dir)?;
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
                cards.push(card(id, Source::Game, when, &summary));
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
    /// `source` and, for recordings, `description`.
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
        let mut summary = read_json(&self.matches_dir.join(safe_name(id)?).join(MATCH_FILE))?;
        summary
            .as_object_mut()?
            .insert("source".into(), "game".into());
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
    }
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
