//! One tracked match: Live Client data in, markers/moments/summary out.
//!
//! Pure state. `tracker::runtime` does the I/O and feeds the results in here, and so
//! do the fixture replays and tests.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::clock::GameClock;
use super::machine::EndReason;
use super::moments::{build_moments, Moment, DEFAULT_MAX_MOMENT_S};
use super::normalize::{GameResult, Kda, Marker, MarkerChange, MarkerConfig, Normalizer};
use super::roster::{PlayerIdentity, Roster, Team};
use crate::capture::{Recording, RecordingInfo};
use crate::league::live::EventCursor;
use crate::league::models::{
    ActivePlayer, EogStatsBlock, GameStats, GameflowSession, Player, RawEvent,
};
use crate::privacy;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub markers: MarkerConfig,
    pub max_moment_s: f64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            markers: MarkerConfig::default(),
            max_moment_s: DEFAULT_MAX_MOMENT_S,
        }
    }
}

/// The kind of game, for the user's recording filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeClass {
    /// Summoner's Rift and other regular modes.
    Normal,
    Aram,
    Arena,
    PracticeTool,
    Custom,
    /// Teamfight Tactics: out of scope, never tracked.
    Tft,
}

impl ModeClass {
    pub fn classify(game_mode: &str, is_custom: bool) -> Self {
        match game_mode.trim().to_ascii_uppercase().as_str() {
            "PRACTICETOOL" => Self::PracticeTool,
            "TFT" => Self::Tft,
            "CHERRY" => Self::Arena,
            // ARAM: Mayhem (queue 2400) reports `KIWI`.
            "ARAM" | "KIWI" => Self::Aram,
            _ if is_custom => Self::Custom,
            _ => Self::Normal,
        }
    }
}

/// Which kinds of games to track (settings `[tracker.filters]`). Everything is on by
/// default, as in Outplayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchFilters {
    pub practice_tool: bool,
    pub custom_games: bool,
    pub aram: bool,
    pub arena: bool,
}

impl Default for MatchFilters {
    fn default() -> Self {
        Self {
            practice_tool: true,
            custom_games: true,
            aram: true,
            arena: true,
        }
    }
}

impl MatchFilters {
    pub fn allows(&self, class: ModeClass) -> bool {
        match class {
            ModeClass::Normal => true,
            ModeClass::Aram => self.aram,
            ModeClass::Arena => self.arena,
            ModeClass::PracticeTool => self.practice_tool,
            ModeClass::Custom => self.custom_games,
            ModeClass::Tft => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "mode")]
pub enum SkipReason {
    /// No active player: spectating or watching a replay. Never "my" match.
    Spectator,
    /// The user's filters exclude this mode.
    Filtered(ModeClass),
}

/// `activeplayer` must fail this many times in a row, while the game itself answers,
/// before the session counts as a spectator/replay.
pub const SPECTATOR_CONFIRMATIONS: u8 = 3;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IngestOutcome {
    pub new_events: usize,
    pub changes: Vec<MarkerChange>,
    pub game_ended: bool,
}

/// Live view of the current match, for the UI and tray.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchProgress {
    pub tracking: bool,
    pub skipped: Option<SkipReason>,
    pub game_mode: Option<String>,
    pub champion: Option<String>,
    pub game_time_s: Option<f64>,
    pub last_event_id: Option<u64>,
    pub markers: usize,
    pub moments: Vec<Moment>,
}

/// Where a match result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSource {
    /// The League client's end-of-game stats (authoritative).
    EndOfGameStats,
    /// Riot's `GameEnd` event from the Live Client API.
    GameEndEvent,
}

/// Everything known about a finished match; the library saves it as `match.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSummary {
    pub game_id: Option<u64>,
    pub queue_id: Option<i64>,
    pub queue: Option<String>,
    pub game_mode: Option<String>,
    pub champion: Option<String>,
    pub team: Option<Team>,
    pub result: Option<GameResult>,
    pub result_source: Option<ResultSource>,
    pub kda: Option<Kda>,
    pub duration_s: Option<f64>,
    pub ended: EndReason,
    pub started_wall_ms: u64,
    pub event_count: usize,
    /// The match's video, when it was recorded. Marker `videoTimeS` are seconds into it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingInfo>,
    pub markers: Vec<Marker>,
    pub moments: Vec<Moment>,
}

