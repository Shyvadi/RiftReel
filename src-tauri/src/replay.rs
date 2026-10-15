//! Offline replay of Live Client fixtures (`fixtures/lol/*.json`): the backend of
//! `downplayed-dev replay` and of the fixture tests.
//!
//! A fixture holds what the Live Client Data API would serve during one match:
//! `activeplayer` (or the error a spectator gets), `playerlist`, `gamestats` and the
//! complete `eventdata` history. Payloads are kept as raw JSON so a fake server can
//! serve them verbatim and they go through the same parsing as live data.

use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::league::models::{
    ActivePlayer, EogStatsBlock, EventData, GameStats, GameflowSession, Player, RawEvent,
};
use crate::tracker::machine::EndReason;
use crate::tracker::session::{MatchSession, MatchSummary, SessionConfig};

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("cannot read fixture {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid fixture: {0}")]
    Invalid(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fixture {
    #[serde(default)]
    pub description: String,
    /// `GET /liveclientdata/activeplayer` body; `null` for spectator/replay sessions.
    #[serde(default)]
    pub activeplayer: Value,
    /// Error body `activeplayer` returns when there is no active player.
    #[serde(default)]
    pub activeplayer_error: Option<Value>,
    #[serde(default)]
    pub playerlist: Value,
    #[serde(default)]
    pub gamestats: Value,
    /// The complete `eventdata.Events` history.
    #[serde(default)]
    pub events: Vec<Value>,
}

impl Fixture {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ReplayError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    pub fn parse(json: &str) -> Result<Self, ReplayError> {
        Ok(serde_json::from_str(json)?)
    }

    /// `None` when the fixture is a spectator/replay session.
    pub fn active_player(&self) -> Result<Option<ActivePlayer>, ReplayError> {
        if self.activeplayer.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(self.activeplayer.clone())?))
    }

    pub fn players(&self) -> Result<Vec<Player>, ReplayError> {
        if self.playerlist.is_null() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_value(self.playerlist.clone())?)
    }

    pub fn game_stats(&self) -> Result<GameStats, ReplayError> {
        Ok(serde_json::from_value(self.gamestats.clone())?)
    }

    pub fn events(&self) -> Result<Vec<RawEvent>, ReplayError> {
        let data: EventData = serde_json::from_value(serde_json::json!({ "Events": self.events }))?;
        Ok(data.events)
    }

    /// Game time of the last event.
    pub fn end_time(&self) -> f64 {
        self.events
            .iter()
            .filter_map(|e| e["EventTime"].as_f64())
            .fold(0.0, f64::max)
    }

    /// The `eventdata` body at a given game time: the history so far.
    pub fn eventdata_at(&self, game_time: f64) -> Value {
        let events: Vec<&Value> = self
            .events
            .iter()
            .filter(|e| e["EventTime"].as_f64().unwrap_or(0.0) <= game_time)
            .collect();
        serde_json::json!({ "Events": events })
    }

    /// The `gamestats` body at a given game time.
    pub fn gamestats_at(&self, game_time: f64) -> Value {
        let mut stats = self.gamestats.clone();
        if let Some(object) = stats.as_object_mut() {
            object.insert("gameTime".into(), game_time.into());
        }
        stats
    }
}

/// League client payloads recorded with a fixture: `lcu/<name>.session.json` and
/// `lcu/<name>.eog.json` next to `<name>.json`.
#[derive(Debug, Clone, Default)]
pub struct LcuRecording {
    pub session: Option<GameflowSession>,
    pub eog: Option<EogStatsBlock>,
}

