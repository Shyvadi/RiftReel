//! The tracker runtime end to end: League client phases (in-process fake) plus a fake
//! Live Client API over HTTPS, through a crash/reconnect to a recorded win, and the
//! capture controller driven by the match (with a scripted ffmpeg).

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::sync::{mpsc, oneshot, watch};

use downplayed_lib::capture::audio::{AudioRun, SourceConfig};
use downplayed_lib::capture::controller::{CaptureBackend, CaptureTimings};
use downplayed_lib::capture::display::Display;
use downplayed_lib::capture::ffmpeg::{CapturePlan, CaptureRun, ExitReport, Progress};
use downplayed_lib::capture::{CaptureConfig, CaptureController, CaptureState};
use downplayed_lib::league::client::{LcuError, LeagueClient};
use downplayed_lib::league::live::LiveClient;
use downplayed_lib::league::models::{EogStatsBlock, GameflowPhase, GameflowSession};
use downplayed_lib::library::Library;
use downplayed_lib::settings::Settings;
use downplayed_lib::tracker::machine::{EndReason, TrackerState};
use downplayed_lib::tracker::normalize::{GameResult, Kda};
use downplayed_lib::tracker::runtime::{
    RuntimeEvent, Timings, TrackerConfig, TrackerRuntime, TrackerStatus,
};
use downplayed_lib::tracker::session::{MatchFilters, MatchSummary, ResultSource, SkipReason};
use support::{eventually, load_fixture, load_json, FakeLiveServer, TestPki};

const WAIT: Duration = Duration::from_secs(10);

/// League client double: the gameflow session is always there, end-of-game stats only
/// once `eog_ready` is set (the client publishes them on its end-of-game screen).
struct FakeLcu {
    session: GameflowSession,
    eog: EogStatsBlock,
    eog_ready: AtomicBool,
    eog_requests: AtomicUsize,
}