#[derive(Debug)]
pub struct MatchSession {
    config: SessionConfig,
    started_at: Instant,
    started_wall_ms: u64,
    cursor: EventCursor,
    normalizer: Option<Normalizer>,
    me: PlayerIdentity,
    players: Vec<Player>,
    roster: Roster,
    final_scores: Option<Kda>,
    clock: GameClock,
    game_mode: Option<String>,
    game_time: Option<f64>,
    lcu_session: Option<GameflowSession>,
    eog: Option<EogStatsBlock>,
    skip: Option<SkipReason>,
    missing_active_player: u8,
    event_count: usize,
    recording: Option<Recording>,
}

impl MatchSession {
    pub fn new(config: SessionConfig) -> Self {
        let started_wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        Self {
            config,
            started_at: Instant::now(),
            started_wall_ms,
            cursor: EventCursor::new(),
            normalizer: None,
            me: PlayerIdentity::default(),
            players: Vec::new(),
            roster: Roster::default(),
            final_scores: None,
            clock: GameClock::new(),
            game_mode: None,
            game_time: None,
            lcu_session: None,
            eog: None,
            skip: None,
            missing_active_player: 0,
            event_count: 0,
            recording: None,
        }
    }

    /// When tracking started: the reference for marker video times when the match
    /// wasn't recorded.
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn started_wall_ms(&self) -> u64 {
        self.started_wall_ms
    }

    /// The queue as the League client describes it (`Ranked Solo/Duo`), once known.
    pub fn queue(&self) -> Option<&str> {
        self.lcu_session
            .as_ref()
            .map(|s| s.game_data.queue.description.as_str())
            .filter(|d| !d.is_empty())
    }

    /// The match's video: marker video times are counted from its first frame.
    pub fn set_recording(&mut self, recording: Recording) {
        self.recording = Some(recording);
    }

    /// "Me" is known, so events can be attributed.
    pub fn has_identity(&self) -> bool {
        self.normalizer.is_some()
    }

    pub fn needs_roster(&self) -> bool {
        self.roster.is_empty()
    }

    pub fn is_tracking(&self) -> bool {
        self.skip.is_none()
    }

    pub fn skip_reason(&self) -> Option<SkipReason> {
        self.skip
    }

    pub fn has_eog(&self) -> bool {
        self.eog.is_some()
    }

    pub fn has_lcu_session(&self) -> bool {
        self.lcu_session.is_some()
    }

    pub fn cursor(&self) -> &EventCursor {
        &self.cursor
    }

    pub fn clock(&self) -> &GameClock {
        &self.clock
    }

    pub fn game_mode(&self) -> Option<&str> {
        self.game_mode.as_deref()
    }

    pub fn champion(&self) -> Option<&str> {
        self.roster
            .me()
            .map(|e| e.champion.as_str())
            .filter(|c| !c.is_empty())
    }

    pub fn markers(&self) -> &[Marker] {
        match &self.normalizer {
            Some(normalizer) => normalizer.markers(),
            None => &[],
        }
    }

    pub fn moments(&self) -> Vec<Moment> {
        build_moments(self.markers(), self.config.max_moment_s)
    }

    /// The local player from `activeplayer`.
    pub fn set_identity(&mut self, player: &ActivePlayer) {
        let me = PlayerIdentity::from_active_player(player);
        if !me.is_known() {
            return;
        }
        privacy::register_names(me.names());
        self.missing_active_player = 0;
        self.me = me.clone();
        self.roster = Roster::from_players(&self.players, &self.me);
        match &mut self.normalizer {
            Some(normalizer) => normalizer.set_roster(self.roster.clone()),
            None => {
                self.normalizer = Some(Normalizer::new(
                    self.config.markers.clone(),
                    me,
                    self.roster.clone(),
                ))
            }
        }
    }

