//! The tracker runtime end to end: League client phases (in-process fake) plus a fake
//! Live Client API over HTTPS, through a crash/reconnect to a recorded win.

mod support;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, watch};

use downplayed_lib::league::client::{LcuError, LeagueClient};
use downplayed_lib::league::live::LiveClient;
use downplayed_lib::league::models::{EogStatsBlock, GameflowPhase, GameflowSession};
use downplayed_lib::settings::Settings;
use downplayed_lib::tracker::machine::{EndReason, TrackerState};
use downplayed_lib::tracker::normalize::{GameResult, Kda};
use downplayed_lib::tracker::runtime::{
    RuntimeEvent, Timings, TrackerConfig, TrackerRuntime, TrackerStatus,
};
use downplayed_lib::tracker::session::{MatchFilters, MatchSummary, ResultSource, SkipReason};
use support::{load_fixture, load_json, FakeLiveServer, TestPki};

const WAIT: Duration = Duration::from_secs(10);

/// League client double: the gameflow session is always there, end-of-game stats only
/// once `eog_ready` is set (the client publishes them on its end-of-game screen).
struct FakeLcu {
    session: GameflowSession,
    eog: EogStatsBlock,
    eog_ready: AtomicBool,
}

impl FakeLcu {
    fn ranked_win() -> Arc<Self> {
        Arc::new(Self {
            session: serde_json::from_value(load_json("lol/lcu/ranked_win.session.json")).unwrap(),
            eog: serde_json::from_value(load_json("lol/lcu/ranked_win.eog.json")).unwrap(),
            eog_ready: AtomicBool::new(false),
        })
    }
}

impl LeagueClient for FakeLcu {
    fn gameflow_phase(&self) -> BoxFuture<'_, Result<GameflowPhase, LcuError>> {
        Box::pin(async { Ok(GameflowPhase::InProgress) })
    }

    fn gameflow_session(&self) -> BoxFuture<'_, Result<GameflowSession, LcuError>> {
        Box::pin(async { Ok(self.session.clone()) })
    }

    fn eog_stats_block(&self) -> BoxFuture<'_, Result<EogStatsBlock, LcuError>> {
        Box::pin(async {
            if self.eog_ready.load(Ordering::SeqCst) {
                Ok(self.eog.clone())
            } else {
                Err(LcuError::Status {
                    status: 404,
                    path: "/lol-end-of-game/v1/eog-stats-block".into(),
                })
            }
        })
    }
}

struct Harness {
    server: FakeLiveServer,
    events: mpsc::Sender<RuntimeEvent>,
    status: watch::Receiver<TrackerStatus>,
    summaries: mpsc::UnboundedReceiver<MatchSummary>,
    _pki: TestPki,
}

impl Harness {
    async fn start(fixture: &str, config: TrackerConfig) -> Self {
        let pki = TestPki::new();
        let server = FakeLiveServer::start(&pki, load_fixture(fixture)).await;
        server.set_up(false); // no game yet
        let live = LiveClient::with_tls(server.url(), pki.client_config()).unwrap();
        let timings = Timings {
            event_poll: Duration::from_millis(20),
            clock_sample: Duration::from_millis(50),
            live_probe: Duration::from_millis(20),
            finalize_wait: Duration::from_secs(5),
        };
        let (status_tx, status) = watch::channel(TrackerStatus::default());
        let (summary_tx, summaries) = mpsc::unbounded_channel();
        let (events, events_rx) = mpsc::channel(16);
        let runtime =
            TrackerRuntime::new(config, live, timings, status_tx).with_summaries(summary_tx);
        tokio::spawn(runtime.run(events_rx));
        Self {
            server,
            events,
            status,
            summaries,
            _pki: pki,
        }
    }

    async fn send(&self, event: RuntimeEvent) {
        self.events.send(event).await.unwrap();
    }

    async fn phase(&self, phase: GameflowPhase) {
        self.send(RuntimeEvent::Phase(phase)).await;
    }

    /// Waits until the published status satisfies `condition`.
    async fn wait_for(
        &mut self,
        what: &str,
        condition: impl Fn(&TrackerStatus) -> bool,
    ) -> TrackerStatus {
        let reached = match tokio::time::timeout(WAIT, self.status.wait_for(|s| condition(s))).await
        {
            Ok(Ok(status)) => Some(status.clone()),
            _ => None,
        };
        reached.unwrap_or_else(|| {
            panic!(
                "timed out waiting for {what}; status: {:#?}",
                *self.status.borrow()
            )
        })
    }