impl FakeLcu {
    fn ranked_win() -> Arc<Self> {
        Arc::new(Self {
            session: serde_json::from_value(load_json("lol/lcu/ranked_win.session.json")).unwrap(),
            eog: serde_json::from_value(load_json("lol/lcu/ranked_win.eog.json")).unwrap(),
            eog_ready: AtomicBool::new(false),
            eog_requests: AtomicUsize::new(0),
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
            self.eog_requests.fetch_add(1, Ordering::SeqCst);
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
        Self::start_with(fixture, config, None).await
    }

    async fn start_with(
        fixture: &str,
        config: TrackerConfig,
        capture: Option<CaptureController>,
    ) -> Self {
        let pki = TestPki::new();
        let server = FakeLiveServer::start(&pki, load_fixture(fixture)).await;
        server.set_up(false); // no game yet
        let live = LiveClient::with_tls(server.url(), pki.client_config()).unwrap();
        let timings = Timings {
            event_poll: Duration::from_millis(20),
            clock_sample: Duration::from_millis(50),
            live_probe: Duration::from_millis(20),
            finalize_wait: Duration::from_secs(5),
            end_screen_tail: Duration::from_millis(20),
        };
        let (status_tx, status) = watch::channel(TrackerStatus::default());
        let (summary_tx, summaries) = mpsc::unbounded_channel();
        let (events, events_rx) = mpsc::channel(16);
        let mut runtime =
            TrackerRuntime::new(config, live, timings, status_tx).with_summaries(summary_tx);
        if let Some(capture) = capture {
            runtime = runtime.with_capture(capture);
        }
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

    async fn wait_for_event(&mut self, id: u64) -> TrackerStatus {
        self.wait_for(&format!("EventID {id}"), |s| {
            s.current.as_ref().and_then(|c| c.last_event_id) == Some(id)
        })
        .await
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

fn assert_no_duplicate_events(summary: &MatchSummary) {
    let mut seen = HashSet::new();
    assert!(
        summary
            .markers
            .iter()
            .flat_map(|m| &m.riot_event_ids)
            .all(|id| seen.insert(*id)),
        "a Riot event produced two markers"
    );
}

/// Champ select, into the game, then a crash at 1000 s with the client showing
/// Reconnect and the game's API down.
async fn play_until_crash(h: &mut Harness) -> Arc<FakeLcu> {
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
    h.wait_for_event(12).await;

    // Crash: the game dies and the client shows Reconnect.
    h.server.set_up(false);
    h.phase(GameflowPhase::Reconnect).await;
    h.wait_for("reconnecting", |s| {
        s.reconnecting && s.state == TrackerState::InGame
    })
    .await;
    lcu
}

/// Nexus down at 1810 s, then the client's end-of-game screens. The stats aren't
/// published when PreEndOfGame asks for them, so the runtime has to keep waiting
/// until EndOfGame.
async fn finish_with_end_of_game_stats(h: &mut Harness, lcu: &FakeLcu) -> MatchSummary {
    h.server.set_game_time(1810.0);
    h.wait_for("finalizing", |s| s.state == TrackerState::Finalizing)
        .await;
    h.phase(GameflowPhase::WaitingForStats).await;
    h.phase(GameflowPhase::PreEndOfGame).await;
    assert!(
        eventually(WAIT, || lcu.eog_requests.load(Ordering::SeqCst) >= 1).await,
        "end-of-game stats requested at PreEndOfGame"
    );
    assert_eq!(
        h.status.borrow().state,
        TrackerState::Finalizing,
        "no stats yet: still waiting"
    );
    lcu.eog_ready.store(true, Ordering::SeqCst);
    h.server.set_up(false); // the game closes
    h.phase(GameflowPhase::EndOfGame).await;
    h.summary().await
}

#[tokio::test]
async fn ranked_win_through_a_crash_and_reconnect() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    let lcu = play_until_crash(&mut h).await;
    tokio::time::sleep(Duration::from_millis(100)).await; // polls fail while the game is down

    // The player reconnects: the client reports InProgress while the restarted game is
    // still loading, so the immediate re-read finds nothing yet...
    h.phase(GameflowPhase::InProgress).await;
    h.wait_for("reconnected", |s| {
        !s.reconnecting && s.state == TrackerState::InGame
    })
    .await;
    // ...then its API answers and events pick up after the cursor.
    h.server.set_game_time(1300.0);
    h.server.set_up(true);
    h.wait_for_event(16).await;

    let summary = finish_with_end_of_game_stats(&mut h, &lcu).await;
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
    assert_no_duplicate_events(&summary);
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
    assert!(!status.reconnecting);
}

#[tokio::test]
async fn events_may_resume_before_the_client_reports_the_reconnect() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    let lcu = play_until_crash(&mut h).await;

    // The restarted game's API answers while the client still says Reconnect. Polling
    // carries on and picks up after the cursor.
    h.server.set_game_time(1300.0);
    h.server.set_up(true);
    let status = h.wait_for_event(16).await;
    assert!(
        status.reconnecting,
        "the client hasn't reported InProgress yet"
    );
    // The client catches up.
    h.phase(GameflowPhase::InProgress).await;
    h.wait_for("reconnected", |s| {
        !s.reconnecting && s.state == TrackerState::InGame
    })
    .await;

    let summary = finish_with_end_of_game_stats(&mut h, &lcu).await;
    assert_eq!(labels(&summary), RANKED_WIN_LABELS);
    assert_eq!(summary.result, Some(GameResult::Win));
    assert_no_duplicate_events(&summary);
}

#[tokio::test]
async fn league_client_appearing_mid_match_adds_its_details() {
    let mut h = Harness::start("ranked_win", Settings::default().tracker_config()).await;
    // Downplayed started mid-game, before it reached the client: degraded start.
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(600.0);
    h.server.set_up(true);
    h.wait_for_event(6).await;

    let lcu = FakeLcu::ranked_win();
    lcu.eog_ready.store(true, Ordering::SeqCst);
    h.send(RuntimeEvent::LcuConnected(lcu)).await;
    h.phase(GameflowPhase::InProgress).await;
    // Let the runtime attach the client before the game ends.
    h.wait_for("client attached", |s| {
        s.lcu_connected && s.phase.as_deref() == Some("InProgress")
    })
    .await;
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
    h.wait_for_event(6).await;
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

/// A scripted ffmpeg: a frame every 5 ms, timestamped from its own start, until it is
/// asked to stop.
struct FakeCapture {
    plans: Mutex<Vec<CapturePlan>>,
    encoder_asks: AtomicUsize,
    remuxed: AtomicUsize,
}

impl FakeCapture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(Vec::new()),
            encoder_asks: AtomicUsize::new(0),
            remuxed: AtomicUsize::new(0),
        })
    }
}