    /// `activeplayer` answered with an error while the game was up. Returns `true` once
    /// the session is confirmed as a spectator/replay session (then it stops tracking).
    pub fn note_missing_active_player(&mut self) -> bool {
        if self.normalizer.is_some() {
            return false;
        }
        self.missing_active_player = self.missing_active_player.saturating_add(1);
        if self.missing_active_player >= SPECTATOR_CONFIRMATIONS {
            self.skip = Some(SkipReason::Spectator);
            return true;
        }
        false
    }

    /// The match roster from `playerlist`.
    pub fn set_players(&mut self, players: Vec<Player>) {
        self.roster = Roster::from_players(&players, &self.me);
        privacy::register_names(self.roster.names());
        if let Some(normalizer) = &mut self.normalizer {
            normalizer.set_roster(self.roster.clone());
        }
        self.players = players;
    }

    /// `playerlist` read at the end of the match: final K/D/A.
    pub fn set_final_players(&mut self, players: Vec<Player>) {
        self.set_players(players);
        self.final_scores = self.roster.me().map(|e| Kda {
            kills: e.scores.kills,
            deaths: e.scores.deaths,
            assists: e.scores.assists,
        });
    }

    pub fn set_game_stats(&mut self, stats: &GameStats) {
        if !stats.game_mode.is_empty() {
            self.game_mode = Some(stats.game_mode.clone());
        }
        if stats.game_time > 0.0 {
            self.game_time = Some(stats.game_time);
        }
    }

    /// A `gamestats.gameTime` reading for clock alignment (see `tracker::clock`).
    pub fn record_clock(&mut self, game_time: f64, sent: Instant, received: Instant) {
        self.clock.record(game_time, sent, received);
    }

    pub fn set_lcu_session(&mut self, session: GameflowSession) {
        self.lcu_session = Some(session);
    }

    pub fn set_eog(&mut self, eog: EogStatsBlock) {
        self.eog = Some(eog);
    }

    pub fn mode_class(&self) -> Option<ModeClass> {
        if let Some(session) = &self.lcu_session {
            let data = &session.game_data;
            let mode = [
                data.queue.game_mode.as_str(),
                session.map.game_mode.as_str(),
            ]
            .into_iter()
            .find(|m| !m.is_empty())
            .unwrap_or_default();
            if !mode.is_empty() || data.is_custom_game {
                return Some(ModeClass::classify(mode, data.is_custom_game));
            }
        }
        self.game_mode
            .as_deref()
            .map(|mode| ModeClass::classify(mode, false))
    }

    /// Applies the user's filters once the mode is known. Returns why the match isn't
    /// tracked, if it isn't.
    pub fn apply_filters(&mut self, filters: &MatchFilters) -> Option<SkipReason> {
        if self.skip.is_none() {
            if let Some(class) = self.mode_class().filter(|c| !filters.allows(*c)) {
                self.skip = Some(SkipReason::Filtered(class));
            }
        }
        self.skip
    }

    /// Feeds one `eventdata` poll (the full history). Nothing is consumed until "me" is
    /// known, so no event is lost while `activeplayer` is still loading.
    pub fn ingest(&mut self, events: Vec<RawEvent>) -> IngestOutcome {
        let mut outcome = IngestOutcome::default();
        if self.skip.is_some() {
            return outcome;
        }
        let Some(normalizer) = self.normalizer.as_mut() else {
            return outcome;
        };
        for event in self.cursor.take_new(events) {
            tracing::trace!(?event, "live event");
            outcome.new_events += 1;
            outcome.game_ended |= event.event_name == "GameEnd";
            self.game_time = Some(self.game_time.unwrap_or_default().max(event.event_time));
            outcome.changes.extend(normalizer.push(&event));
        }
        self.event_count += outcome.new_events;
        outcome
    }

    pub fn progress(&self) -> MatchProgress {
        MatchProgress {
            tracking: self.is_tracking(),
            skipped: self.skip,
            game_mode: self.game_mode.clone(),
            champion: self.champion().map(str::to_owned),
            game_time_s: self.game_time,
            last_event_id: self.cursor.last_event_id(),
            markers: self.markers().len(),
            moments: self.moments(),
        }
    }