    async fn wait_for_event(&mut self, id: u64) {
        self.wait_for(&format!("EventID {id}"), |s| {
            s.current.as_ref().and_then(|c| c.last_event_id) == Some(id)
        })
        .await;
    }

    async fn summary(&mut self) -> MatchSummary {
        tokio::time::timeout(WAIT, self.summaries.recv())
            .await
            .expect("no match summary")
            .expect("runtime stopped")
    }
}

fn labels(summary: &MatchSummary) -> Vec<&'static str> {
    summary.markers.iter().map(|m| m.label.as_str()).collect()
}

const RANKED_WIN_LABELS: [&str; 13] = [
    "kill",
    "death",
    "dragon_kill",
    "assist",
    "turret_kill",
    "kill",
    "double_kill",
    "assist",
    "baron_kill",
    "kill",
    "inhibitor_kill",
    "death",
    "victory",
];

#[tokio::test]
async fn ranked_win_through_a_crash_and_reconnect() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    let lcu = FakeLcu::ranked_win();
    h.send(RuntimeEvent::LcuConnected(lcu.clone())).await;
    h.phase(GameflowPhase::Lobby).await;
    h.phase(GameflowPhase::ChampSelect).await;
    h.wait_for("champ select", |s| {
        s.state == TrackerState::Armed && s.lcu_connected
    })
    .await;

    // The game loads and the client reports InProgress.
    h.server.set_game_time(20.0);
    h.server.set_up(true);
    h.phase(GameflowPhase::GameStart).await;
    h.phase(GameflowPhase::InProgress).await;
    let status = h
        .wait_for("tracking", |s| {
            s.current
                .as_ref()
                .is_some_and(|c| c.champion.as_deref() == Some("Shyvana"))
        })
        .await;
    assert_eq!(status.state, TrackerState::InGame);

    h.server.set_game_time(1000.0);
    h.wait_for_event(11).await;

    // Crash: the game dies, the client shows Reconnect, the game comes back later.
    h.server.set_up(false);
    h.phase(GameflowPhase::Reconnect).await;
    h.wait_for("reconnecting", |s| {
        s.reconnecting && s.state == TrackerState::InGame
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await; // a few failed polls
    h.server.set_game_time(1300.0);
    h.server.set_up(true);
    h.phase(GameflowPhase::InProgress).await;
    h.wait_for_event(15).await;
    assert!(!h.status.borrow().reconnecting);

    // Nexus down: GameEnd arrives, the client moves to its end-of-game screens.
    h.server.set_game_time(1810.0);
    h.wait_for("finalizing", |s| s.state == TrackerState::Finalizing)
        .await;
    h.phase(GameflowPhase::WaitingForStats).await;
    h.phase(GameflowPhase::PreEndOfGame).await; // stats not published yet: keeps waiting
    lcu.eog_ready.store(true, Ordering::SeqCst);
    h.server.set_up(false); // the game closes
    h.phase(GameflowPhase::EndOfGame).await;

    let summary = h.summary().await;
    assert_eq!(labels(&summary), RANKED_WIN_LABELS);
    assert_eq!(
        summary.result,
        Some(GameResult::Win),
        "wins must be recorded"
    );
    assert_eq!(summary.result_source, Some(ResultSource::EndOfGameStats));
    assert_eq!(
        summary.kda,
        Some(Kda {
            kills: 4,
            deaths: 2,
            assists: 2
        })
    );
    assert_eq!(summary.game_id, Some(5_123_456_789));
    assert_eq!(summary.queue_id, Some(420));
    assert_eq!(summary.queue.as_deref(), Some("Ranked Solo/Duo"));
    assert_eq!(summary.champion.as_deref(), Some("Shyvana"));
    assert_eq!(summary.duration_s, Some(1803.0));
    assert_eq!(summary.ended, EndReason::GameEnd);
    let mut seen = HashSet::new();
    assert!(
        summary
            .markers
            .iter()
            .flat_map(|m| &m.riot_event_ids)
            .all(|id| seen.insert(*id)),
        "duplicate markers"
    );
    assert!(
        summary.markers.iter().all(|m| m.video_time_s.is_some()),
        "clock was synced"
    );
    assert_eq!(summary.moments.len(), 10);

    let status = h.wait_for("idle", |s| s.state == TrackerState::Idle).await;
    assert_eq!(
        status.last_match.as_ref().map(|m| m.result),
        Some(Some(GameResult::Win))
    );
    assert!(status.current.is_none());
}

#[tokio::test]
async fn league_client_appearing_mid_match_adds_its_details() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    // Downplayed started mid-game, before it reached the client: degraded start.
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(600.0);
    h.server.set_up(true);
    h.wait_for_event(5).await;

    let lcu = FakeLcu::ranked_win();
    lcu.eog_ready.store(true, Ordering::SeqCst);
    h.send(RuntimeEvent::LcuConnected(lcu)).await;
    h.phase(GameflowPhase::InProgress).await;
    h.server.set_game_time(1810.0);
    h.wait_for("finalizing", |s| s.state == TrackerState::Finalizing)
        .await;
    h.phase(GameflowPhase::EndOfGame).await;

    let summary = h.summary().await;
    assert_eq!(
        summary.queue_id,
        Some(420),
        "queue from the late gameflow session"
    );
    assert_eq!(summary.game_id, Some(5_123_456_789));
    assert_eq!(summary.result, Some(GameResult::Win));
    assert_eq!(labels(&summary), RANKED_WIN_LABELS);
}

