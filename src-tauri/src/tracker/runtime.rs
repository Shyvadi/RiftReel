//! Async orchestration. League client and process signals drive the state machine;
//! during a match, a [`MatchSession`] is fed from the Live Client Data API and, when
//! capture is on, a [`CaptureController`] records it.
//!
//! [`TrackerRuntime::run`] consumes [`RuntimeEvent`]s. In the app they come from
//! [`watch_league`] (process scan + LCU WebSocket); tests inject them directly.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, MissedTickBehavior};

use super::clock::SAMPLE_INTERVAL;
use super::machine::{Effect, EndReason, Machine, Signal, TrackerState};
use super::session::{MatchFilters, MatchProgress, MatchSession, MatchSummary, SessionConfig};
use crate::capture::{CaptureController, CaptureStatus, StartRequest};
use crate::league::client::{
    credentials_for, scan_processes, ClientProcess, LcuClient, LcuError, LeagueClient,
    PhaseSubscription,
};
use crate::league::live::{LiveClient, LiveError, EVENT_POLL_INTERVAL};
use crate::league::models::GameflowPhase;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackerConfig {
    pub session: SessionConfig,
    pub filters: MatchFilters,
}

#[derive(Debug, Clone)]
pub struct Timings {
    /// `eventdata` poll interval in game (500 ms).
    pub event_poll: Duration,
    /// `gamestats` clock sample interval (10 s).
    pub clock_sample: Duration,
    /// How often an armed tracker checks whether the Live Client API is up.
    pub live_probe: Duration,
    /// How long to wait for the client's end-of-game screen before summarising without
    /// its stats.
    pub finalize_wait: Duration,
    /// Recording continues this long after Riot's `GameEnd` (the nexus falling), for
    /// the victory or defeat screen.
    pub end_screen_tail: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            event_poll: EVENT_POLL_INTERVAL,
            clock_sample: SAMPLE_INTERVAL,
            live_probe: Duration::from_secs(1),
            finalize_wait: Duration::from_secs(90),
            end_screen_tail: Duration::from_secs(3),
        }
    }
}

/// Input to the runtime.
pub enum RuntimeEvent {
    LcuConnected(Arc<dyn LeagueClient>),
    LcuDisconnected,
    Phase(GameflowPhase),
    /// `League of Legends.exe` started (`true`) or exited (`false`).
    GameProcess(bool),
}

impl fmt::Debug for RuntimeEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LcuConnected(_) => f.write_str("LcuConnected"),
            Self::LcuDisconnected => f.write_str("LcuDisconnected"),
            Self::Phase(phase) => write!(f, "Phase({phase})"),
            Self::GameProcess(running) => write!(f, "GameProcess({running})"),
        }
    }
}

/// Snapshot for the tray and UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerStatus {
    pub state: TrackerState,
    pub lcu_connected: bool,
    pub phase: Option<String>,
    pub reconnecting: bool,
    pub current: Option<MatchProgress>,
    pub last_match: Option<MatchSummary>,
    /// Video capture, when the runtime has a capture controller.
    pub capture: Option<CaptureStatus>,
}

pub struct TrackerRuntime {
    config: TrackerConfig,
    live: LiveClient,
    timings: Timings,
    machine: Machine,
    lcu: Option<Arc<dyn LeagueClient>>,
    session: Option<MatchSession>,
    /// Signals raised while handling something else; drained by the main loop.
    pending: VecDeque<Signal>,
    end_reason: Option<EndReason>,
    finalize_deadline: Option<time::Instant>,
    last_live_error: Option<String>,
    status: watch::Sender<TrackerStatus>,
    summaries: Option<mpsc::UnboundedSender<MatchSummary>>,
    last_match: Option<MatchSummary>,
    capture: Option<CaptureController>,
    /// The capture controller's latest status, for [`TrackerStatus`].
    capture_status: Option<CaptureStatus>,
}