    pub fn summary(&self, ended: EndReason) -> MatchSummary {
        let normalizer = self.normalizer.as_ref();
        let eog = self.eog.as_ref();
        let queue = self.lcu_session.as_ref().map(|s| &s.game_data.queue);

        let counted = normalizer.map(Normalizer::counted_kda);
        if let (Some(final_scores), Some(counted)) = (self.final_scores, counted) {
            if final_scores.kills != counted.kills {
                tracing::debug!(
                    ?final_scores,
                    ?counted,
                    "playerlist kills differ from ChampionKill events"
                );
            }
        }
        let eog_kda = eog
            .and_then(EogStatsBlock::kda)
            .map(|(kills, deaths, assists)| Kda {
                kills,
                deaths,
                assists,
            });
        let eog_result = eog.and_then(EogStatsBlock::win).map(|win| {
            if win {
                GameResult::Win
            } else {
                GameResult::Lose
            }
        });
        let event_result = normalizer.and_then(Normalizer::result);
        if let (Some(from_eog), Some(from_event)) = (eog_result, event_result) {
            if from_eog != from_event {
                tracing::warn!(
                    ?from_eog,
                    ?from_event,
                    "end-of-game stats and GameEnd disagree"
                );
            }
        }
        // End-of-game stats are authoritative; Riot's GameEnd event is the fallback.
        let (result, result_source) = match (eog_result, event_result) {
            (Some(result), _) => (Some(result), Some(ResultSource::EndOfGameStats)),
            (None, Some(result)) => (Some(result), Some(ResultSource::GameEndEvent)),
            (None, None) => (None, None),
        };

        // Video time 0 is the recording's first frame (or, unrecorded, tracking's start).
        let video_start = self
            .recording
            .as_ref()
            .map_or(self.started_at, |r| r.started_at);
        let markers: Vec<Marker> = self
            .markers()
            .iter()
            .cloned()
            .map(|mut marker| {
                marker.video_time_s = self.clock.video_time(marker.game_time_s, video_start);
                marker
            })
            .collect();
        let moments = build_moments(&markers, self.config.max_moment_s);
        let recording = self.recording.as_ref().map(|r| RecordingInfo {
            game_time_at_start_s: self.clock.game_time_at(r.started_at),
            ..r.info.clone()
        });

        MatchSummary {
            game_id: self
                .lcu_session
                .as_ref()
                .map(|s| s.game_data.game_id)
                .filter(|id| *id != 0)
                .or_else(|| eog.map(|e| e.game_id).filter(|id| *id != 0)),
            queue_id: queue.map(|q| q.id),
            queue: queue
                .map(|q| q.description.clone())
                .filter(|d| !d.is_empty()),
            game_mode: queue
                .map(|q| q.game_mode.clone())
                .filter(|m| !m.is_empty())
                .or_else(|| self.game_mode.clone()),
            champion: self
                .champion()
                .map(str::to_owned)
                .or_else(|| eog.and_then(|e| e.local_player.as_ref()?.champion_name.clone())),
            team: self.roster.my_team(),
            result,
            result_source,
            kda: eog_kda.or(self.final_scores).or(counted),
            duration_s: eog
                .map(|e| e.game_length as f64)
                .filter(|d| *d > 0.0)
                .or_else(|| normalizer.and_then(Normalizer::game_end_time))
                .or(self.game_time),
            ended,
            started_wall_ms: self.started_wall_ms,
            event_count: self.event_count,
            recording,
            markers,
            moments,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::replay::Fixture;
    use crate::tracker::normalize::MarkerLabel;

    fn fixture(name: &str) -> Fixture {
        Fixture::load(format!(
            "{}/../fixtures/lol/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn loaded_session(fixture: &Fixture) -> MatchSession {
        let mut session = MatchSession::new(SessionConfig::default());
        session.set_game_stats(&fixture.game_stats().unwrap());
        session.set_identity(&fixture.active_player().unwrap().unwrap());
        session.set_players(fixture.players().unwrap());
        session
    }

    #[test]
    fn events_wait_for_identity_and_are_not_lost() {
        let fixture = fixture("ranked_win.json");
        let events = fixture.events().unwrap();
        let mut session = MatchSession::new(SessionConfig::default());
        assert_eq!(
            session.ingest(events.clone()).new_events,
            0,
            "who is 'me'? not known yet"
        );
        session.set_identity(&fixture.active_player().unwrap().unwrap());
        session.set_players(fixture.players().unwrap());
        let outcome = session.ingest(events.clone());
        assert_eq!(outcome.new_events, events.len());
        assert!(outcome.game_ended);
        assert_eq!(session.ingest(events).new_events, 0);
    }

    #[test]
    fn spectators_are_never_tracked() {
        let fixture = fixture("spectator.json");
        assert!(fixture.active_player().unwrap().is_none());
        let mut session = MatchSession::new(SessionConfig::default());
        assert!(!session.note_missing_active_player());
        assert!(!session.note_missing_active_player());
        assert!(session.note_missing_active_player());
        assert_eq!(session.skip_reason(), Some(SkipReason::Spectator));
        assert_eq!(session.ingest(fixture.events().unwrap()).new_events, 0);
    }

    #[test]
    fn filters_skip_modes_the_user_turned_off() {
        let fixture = fixture("aram_loss.json");
        let mut session = loaded_session(&fixture);
        assert_eq!(session.mode_class(), Some(ModeClass::Aram));
        assert_eq!(session.apply_filters(&MatchFilters::default()), None);
        let no_aram = MatchFilters {
            aram: false,
            ..MatchFilters::default()
        };
        assert_eq!(
            session.apply_filters(&no_aram),
            Some(SkipReason::Filtered(ModeClass::Aram))
        );
        assert_eq!(session.ingest(fixture.events().unwrap()).new_events, 0);
        assert!(!MatchFilters::default().allows(ModeClass::Tft));
        assert_eq!(
            ModeClass::classify("KIWI", false),
            ModeClass::Aram,
            "ARAM: Mayhem"
        );
        assert!(!no_aram.allows(ModeClass::classify("kiwi", false)));
    }

    #[test]
    fn lcu_session_decides_the_mode_when_present() {
        let mut session = MatchSession::new(SessionConfig::default());
        session.set_game_stats(&GameStats {
            game_mode: "CLASSIC".into(),
            ..GameStats::default()
        });
        let mut lcu = GameflowSession::default();
        lcu.game_data.is_custom_game = true;
        lcu.game_data.queue.game_mode = "PRACTICETOOL".into();
        session.set_lcu_session(lcu);
        assert_eq!(session.mode_class(), Some(ModeClass::PracticeTool));
    }

    #[test]
    fn summary_prefers_end_of_game_stats() {
        let fixture = fixture("ranked_win.json");
        let mut session = loaded_session(&fixture);
        session.ingest(fixture.events().unwrap());
        let from_events = session.summary(EndReason::GameEnd);
        assert_eq!(from_events.result, Some(GameResult::Win));
        assert_eq!(from_events.result_source, Some(ResultSource::GameEndEvent));
        assert_eq!(
            from_events.kda,
            Some(Kda {
                kills: 4,
                deaths: 2,
                assists: 2
            })
        );
        assert_eq!(from_events.champion.as_deref(), Some("Shyvana"));
        assert_eq!(from_events.team, Some(Team::Order));

        let eog: EogStatsBlock = serde_json::from_str(
            r#"{"gameId":42,"gameLength":1803,"localPlayer":{"stats":{"WIN":1,"CHAMPIONS_KILLED":4,"NUM_DEATHS":2,"ASSISTS":3}}}"#,
        )
        .unwrap();
        session.set_eog(eog);
        let summary = session.summary(EndReason::GameEnd);
        assert_eq!(summary.game_id, Some(42));
        assert_eq!(summary.result_source, Some(ResultSource::EndOfGameStats));
        assert_eq!(summary.duration_s, Some(1803.0));
        assert_eq!(
            summary.kda,
            Some(Kda {
                kills: 4,
                deaths: 2,
                assists: 3
            })
        );
        assert!(summary
            .markers
            .iter()
            .any(|m| m.label == MarkerLabel::Victory));
    }

    #[test]
    fn marker_video_times_follow_the_clock() {
        let fixture = fixture("ranked_win.json");
        let mut session = loaded_session(&fixture);
        let start = session.started_at();
        // Tracking began at game time 100.
        session.record_clock(100.0, start, start);
        session.ingest(fixture.events().unwrap());
        let summary = session.summary(EndReason::GameEnd);
        let first = &summary.markers[0];
        let video = first.video_time_s.unwrap();
        assert!(
            (video - (first.game_time_s - 100.0)).abs() < 1e-6,
            "{video}"
        );
    }

    #[test]
    fn marker_video_times_count_from_the_recordings_first_frame() {
        let fixture = fixture("ranked_win.json");
        let mut session = loaded_session(&fixture);
        let start = session.started_at();
        session.record_clock(100.0, start, start);
        // The video's first frame came 3 s after tracking began (game time 103).
        session.set_recording(Recording {
            started_at: start + std::time::Duration::from_secs(3),
            info: RecordingInfo {
                file: "match.mp4".into(),
                start_wall_ms: 1_792_080_723_000,
                end_wall_ms: 1_792_082_526_000,
                game_time_at_start_s: None,
                encoder: "h264_nvenc".into(),
                width: 2560,
                height: 1440,
                fps: 60,
                bitrate_kbps: 10_000,
                frames: 108_000,
                dropped_frames: 0,
                duplicated_frames: 4,
                error: None,
                audio: None,
                folder: std::path::PathBuf::from(r"C:\library\2026-10-15_16-12-00"),
            },
        });
        session.ingest(fixture.events().unwrap());
        let summary = session.summary(EndReason::GameEnd);
        let first = &summary.markers[0];
        let video = first.video_time_s.unwrap();
        assert!(
            (video - (first.game_time_s - 103.0)).abs() < 1e-6,
            "{video}"
        );
        let recording = summary.recording.as_ref().unwrap();
        assert!((recording.game_time_at_start_s.unwrap() - 103.0).abs() < 1e-6);

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["recording"]["file"], "match.mp4");
        assert_eq!(json["recording"]["startWallMs"], 1_792_080_723_000_u64);
        assert_eq!(json["recording"]["droppedFrames"], 0);
        assert!(
            json["recording"].get("folder").is_none(),
            "no absolute paths in match.json"
        );
        assert!(
            serde_json::to_value(loaded_session(&fixture).summary(EndReason::GameEnd))
                .unwrap()
                .get("recording")
                .is_none()
        );
    }

    /// Collects formatted log output for one closure.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture_logs(level: tracing::Level, f: impl FnOnce()) -> String {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        // While only one subscriber is registered, tracing-core works out a callsite's
        // interest from the default of whichever thread hits it first, so a test on
        // another thread (no subscriber) could switch this one's events off. A second
        // registered subscriber makes it consult every registered one instead.
        let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let bytes = capture.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_whole_match_is_silent_at_info_level() {
        let fixture = fixture("ranked_win.json");
        let info = capture_logs(tracing::Level::INFO, || {
            let mut session = loaded_session(&fixture);
            let events = fixture.events().unwrap();
            for cut in 0..=events.len() {
                session.ingest(events[..cut].to_vec());
            }
            session.summary(EndReason::GameEnd);
        });
        assert_eq!(info, "", "per-event logging belongs at debug/trace");
    }

    #[test]
    fn trace_logs_never_contain_player_names() {
        let fixture = fixture("ranked_win.json");
        let trace = capture_logs(tracing::Level::TRACE, || {
            let mut session = loaded_session(&fixture);
            session.ingest(fixture.events().unwrap());
        });
        assert!(
            trace.contains("live event"),
            "trace logging is on in this test"
        );
        for player in fixture.players().unwrap() {
            let name = player.riot_id_game_name.unwrap();
            assert!(!trace.contains(&name), "{name} leaked into trace logs");
        }
    }
}