impl LcuRecording {
    /// Whatever exists beside `fixture` (nothing is fine).
    pub fn load_beside(fixture: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let fixture = fixture.as_ref();
        let (Some(dir), Some(stem)) = (fixture.parent(), fixture.file_stem()) else {
            return Ok(Self::default());
        };
        let load = |suffix: &str| -> Result<Option<String>, ReplayError> {
            let path = dir
                .join("lcu")
                .join(format!("{}.{suffix}.json", stem.to_string_lossy()));
            match std::fs::read_to_string(&path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(source) => Err(ReplayError::Read {
                    path: path.display().to_string(),
                    source,
                }),
            }
        };
        Ok(Self {
            session: load("session")?
                .map(|text| serde_json::from_str(&text))
                .transpose()?,
            eog: load("eog")?
                .map(|text| serde_json::from_str(&text))
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReplayReport {
    pub description: String,
    pub event_count: usize,
    pub spectator: bool,
    pub summary: MatchSummary,
}

/// Interval between simulated `eventdata` polls, in game seconds.
const REPLAY_POLL_S: f64 = 0.5;

/// Runs a fixture through the tracker the way a live match would: an `eventdata` poll
/// every 0.5 s of game time, each returning the history so far.
pub fn replay(fixture: &Fixture, config: &SessionConfig) -> Result<ReplayReport, ReplayError> {
    replay_with(fixture, &LcuRecording::default(), config)
}

/// [`replay`], with the League client's gameflow session and end-of-game stats as the
/// runtime would have them.
pub fn replay_with(
    fixture: &Fixture,
    lcu: &LcuRecording,
    config: &SessionConfig,
) -> Result<ReplayReport, ReplayError> {
    let mut session = MatchSession::new(config.clone());
    if let Some(gameflow) = &lcu.session {
        session.set_lcu_session(gameflow.clone());
    }
    session.set_game_stats(&fixture.game_stats()?);
    let spectator = match fixture.active_player()? {
        Some(player) => {
            session.set_identity(&player);
            false
        }
        None => {
            while !session.note_missing_active_player() {}
            true
        }
    };
    session.set_players(fixture.players()?);

    let events = fixture.events()?;
    let end = fixture.end_time();
    let mut polls = 0u32;
    loop {
        let game_time = f64::from(polls) * REPLAY_POLL_S;
        let history = events
            .iter()
            .filter(|e| e.event_time <= game_time)
            .cloned()
            .collect();
        let outcome = session.ingest(history);
        if outcome.game_ended || game_time > end {
            break;
        }
        polls += 1;
    }
    session.set_final_players(fixture.players()?);
    if let Some(eog) = &lcu.eog {
        session.set_eog(eog.clone());
    }
    let ended = if events.iter().any(|e| e.event_name == "GameEnd") {
        EndReason::GameEnd
    } else {
        EndReason::Aborted
    };
    Ok(ReplayReport {
        description: fixture.description.clone(),
        event_count: events.len(),
        spectator,
        summary: session.summary(ended),
    })
}

/// `mm:ss.s`
pub fn clock(seconds: f64) -> String {
    let sign = if seconds < 0.0 { "-" } else { "" };
    let tenths = (seconds.abs() * 10.0).round() as u64;
    format!(
        "{sign}{:02}:{:02}.{}",
        tenths / 600,
        tenths % 600 / 10,
        tenths % 10
    )
}

/// Human-readable report, as printed by `downplayed-dev replay`.
pub fn render_text(report: &ReplayReport) -> String {
    let summary = &report.summary;
    let mut out = String::new();
    let _ = writeln!(out, "{}", report.description);
    let _ = writeln!(
        out,
        "mode {} · champion {} · team {} · {} events",
        summary.game_mode.as_deref().unwrap_or("?"),
        summary.champion.as_deref().unwrap_or("?"),
        summary
            .team
            .map_or("?".to_owned(), |t| format!("{t:?}").to_uppercase()),
        report.event_count,
    );
    if report.spectator {
        let _ = writeln!(out, "no active player (spectator/replay): not tracked");
        return out;
    }

    let _ = writeln!(out, "\nMarkers");
    let _ = writeln!(
        out,
        "  {:>3}  {:>8}  {:<15} {:<21} {:>8}  riot ids",
        "id", "time", "type", "label", "pre/post"
    );
    for marker in &summary.markers {
        let mut label = marker.label.as_str().to_owned();
        if marker.stolen {
            label.push_str(" (stolen)");
        }
        if marker.by_team {
            label.push_str(" (team)");
        }
        let ids: Vec<String> = marker.riot_event_ids.iter().map(u64::to_string).collect();
        let _ = writeln!(
            out,
            "  {:>3}  {:>8}  {:<15} {:<21} {:>8}  {}",
            marker.id,
            clock(marker.game_time_s),
            marker.kind.as_str(),
            label,
            format!("{}/{}", marker.pre_s, marker.post_s),
            ids.join(",")
        );
    }

    let _ = writeln!(out, "\nMoments");
    for (index, moment) in summary.moments.iter().enumerate() {
        let ids: Vec<String> = moment.marker_ids.iter().map(u32::to_string).collect();
        let _ = writeln!(
            out,
            "  {:>3}  {}–{}  {:>5.1} s  {:<15} markers {}",
            index + 1,
            clock(moment.start_s),
            clock(moment.end_s),
            moment.duration_s(),
            moment.label.as_str(),
            ids.join(",")
        );
    }

    let result = summary
        .result
        .map_or("unknown".to_owned(), |r| format!("{r:?}"));
    let kda = summary.kda.map_or("?".to_owned(), |k| {
        format!("{}/{}/{}", k.kills, k.deaths, k.assists)
    });
    let _ = writeln!(out, "\nResult {result} · K/D/A {kda}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formatting() {
        assert_eq!(clock(0.0), "00:00.0");
        assert_eq!(clock(185.24), "03:05.2");
        assert_eq!(clock(3725.0), "62:05.0");
        assert_eq!(clock(-4.5), "-00:04.5");
    }

    #[test]
    fn eventdata_is_served_as_history_so_far() {
        let fixture = Fixture::parse(
            r#"{"events":[{"EventID":0,"EventName":"GameStart","EventTime":0.1},{"EventID":1,"EventName":"MinionsSpawning","EventTime":65.0}]}"#,
        )
        .unwrap();
        assert_eq!(
            fixture.eventdata_at(10.0)["Events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fixture.eventdata_at(65.0)["Events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(fixture.end_time(), 65.0);
    }
}