impl CaptureBackend for FakeCapture {
    fn game_display(&self) -> Option<Display> {
        Some(Display {
            adapter: 0,
            output: 0,
            adapter_name: "Test GPU".into(),
            device_name: r"\\.\DISPLAY1".into(),
            width: 1920,
            height: 1080,
        })
    }

    fn primary_display(&self) -> Option<Display> {
        self.game_display()
    }

    fn encoder(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.encoder_asks.fetch_add(1, Ordering::SeqCst);
            Ok("h264_nvenc".into())
        })
    }

    fn forget_encoder(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn start(&self, plan: CapturePlan) -> BoxFuture<'_, Result<CaptureRun, String>> {
        Box::pin(async move {
            std::fs::write(&plan.file, b"video").map_err(|e| e.to_string())?;
            self.plans.lock().unwrap().push(plan);
            let (progress_tx, progress) = mpsc::channel(64);
            let (stop, mut stop_rx) = oneshot::channel::<()>();
            let (exited_tx, exited) = oneshot::channel();
            tokio::spawn(async move {
                let began = Instant::now();
                let mut frame = 0;
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        _ = tokio::time::sleep(Duration::from_millis(5)) => {
                            frame += 1;
                            let report = Progress {
                                frame,
                                out_time: Some(began.elapsed()),
                                ..Progress::default()
                            };
                            let _ = progress_tx.try_send((Instant::now(), report));
                        }
                    }
                }
                let _ = exited_tx.send(ExitReport {
                    success: true,
                    detail: None,
                });
            });
            Ok(CaptureRun {
                progress,
                stop,
                exited,
            })
        })
    }

    fn start_audio(&self, _sources: Vec<SourceConfig>) -> BoxFuture<'_, Result<AudioRun, String>> {
        Box::pin(async { Err("no audio in this test".to_owned()) })
    }

    fn finish_file(
        &self,
        _file: PathBuf,
        _audio_lead_s: Option<f64>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.remuxed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn free_space(&self, _dir: &Path) -> Option<u64> {
        None
    }
}

fn capture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "downplayed-e2e-capture-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fake_controller(dir: &std::path::Path, backend: Arc<FakeCapture>) -> CaptureController {
    let config = CaptureConfig {
        enabled: true,
        fps: 60,
        bitrate_kbps: 10_000,
        encoder: None,
        audio: Vec::new(),
        ffmpeg: None,
        matches_dir: dir.join("matches"),
        probe_file: dir.join("capture-probe.json"),
        min_free_bytes: 0,
        stop_free_bytes: 0,
    };
    CaptureController::new(config, backend).with_timings(CaptureTimings {
        window_poll: Duration::from_millis(5),
        window_wait: Duration::from_secs(5),
        retry_after: Duration::from_millis(5),
        disk_check: Duration::from_millis(5),
    })
}

fn capture_state(status: &TrackerStatus) -> Option<CaptureState> {
    status.capture.as_ref().map(|c| c.state)
}