impl TrackerRuntime {
    pub fn new(
        config: TrackerConfig,
        live: LiveClient,
        timings: Timings,
        status: watch::Sender<TrackerStatus>,
    ) -> Self {
        Self {
            config,
            live,
            timings,
            machine: Machine::new(),
            lcu: None,
            session: None,
            pending: VecDeque::new(),
            end_reason: None,
            finalize_deadline: None,
            last_live_error: None,
            status,
            summaries: None,
            last_match: None,
            capture: None,
            capture_status: None,
        }
    }

    /// Also deliver each finished match summary on a channel.
    pub fn with_summaries(mut self, summaries: mpsc::UnboundedSender<MatchSummary>) -> Self {
        self.summaries = Some(summaries);
        self
    }

    /// Record each tracked match with this controller.
    pub fn with_capture(mut self, capture: CaptureController) -> Self {
        self.capture_status = Some(capture.status().borrow().clone());
        self.capture = Some(capture);
        self
    }

    /// Runs until the event channel closes.
    pub async fn run(mut self, mut events: mpsc::Receiver<RuntimeEvent>) {
        let mut poll = ticker(self.timings.event_poll);
        let mut clock = ticker(self.timings.clock_sample);
        let mut probe = ticker(self.timings.live_probe);
        let mut capture_status = self.capture.as_ref().map(CaptureController::status);
        self.publish();
        loop {
            let deadline = self.finalize_deadline;
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => self.on_event(event).await,
                    None => break,
                },
                _ = poll.tick(), if self.polling() => self.poll().await,
                _ = clock.tick(), if self.polling() => self.sample_clock().await,
                _ = probe.tick(), if self.machine.state() == TrackerState::Armed => self.probe().await,
                _ = time::sleep_until(deadline.unwrap_or_else(time::Instant::now)), if deadline.is_some() => {
                    tracing::debug!("no end-of-game screen in time; summarising without it");
                    self.complete_finalize().await;
                }
                status = next_status(&mut capture_status) => self.capture_status = Some(status),
            }
            while let Some(signal) = self.pending.pop_front() {
                self.signal(signal).await;
            }
            self.sync_capture();
            self.publish();
        }
        tracing::debug!("tracker runtime stopped");
    }

    fn polling(&self) -> bool {
        self.machine.state() == TrackerState::InGame
            && self.session.as_ref().is_some_and(MatchSession::is_tracking)
    }

    async fn on_event(&mut self, event: RuntimeEvent) {
        let signal = match event {
            RuntimeEvent::LcuConnected(client) => {
                tracing::info!("League client connected");
                self.lcu = Some(client);
                Signal::LcuConnected
            }
            RuntimeEvent::LcuDisconnected => {
                tracing::info!("League client disconnected; Live Client API only");
                self.lcu = None;
                Signal::LcuDisconnected
            }
            RuntimeEvent::Phase(phase) => {
                tracing::debug!(%phase, "gameflow phase");
                Signal::Phase(phase)
            }
            RuntimeEvent::GameProcess(true) => Signal::GameProcessStarted,
            RuntimeEvent::GameProcess(false) => Signal::GameProcessExited,
        };
        let phase = match &signal {
            Signal::Phase(phase) => Some(phase.clone()),
            _ => None,
        };
        self.signal(signal).await;

        // The client can turn up mid-match (RiftReel started during a game, or the
        // client restarted): pick up the queue and mode it knows.
        if self.machine.state() == TrackerState::InGame {
            self.attach_lcu_session().await;
        }

        // Waiting for end-of-game stats: the client's post-game screens tell us when.
        if let (Some(phase), Some(_)) = (phase, self.finalize_deadline) {
            match phase {
                GameflowPhase::WaitingForStats => {}
                GameflowPhase::PreEndOfGame => {
                    if self.fetch_eog().await {
                        self.complete_finalize().await;
                    }
                }
                _ => self.complete_finalize().await,
            }
        }
    }

    async fn signal(&mut self, signal: Signal) {
        let before = (self.machine.state(), self.machine.is_reconnecting());
        let effects = self.machine.handle(signal);
        let after = (self.machine.state(), self.machine.is_reconnecting());
        if before.0 != after.0 {
            tracing::info!(from = ?before.0, to = ?after.0, "tracker state");
        }
        if before != after {
            // The effects below can wait on network I/O; publish the new state now
            // rather than after them.
            self.publish();
        }
        for effect in effects {
            match effect {
                Effect::PrewarmCapture => {
                    if let Some(capture) = &self.capture {
                        capture.prewarm();
                    }
                }
                Effect::StartMatch => self.start_match().await,
                Effect::ResumeMatch => {
                    tracing::info!("back in the match after a reconnect");
                    self.poll().await;
                }
                Effect::Finalize(reason) => self.begin_finalize(reason).await,
            }
        }
    }

    async fn start_match(&mut self) {
        self.session = Some(MatchSession::new(self.config.session.clone()));
        self.last_live_error = None;
        self.attach_lcu_session().await;
        self.poll().await;
        self.start_capture();
    }

    /// Starts recording the new match unless it is already known not to be tracked.
    /// The controller waits for League's window itself.
    fn start_capture(&mut self) {
        let (Some(capture), Some(session)) = (self.capture.as_mut(), self.session.as_ref()) else {
            return;
        };
        if !session.is_tracking() {
            return;
        }
        let started = capture.start(StartRequest {
            started_wall_ms: session.started_wall_ms(),
            queue: session.queue().map(str::to_owned),
        });
        if !started && capture.is_enabled() {
            tracing::debug!("a recording is still running; not starting another");
        }
    }

    /// A match found not to be tracked after all (filtered mode, spectating) isn't
    /// recorded either.
    fn sync_capture(&mut self) {
        let (Some(capture), Some(session)) = (self.capture.as_mut(), self.session.as_ref()) else {
            return;
        };
        if !session.is_tracking() && capture.discard() {
            tracing::debug!("match not tracked; dropping its recording");
        }
    }

    /// Adds the client's gameflow session (queue, mode, game ID) to the current match
    /// if it doesn't have it yet, and applies the mode filters.
    async fn attach_lcu_session(&mut self) {
        let (Some(lcu), Some(session)) = (&self.lcu, self.session.as_mut()) else {
            return;
        };
        if session.has_lcu_session() {
            return;
        }
        match lcu.gameflow_session().await {
            Ok(gameflow) => {
                session.set_lcu_session(gameflow);
                if let Some(reason) = session.apply_filters(&self.config.filters) {
                    tracing::info!(?reason, "match not tracked");
                }
            }
            Err(e) => tracing::debug!(error = %e, "gameflow session unavailable"),
        }
    }

    /// Bootstraps the session (mode, identity, roster) and then reads `eventdata`.
    async fn poll(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !session.is_tracking() {
            return;
        }
        let result = if session.has_identity() {
            match self.live.event_data().await {
                Ok(data) => {
                    let outcome = session.ingest(data.events);
                    if outcome.game_ended {
                        self.pending.push_back(Signal::GameEnded);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            bootstrap(&self.live, session, &self.config.filters).await
        };
        self.note_live_result(result);
    }

    async fn sample_clock(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let sent = Instant::now();
        let result = match self.live.game_stats().await {
            Ok(stats) => {
                session.record_clock(stats.game_time, sent, Instant::now());
                session.set_game_stats(&stats);
                Ok(())
            }
            Err(e) => Err(e),
        };
        if session.has_identity() && session.needs_roster() {
            if let Ok(players) = self.live.player_list().await {
                session.set_players(players);
            }
        }
        self.note_live_result(result);
    }

    async fn probe(&mut self) {
        if self.live.game_stats().await.is_ok() {
            self.pending.push_back(Signal::LiveApiUp);
        }
    }

    fn note_live_result(&mut self, result: Result<(), LiveError>) {
        match result {
            Ok(()) => self.last_live_error = None,
            Err(error) => {
                let text = error.to_string();
                if self.last_live_error.as_deref() != Some(text.as_str()) {
                    if error.is_unavailable() {
                        tracing::debug!(error = %text, "Live Client API unavailable (loading, reconnecting or closed)");
                    } else {
                        tracing::warn!(error = %text, "Live Client API error");
                    }
                    self.last_live_error = Some(text);
                }
            }
        }
    }

    async fn begin_finalize(&mut self, reason: EndReason) {
        self.end_reason = Some(reason);
        if let Some(capture) = self.capture.as_mut() {
            // GameEnd arrives as the nexus falls: keep its end screen.
            let tail = if reason == EndReason::GameEnd {
                self.timings.end_screen_tail
            } else {
                Duration::ZERO
            };
            capture.stop(tail);
        }
        let tracked = self
            .session
            .as_ref()
            .is_some_and(|s| s.is_tracking() && s.has_identity());
        if tracked {
            // Last chance to read the game before it closes: GameEnd and final scores.
            if let Some(session) = self.session.as_mut() {
                if reason != EndReason::GameEnd {
                    if let Ok(data) = self.live.event_data().await {
                        session.ingest(data.events);
                    }
                }
                if let Ok(players) = self.live.player_list().await {
                    session.set_final_players(players);
                }
            }
        }

        if tracked && self.lcu.is_some() {
            let phase = self.machine.last_phase().cloned();
            let ready = matches!(
                phase,
                Some(GameflowPhase::PreEndOfGame | GameflowPhase::EndOfGame)
            );
            // Stats in hand, or the end-of-game screen is already up (waiting won't help).
            if (ready && self.fetch_eog().await) || phase == Some(GameflowPhase::EndOfGame) {
                self.complete_finalize().await;
            } else {
                tracing::debug!("waiting for the end-of-game screen");
                self.finalize_deadline = Some(time::Instant::now() + self.timings.finalize_wait);
            }
        } else {
            self.complete_finalize().await;
        }
    }

    async fn fetch_eog(&mut self) -> bool {
        let (Some(lcu), Some(session)) = (&self.lcu, self.session.as_mut()) else {
            return false;
        };
        if session.has_eog() {
            return true;
        }
        match lcu.eog_stats_block().await {
            Ok(eog) => {
                tracing::debug!(win = ?eog.win(), kda_found = eog.kda().is_some(), "end-of-game stats");
                session.set_eog(eog);
                true
            }
            Err(e) => {
                tracing::debug!(error = %e, "end-of-game stats not available yet");
                false
            }
        }
    }

    async fn complete_finalize(&mut self) {
        self.finalize_deadline = None;
        let reason = self.end_reason.take().unwrap_or(EndReason::Aborted);
        let tracked = self
            .session
            .as_ref()
            .is_some_and(|s| s.is_tracking() && s.has_identity());
        if tracked {
            self.fetch_eog().await;
        }
        // The video is finished first: marker times count from its first frame. A match
        // with no summary keeps no video.
        let recording = match self.capture.as_mut() {
            Some(capture) => capture.finish(tracked).await,
            None => None,
        };
        if let Some(mut session) = self.session.take() {
            if tracked {
                if let Some(recording) = recording {
                    session.set_recording(recording);
                }
                let summary = session.summary(reason);
                tracing::info!(
                    result = ?summary.result,
                    source = ?summary.result_source,
                    markers = summary.markers.len(),
                    moments = summary.moments.len(),
                    ended = ?reason,
                    "match finished"
                );
                if let Some(summaries) = &self.summaries {
                    let _ = summaries.send(summary.clone());
                }
                self.last_match = Some(summary);
            } else {
                tracing::info!(skipped = ?session.skip_reason(), "match ended (not tracked)");
            }
        }
        self.pending.push_back(Signal::Finalized);
    }

    fn publish(&self) {
        let status = TrackerStatus {
            state: self.machine.state(),
            lcu_connected: self.lcu.is_some(),
            phase: self.machine.last_phase().map(|p| p.as_str().to_owned()),
            reconnecting: self.machine.is_reconnecting(),
            current: self.session.as_ref().map(MatchSession::progress),
            last_match: self.last_match.clone(),
            capture: self.capture_status.clone(),
        };
        self.status.send_if_modified(|current| {
            if *current == status {
                false
            } else {
                *current = status;
                true
            }
        });
    }
}

/// First contact with a match: mode (for filters), clock, "me" and the roster.
async fn bootstrap(
    live: &LiveClient,
    session: &mut MatchSession,
    filters: &MatchFilters,
) -> Result<(), LiveError> {
    let sent = Instant::now();
    let stats = live.game_stats().await?;
    session.record_clock(stats.game_time, sent, Instant::now());
    session.set_game_stats(&stats);
    if let Some(reason) = session.apply_filters(filters) {
        tracing::info!(?reason, "match not tracked");
        return Ok(());
    }
    match live.active_player().await {
        Ok(player) => session.set_identity(&player),
        // The game answers but there's no active player: spectator or replay.
        Err(LiveError::Status { .. }) => {
            if session.note_missing_active_player() {
                tracing::info!("no active player (spectator or replay); not tracking this game");
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    }
    match live.player_list().await {
        Ok(players) => session.set_players(players),
        Err(e) => tracing::debug!(error = %e, "playerlist not available yet"),
    }
    tracing::info!(
        mode = session.game_mode(),
        champion = session.champion(),
        "tracking match"
    );
    Ok(())
}

/// The capture controller's next status; never resolves without a controller.
async fn next_status(status: &mut Option<watch::Receiver<CaptureStatus>>) -> CaptureStatus {
    if let Some(receiver) = status {
        if receiver.changed().await.is_ok() {
            return receiver.borrow_and_update().clone();
        }
    }
    std::future::pending().await
}

fn ticker(period: Duration) -> time::Interval {
    let mut interval = time::interval(period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// Watches for the League client and the game: scans the process table every
/// `scan_every`, reports the game process starting/stopping, and keeps a WebSocket
/// subscription to the client's gameflow phase while it runs. With `use_lcu = false`
/// only the game process is watched (degraded mode).
pub async fn watch_league(events: mpsc::Sender<RuntimeEvent>, use_lcu: bool, scan_every: Duration) {
    let mut scan = ticker(scan_every);
    let mut game_running: Option<bool> = None;
    let mut subscription: Option<PhaseSubscription> = None;
    loop {
        let mut outgoing = Vec::new();
        tokio::select! {
            _ = scan.tick() => {
                // While the client's WebSocket is up its phases say everything; the
                // process table only matters for finding it or for degraded mode.
                if subscription.is_some() {
                    continue;
                }
                let Ok(found) = tokio::task::spawn_blocking(scan_processes).await else {
                    continue;
                };
                // The client first: once it's connected its phases drive the tracker, and
                // the game process alone no longer starts a degraded-mode match.
                if let Some(process) = found.client.filter(|_| use_lcu && subscription.is_none()) {
                    match connect_client(&process).await {
                        Ok((client, phases, phase)) => {
                            subscription = Some(phases);
                            outgoing.push(RuntimeEvent::LcuConnected(client));
                            outgoing.push(RuntimeEvent::Phase(phase));
                        }
                        Err(e) => tracing::debug!(error = %e, "League client not ready"),
                    }
                }
                if game_running != Some(found.game_running) {
                    game_running = Some(found.game_running);
                    outgoing.push(RuntimeEvent::GameProcess(found.game_running));
                }
            }
            phase = next_phase(&mut subscription) => match phase {
                Some(phase) => outgoing.push(RuntimeEvent::Phase(phase)),
                None => {
                    subscription = None;
                    outgoing.push(RuntimeEvent::LcuDisconnected);
                }
            },
        }
        for event in outgoing {
            if events.send(event).await.is_err() {
                return;
            }
        }
    }
}

async fn next_phase(subscription: &mut Option<PhaseSubscription>) -> Option<GameflowPhase> {
    match subscription {
        Some(phases) => phases.next().await,
        None => std::future::pending().await,
    }
}

async fn connect_client(
    process: &ClientProcess,
) -> Result<(Arc<dyn LeagueClient>, PhaseSubscription, GameflowPhase), LcuError> {
    let client = LcuClient::new(credentials_for(process)?)?;
    // Subscribe first so no change slips in between reading and subscribing.
    let phases = client.subscribe_gameflow_phase().await?;
    let phase = client.get_gameflow_phase().await?;
    Ok((Arc::new(client), phases, phase))
}