#[tokio::test]
async fn degraded_mode_without_the_league_client_records_the_win() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.wait_for("armed by the game process", |s| {
        s.state == TrackerState::Armed
    })
    .await;

    h.server.set_game_time(600.0);
    h.server.set_up(true); // the Live Client API comes up: match starts
    h.wait_for_event(5).await;
    h.server.set_game_time(1810.0);

    let summary = h.summary().await;
    assert_eq!(labels(&summary), RANKED_WIN_LABELS);
    assert_eq!(
        summary.result,
        Some(GameResult::Win),
        "from Riot's GameEnd event"
    );
    assert_eq!(summary.result_source, Some(ResultSource::GameEndEvent));
    assert_eq!(
        summary.kda,
        Some(Kda {
            kills: 4,
            deaths: 2,
            assists: 2
        }),
        "final playerlist scores"
    );
    assert_eq!(summary.game_id, None);
    h.wait_for("idle", |s| s.state == TrackerState::Idle).await;
}

#[tokio::test]
async fn filtered_modes_are_not_tracked() {
    let mut config = Settings::default().tracker_config();
    config.filters = MatchFilters {
        aram: false,
        ..MatchFilters::default()
    };
    let mut h = Harness::start("aram_loss", config).await;
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(300.0);
    h.server.set_up(true);
    let status = h
        .wait_for("skipped", |s| {
            s.current.as_ref().is_some_and(|c| !c.tracking)
        })
        .await;
    assert_eq!(
        status.current.unwrap().skipped,
        Some(SkipReason::Filtered(
            downplayed_lib::tracker::session::ModeClass::Aram
        ))
    );

    h.server.set_up(false);
    h.send(RuntimeEvent::GameProcess(false)).await;
    h.wait_for("idle", |s| s.state == TrackerState::Idle).await;
    assert!(
        h.summaries.try_recv().is_err(),
        "a skipped match produces no summary"
    );
}

#[tokio::test]
async fn spectating_is_never_recorded() {
    let mut h = Harness::start("spectator", Settings::default().tracker_config()).await;
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(260.0);
    h.server.set_up(true);
    let status = h
        .wait_for("spectator detected", |s| {
            s.current
                .as_ref()
                .is_some_and(|c| c.skipped == Some(SkipReason::Spectator))
        })
        .await;
    assert_eq!(status.current.unwrap().markers, 0);
    h.send(RuntimeEvent::GameProcess(false)).await;
    h.wait_for("idle", |s| s.state == TrackerState::Idle).await;
    assert!(h.summaries.try_recv().is_err());
}

#[tokio::test]
async fn aram_loss_has_no_victory_and_records_the_loss() {
    let mut h = Harness::start("aram_loss", Settings::default().tracker_config()).await;
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(900.0);
    h.server.set_up(true);
    let summary = h.summary().await;
    assert_eq!(summary.result, Some(GameResult::Lose));
    assert!(!labels(&summary).contains(&"victory"));
    assert!(labels(&summary).contains(&"triple_kill"));
}