#[tokio::test]
async fn the_match_is_recorded_and_saved_with_its_video() {
    let dir = capture_dir("recorded");
    let backend = FakeCapture::new();
    let mut h = Harness::start_with(
        "ranked_win",
        Settings::default().tracker_config(),
        Some(fake_controller(&dir, backend.clone())),
    )
    .await;
    h.wait_for("recorder ready", |s| {
        capture_state(s) == Some(CaptureState::Idle)
    })
    .await;

    let lcu = FakeLcu::ranked_win();
    h.send(RuntimeEvent::LcuConnected(lcu.clone())).await;
    h.phase(GameflowPhase::ChampSelect).await;
    h.wait_for("champ select", |s| s.state == TrackerState::Armed)
        .await;
    assert!(
        eventually(WAIT, || backend.encoder_asks.load(Ordering::SeqCst) >= 1).await,
        "champ select finds the encoder"
    );
    assert!(
        backend.plans.lock().unwrap().is_empty(),
        "nothing is captured in champ select"
    );

    h.server.set_game_time(20.0);
    h.server.set_up(true);
    h.phase(GameflowPhase::InProgress).await;
    h.wait_for("recording", |s| {
        capture_state(s) == Some(CaptureState::Recording)
    })
    .await;
    h.server.set_game_time(600.0);
    h.wait_for_event(6).await;
    let summary = finish_with_end_of_game_stats(&mut h, &lcu).await;

    let recording = summary.recording.clone().expect("the match was recorded");
    assert_eq!(recording.file, "match.mp4");
    assert!(recording.frames > 0 && recording.error.is_none());
    assert_eq!(
        (
            recording.width,
            recording.height,
            recording.encoder.as_str()
        ),
        (1920, 1080, "h264_nvenc")
    );
    assert!(recording.game_time_at_start_s.is_some());
    let recorded_as = recording.folder.file_name().unwrap().to_string_lossy();
    assert!(
        recorded_as.ends_with("_Ranked-Solo-Duo"),
        "named when recording began (no champion yet): {recorded_as}"
    );
    assert!(summary.markers.iter().all(|m| m.video_time_s.is_some()));
    assert_eq!(backend.plans.lock().unwrap().len(), 1);
    assert_eq!(backend.remuxed.load(Ordering::SeqCst), 1);
    h.wait_for("idle, recorder ready", |s| {
        s.state == TrackerState::Idle && capture_state(s) == Some(CaptureState::Idle)
    })
    .await;

    // The library keeps the folder and its video, renamed now the champion is known.
    let library = Library::new(
        dir.join("matches"),
        dir.join("recordings"),
        Settings::default().tracker_config().session,
    );
    let id = library.save(&summary).unwrap();
    assert!(id.ends_with("_Ranked-Solo-Duo_Shyvana"), "{id}");
    assert!(!recording.folder.exists(), "renamed, not copied");
    let review = library.get(&id).unwrap();
    assert_eq!(
        PathBuf::from(review["videoPath"].as_str().unwrap()),
        dir.join("matches").join(&id).join("match.mp4")
    );
    assert_eq!(review["recording"]["file"], "match.mp4");
    assert!(review["recording"].get("folder").is_none());
    assert!(library.list()[0].video);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_spectated_game_keeps_no_recording() {
    let dir = capture_dir("spectator");
    let backend = FakeCapture::new();
    let mut h = Harness::start_with(
        "spectator",
        Settings::default().tracker_config(),
        Some(fake_controller(&dir, backend.clone())),
    )
    .await;
    h.send(RuntimeEvent::GameProcess(true)).await;
    h.server.set_game_time(260.0);
    h.server.set_up(true);
    h.wait_for("spectator detected", |s| {
        s.current
            .as_ref()
            .is_some_and(|c| c.skipped == Some(SkipReason::Spectator))
    })
    .await;
    h.wait_for("recording dropped", |s| {
        capture_state(s) == Some(CaptureState::Idle)
    })
    .await;
    h.send(RuntimeEvent::GameProcess(false)).await;
    h.wait_for("idle", |s| s.state == TrackerState::Idle).await;
    assert!(h.summaries.try_recv().is_err());
    assert!(
        !backend.plans.lock().unwrap().is_empty(),
        "recording began before the spectator was confirmed"
    );
    let left: Vec<_> = std::fs::read_dir(dir.join("matches"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the recording was deleted: {left:?}");
    std::fs::remove_dir_all(&dir).ok();
}
