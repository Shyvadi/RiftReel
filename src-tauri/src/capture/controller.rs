//! One recording per match (doc 03 §6, `full_match`).
//!
//! The tracker runtime drives the controller: [`CaptureController::start`] when a match
//! starts, [`CaptureController::stop`] when it ends, [`CaptureController::discard`] when it
//! turns out not to be tracked (filtered mode, spectating), and
//! [`CaptureController::finish`] before the match summary is written. A recording task
//! does the waiting: for League's game window (to pick its monitor), for the encoder
//! probe, for ffmpeg, and for the remux that turns the crash-safe fragmented file into a
//! regular MP4 a player opens instantly.
//!
//! ```text
//! start ──► Starting (window? encoder?) ──► Recording ──stop──► Saving (remux) ──► Idle
//!              │ stop/discard: nothing recorded    │ discard: file deleted
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use serde::Serialize;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use super::display::{self, Display};
use super::ffmpeg::{
    self, CapturePlan, CaptureRun, EncoderFamily, Priority, Progress, StartEstimate,
};
use super::probe;
use crate::library;

/// The `[capture]` settings in effect, plus where things go.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureConfig {
    pub enabled: bool,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// An ffmpeg encoder name; `None` uses the probe's choice.
    pub encoder: Option<String>,
    /// The ffmpeg to run; `None` finds the bundled one.
    pub ffmpeg: Option<PathBuf>,
    /// Match folders (`library/League of Legends`).
    pub matches_dir: PathBuf,
    /// Where the encoder probe result is stored.
    pub probe_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CaptureTimings {
    /// How often to look for League's game window.
    pub window_poll: Duration,
    /// Record the primary monitor if the game window hasn't appeared by then.
    pub window_wait: Duration,
    /// A first attempt that produced nothing is retried (with a fresh probe) after this.
    pub retry_after: Duration,
}

impl Default for CaptureTimings {
    fn default() -> Self {
        Self {
            window_poll: Duration::from_millis(500),
            window_wait: Duration::from_secs(90),
            retry_after: Duration::from_secs(2),
        }
    }
}

/// What the runtime knows when the match starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    /// The match's start; the folder is named from it, like the saved match.
    pub started_wall_ms: u64,
    /// The queue, if the League client said.
    pub queue: Option<String>,
}

/// The `recording` block of `match.json` (doc 03 §7).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    /// The video file, inside the match folder.
    pub file: String,
    /// Wall clock of video time 0 and of the last frame (Unix ms).
    pub start_wall_ms: u64,
    pub end_wall_ms: u64,
    /// Game time at video time 0; negative when recording began during the loading
    /// screen. Video time ≈ game time − this (pauses aside).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_time_at_start_s: Option<f64>,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub frames: u64,
    /// ffmpeg's counters for frames dropped or repeated to hold the frame rate. A few
    /// duplicates in the first second are the encoder starting up.
    pub dropped_frames: u64,
    pub duplicated_frames: u64,
    /// Why the recording ended before the match did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The match folder it was recorded into; the library renames it when saving.
    #[serde(skip)]
    pub folder: PathBuf,
}

/// A finished recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// Monotonic time of video time 0, for marker alignment (`tracker::clock`).
    pub started_at: Instant,
    pub info: RecordingInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    /// Turned off in the settings.
    #[default]
    Off,
    Idle,
    /// Waiting for League's window or the encoder.
    Starting,
    Recording,
    /// Finishing the file.
    Saving,
    /// The last recording failed or ended early (see `error`).
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub state: CaptureState,
    pub encoder: Option<String>,
    pub error: Option<String>,
}

/// What a recording needs from the outside world: ffmpeg and Windows in the app, a
/// script in tests.
pub trait CaptureBackend: Send + Sync + 'static {
    /// The monitor League's game window is on; `None` while there is no window.
    fn game_display(&self) -> Option<Display>;
    fn primary_display(&self) -> Option<Display>;
    /// The encoder to record with (probing once if needed).
    fn encoder(&self) -> BoxFuture<'_, Result<String, String>>;
    /// The encoder failed in practice: probe again next time.
    fn forget_encoder(&self) -> BoxFuture<'_, ()>;
    fn start(&self, plan: CapturePlan) -> BoxFuture<'_, Result<CaptureRun, String>>;
    /// Turns the finished fragmented file into a regular MP4.
    fn finish_file(&self, file: PathBuf) -> BoxFuture<'_, Result<(), String>>;
}

/// The real backend: DXGI for the monitor, the ffmpeg sidecar for everything else.
pub struct FfmpegBackend {
    ffmpeg: Option<PathBuf>,
    encoder: Option<String>,
    probe_file: PathBuf,
    /// The probe's verdict once known. The lock also keeps a champ-select prewarm and a
    /// match start from probing twice at once.
    probed: Mutex<Option<Result<String, String>>>,
}

impl FfmpegBackend {
    pub fn new(config: &CaptureConfig) -> Self {
        Self {
            ffmpeg: ffmpeg::locate(config.ffmpeg.as_deref()),
            encoder: config.encoder.clone(),
            probe_file: config.probe_file.clone(),
            probed: Mutex::new(None),
        }
    }

    pub fn ffmpeg(&self) -> Option<&Path> {
        self.ffmpeg.as_deref()
    }

    fn binary(&self) -> Result<&Path, String> {
        self.ffmpeg.as_deref().ok_or_else(|| {
            "ffmpeg not found (run tools/fetch-ffmpeg.ps1, or set [capture] ffmpeg_path)".to_owned()
        })
    }
}

impl CaptureBackend for FfmpegBackend {
    fn game_display(&self) -> Option<Display> {
        display::game_display()
    }

    fn primary_display(&self) -> Option<Display> {
        display::primary_display()
    }

    fn encoder(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            let binary = self.binary()?;
            if let Some(encoder) = &self.encoder {
                return Ok(encoder.clone());
            }
            let mut probed = self.probed.lock().await;
            if let Some(verdict) = probed.as_ref() {
                return verdict.clone();
            }
            let verdict = match probe::cached_or_probe(binary, &self.probe_file, false).await {
                Ok(result) => result.encoder.ok_or_else(|| {
                    if result.ddagrab {
                        "no working encoder (see the encoder probe file)".to_owned()
                    } else {
                        "this ffmpeg has no ddagrab".to_owned()
                    }
                }),
                Err(e) => Err(format!("encoder probe failed: {e}")),
            };
            *probed = Some(verdict.clone());
            verdict
        })
    }

    fn forget_encoder(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            *self.probed.lock().await = None;
            let _ = tokio::fs::remove_file(&self.probe_file).await;
        })
    }

    fn start(&self, plan: CapturePlan) -> BoxFuture<'_, Result<CaptureRun, String>> {
        Box::pin(async move {
            let binary = self.binary()?;
            let priority = if EncoderFamily::of(&plan.encoder).is_hardware() {
                Priority::AboveNormal
            } else {
                Priority::BelowNormal
            };
            ffmpeg::spawn_capture(binary, &ffmpeg::capture_args(&plan), priority)
                .map_err(|e| format!("could not start ffmpeg: {e}"))
        })
    }

    fn finish_file(&self, file: PathBuf) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { ffmpeg::remux_to_mp4(self.binary()?, &file).await })
    }
}

/// What the runtime wants from the recording task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Record,
    /// Stop at this moment (a little after the match ends, for its end screen).
    Stop(tokio::time::Instant),
    /// Stop and delete the recording.
    Discard,
}

struct Active {
    commands: watch::Sender<Command>,
    task: JoinHandle<Option<Recording>>,
}

pub struct CaptureController {
    config: CaptureConfig,
    timings: CaptureTimings,
    backend: Arc<dyn CaptureBackend>,
    active: Option<Active>,
    status: Arc<watch::Sender<CaptureStatus>>,
}

impl CaptureController {
    pub fn new(config: CaptureConfig, backend: Arc<dyn CaptureBackend>) -> Self {
        let state = if config.enabled {
            CaptureState::Idle
        } else {
            CaptureState::Off
        };
        let (status, _) = watch::channel(CaptureStatus {
            state,
            ..CaptureStatus::default()
        });
        Self {
            config,
            timings: CaptureTimings::default(),
            backend,
            active: None,
            status: Arc::new(status),
        }
    }

    /// The controller the app uses: the ffmpeg sidecar.
    pub fn with_ffmpeg(config: CaptureConfig) -> Self {
        let backend = Arc::new(FfmpegBackend::new(&config));
        if config.enabled && backend.ffmpeg().is_none() {
            tracing::warn!("capture is on but ffmpeg wasn't found; matches won't be recorded (run tools/fetch-ffmpeg.ps1)");
        }
        Self::new(config, backend)
    }

    pub fn with_timings(mut self, timings: CaptureTimings) -> Self {
        self.timings = timings;
        self
    }

    pub fn status(&self) -> watch::Receiver<CaptureStatus> {
        self.status.subscribe()
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// A recording was started and not yet collected with [`finish`](Self::finish).
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Champ select: find the encoder now (a probe takes a few seconds) so the match
    /// start doesn't wait for it. Nothing is captured.
    pub fn prewarm(&self) {
        if !self.config.enabled {
            return;
        }
        let backend = self.backend.clone();
        tokio::spawn(async move {
            if let Err(error) = backend.encoder().await {
                tracing::warn!(%error, "capture unavailable");
            }
        });
    }

    /// Starts recording the match. Returns `false` when capture is off or a recording
    /// is already running.
    pub fn start(&mut self, request: StartRequest) -> bool {
        if !self.config.enabled || self.active.is_some() {
            return false;
        }
        let (commands, receiver) = watch::channel(Command::Record);
        let task = Recorder {
            backend: self.backend.clone(),
            config: self.config.clone(),
            timings: self.timings.clone(),
            status: self.status.clone(),
            commands: receiver,
        };
        self.active = Some(Active {
            commands,
            task: tokio::spawn(task.run(request)),
        });
        true
    }

    /// Ends the recording `tail` from now (the match is over; the tail keeps its end
    /// screen). Earlier stops win.
    pub fn stop(&mut self, tail: Duration) {
        if let Some(active) = &self.active {
            let at = tokio::time::Instant::now() + tail;
            active.commands.send_if_modified(|command| match *command {
                Command::Record => {
                    *command = Command::Stop(at);
                    true
                }
                Command::Stop(current) if at < current => {
                    *command = Command::Stop(at);
                    true
                }
                _ => false,
            });
        }
    }

    /// Stops and deletes the recording: the match isn't tracked after all. Returns
    /// `false` if there was nothing (new) to discard.
    pub fn discard(&mut self) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        active.commands.send_if_modified(|command| {
            let changed = *command != Command::Discard;
            *command = Command::Discard;
            changed
        })
    }

    /// Waits for the recording to be finished and returns it. With `keep = false` it is
    /// deleted instead (the match produced no summary).
    pub async fn finish(&mut self, keep: bool) -> Option<Recording> {
        let active = self.active.take()?;
        if keep {
            // End it now unless a stop (with its tail) is already under way.
            active.commands.send_if_modified(|command| {
                let record = *command == Command::Record;
                if record {
                    *command = Command::Stop(tokio::time::Instant::now());
                }
                record
            });
        } else {
            active.commands.send_replace(Command::Discard);
        }
        let recording = match active.task.await {
            Ok(recording) => recording,
            Err(e) => {
                tracing::warn!(error = %e, "recording task failed");
                None
            }
        };
        match recording {
            // It had already finished when the discard came.
            Some(recording) if !keep => {
                remove_folder(recording.info.folder).await;
                None
            }
            recording => recording,
        }
    }
}

async fn remove_folder(folder: PathBuf) {
    let shown = folder.display().to_string();
    match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(folder)).await {
        Ok(Ok(())) => tracing::debug!(folder = %shown, "recording deleted"),
        Ok(Err(e)) => tracing::warn!(folder = %shown, error = %e, "could not delete the recording"),
        Err(e) => tracing::warn!(error = %e, "could not delete the recording"),
    }
}

fn wall_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// One ffmpeg run, as it ended.
struct RunOutcome {
    estimate: StartEstimate,
    last: Progress,
    /// ffmpeg's error, if it exited without being asked to.
    error: Option<String>,
    /// It was asked to stop.
    asked: bool,
    discard: bool,
}

/// The recording task.
struct Recorder {
    backend: Arc<dyn CaptureBackend>,
    config: CaptureConfig,
    timings: CaptureTimings,
    status: Arc<watch::Sender<CaptureStatus>>,
    commands: watch::Receiver<Command>,
}

impl Recorder {
    fn set(&self, state: CaptureState, encoder: Option<&str>, error: Option<String>) {
        let status = CaptureStatus {
            state,
            encoder: encoder.map(str::to_owned),
            error,
        };
        self.status.send_if_modified(|current| {
            let changed = *current != status;
            *current = status;
            changed
        });
    }

    fn wants_out(&self) -> bool {
        *self.commands.borrow() != Command::Record
    }

    async fn run(mut self, request: StartRequest) -> Option<Recording> {
        self.set(CaptureState::Starting, None, None);
        let Some(monitor) = self.wait_for_display().await else {
            self.set(CaptureState::Idle, None, None);
            return None;
        };
        let mut retried = false;
        loop {
            let encoder = match self.backend.encoder().await {
                Ok(encoder) => encoder,
                Err(error) => {
                    tracing::warn!(%error, "not recording this match");
                    self.set(CaptureState::Failed, None, Some(error));
                    return None;
                }
            };
            if self.wants_out() {
                self.set(CaptureState::Idle, None, None);
                return None;
            }
            match self.record(&request, &monitor, &encoder).await {
                Attempt::Done(recording) => return recording,
                // Nothing came out of ffmpeg: the stored encoder may be stale (driver,
                // GPU change). Probe again and give it one more go.
                Attempt::NothingRecorded(error) if !retried && !self.wants_out() => {
                    tracing::warn!(%encoder, %error, "capture failed to start; probing again");
                    retried = true;
                    self.backend.forget_encoder().await;
                    tokio::time::sleep(self.timings.retry_after).await;
                }
                Attempt::NothingRecorded(error) => {
                    tracing::warn!(%encoder, %error, "capture failed; not recording this match");
                    self.backend.forget_encoder().await;
                    self.set(CaptureState::Failed, Some(&encoder), Some(error));
                    return None;
                }
            }
        }
    }

    /// League's game window's monitor; the primary one if the window doesn't show up in
    /// time; `None` if the match ends first.
    async fn wait_for_display(&mut self) -> Option<Display> {
        let deadline = tokio::time::Instant::now() + self.timings.window_wait;
        loop {
            if self.wants_out() {
                return None;
            }
            let backend = self.backend.clone();
            if let Ok(Some(found)) =
                tokio::task::spawn_blocking(move || backend.game_display()).await
            {
                return Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                let backend = self.backend.clone();
                let primary = tokio::task::spawn_blocking(move || backend.primary_display())
                    .await
                    .ok()
                    .flatten();
                tracing::info!(
                    found = primary.is_some(),
                    "League's window didn't appear; recording the primary monitor"
                );
                return primary;
            }
            tokio::select! {
                _ = tokio::time::sleep(self.timings.window_poll) => {}
                changed = self.commands.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    async fn record(
        &mut self,
        request: &StartRequest,
        monitor: &Display,
        encoder: &str,
    ) -> Attempt {
        let matches_dir = self.config.matches_dir.clone();
        let (started, queue) = (request.started_wall_ms, request.queue.clone());
        let reserved = tokio::task::spawn_blocking(move || {
            library::reserve_folder(&matches_dir, started, queue.as_deref())
        })
        .await;
        let folder = match reserved {
            Ok(Ok(folder)) => folder,
            Ok(Err(e)) => {
                return Attempt::NothingRecorded(format!("cannot create the match folder: {e}"))
            }
            Err(e) => return Attempt::NothingRecorded(e.to_string()),
        };
        let file = folder.join(library::VIDEO_FILE);
        let plan = CapturePlan {
            adapter: monitor.adapter,
            output: monitor.output,
            fps: self.config.fps,
            bitrate_kbps: self.config.bitrate_kbps,
            encoder: encoder.to_owned(),
            file: file.clone(),
        };
        let run = match self.backend.start(plan).await {
            Ok(run) => run,
            Err(error) => {
                remove_folder(folder).await;
                return Attempt::NothingRecorded(error);
            }
        };
        tracing::info!(
            %encoder,
            monitor = %monitor.device_name,
            gpu = %monitor.adapter_name,
            width = monitor.width,
            height = monitor.height,
            fps = self.config.fps,
            "recording"
        );
        self.set(CaptureState::Recording, Some(encoder), None);

        let outcome = self.drive(run).await;
        let ended = SystemTime::now();
        let Some(started_at) = outcome.estimate.get() else {
            remove_folder(folder).await;
            // Stopped (or discarded) before the first frame: nothing went wrong.
            if outcome.discard || outcome.asked {
                self.set(CaptureState::Idle, Some(encoder), None);
                return Attempt::Done(None);
            }
            return Attempt::NothingRecorded(
                outcome.error.unwrap_or_else(|| "no frames captured".into()),
            );
        };
        if outcome.discard {
            remove_folder(folder).await;
            self.set(CaptureState::Idle, Some(encoder), None);
            return Attempt::Done(None);
        }

        let last = &outcome.last;
        let start_wall = ended
            .checked_sub(started_at.elapsed())
            .unwrap_or(UNIX_EPOCH);
        // The last frame, not ffmpeg's exit (it flushes and writes the index after it).
        let end_wall = last
            .out_time
            .and_then(|t| start_wall.checked_add(t))
            .unwrap_or(ended);
        tracing::info!(
            frames = last.frame,
            dropped = last.drop_frames,
            duplicated = last.dup_frames,
            seconds = last.out_time.map(|t| t.as_secs()),
            megabytes = last.total_size.map(|b| b / 1_000_000),
            "recording stopped"
        );
        if let Some(error) = &outcome.error {
            tracing::warn!(%error, "ffmpeg stopped before the match ended; keeping what was recorded");
        }

        // The fragmented file plays as it is, but a player has to walk every fragment
        // to open it. One stream copy now makes every later open instant.
        self.set(CaptureState::Saving, Some(encoder), None);
        let remux_started = Instant::now();
        match self.backend.finish_file(file).await {
            Ok(()) => tracing::debug!(
                seconds = remux_started.elapsed().as_secs_f32(),
                "recording remuxed"
            ),
            Err(error) => {
                tracing::warn!(%error, "could not remux the recording; keeping the fragmented file")
            }
        }
        match &outcome.error {
            Some(error) => self.set(CaptureState::Failed, Some(encoder), Some(error.clone())),
            None => self.set(CaptureState::Idle, Some(encoder), None),
        }

        Attempt::Done(Some(Recording {
            started_at,
            info: RecordingInfo {
                file: library::VIDEO_FILE.into(),
                start_wall_ms: wall_ms(start_wall),
                end_wall_ms: wall_ms(end_wall),
                game_time_at_start_s: None,
                encoder: encoder.to_owned(),
                width: monitor.width,
                height: monitor.height,
                fps: self.config.fps,
                bitrate_kbps: self.config.bitrate_kbps,
                frames: last.frame,
                dropped_frames: last.drop_frames,
                duplicated_frames: last.dup_frames,
                error: outcome.error,
                folder,
            },
        }))
    }

    /// Follows one ffmpeg run until it exits: progress in, stop or discard out.
    async fn drive(&mut self, run: CaptureRun) -> RunOutcome {
        let CaptureRun {
            mut progress,
            stop,
            mut exited,
        } = run;
        let mut stop = Some(stop);
        let mut stop_at: Option<tokio::time::Instant> = None;
        let mut commands_open = true;
        let mut progress_open = true;
        let mut discard = false;
        let mut estimate = StartEstimate::default();
        let mut last = Progress::default();
        let mut lag = LagWatch::default();
        loop {
            let take = |command: Command,
                        stop_at: &mut Option<tokio::time::Instant>,
                        discard: &mut bool| match command {
                Command::Record => {}
                Command::Stop(at) => *stop_at = Some(stop_at.map_or(at, |current| current.min(at))),
                Command::Discard => {
                    *discard = true;
                    *stop_at = Some(tokio::time::Instant::now());
                }
            };
            take(
                *self.commands.borrow_and_update(),
                &mut stop_at,
                &mut discard,
            );
            tokio::select! {
                report = progress.recv(), if progress_open => match report {
                    Some((received, report)) => {
                        estimate.add(received, &report);
                        lag.check(&report);
                        last = report;
                    }
                    // stdout closed: ffmpeg is exiting; its exit report follows.
                    None => progress_open = false,
                },
                changed = self.commands.changed(), if commands_open && stop.is_some() => {
                    // The controller is gone: stop and keep the file.
                    if changed.is_err() {
                        commands_open = false;
                        stop_at = Some(tokio::time::Instant::now());
                    }
                }
                _ = tokio::time::sleep_until(stop_at.unwrap_or_else(tokio::time::Instant::now)), if stop_at.is_some() && stop.is_some() => {
                    if let Some(stop) = stop.take() {
                        let _ = stop.send(());
                    }
                }
                exit = &mut exited => {
                    while let Ok((received, report)) = progress.try_recv() {
                        estimate.add(received, &report);
                        last = report;
                    }
                    let asked = stop.is_none();
                    let error = match exit {
                        Ok(exit) if exit.success => None,
                        Ok(exit) => Some(exit.detail.unwrap_or_else(|| "ffmpeg failed".into())),
                        Err(_) => Some("ffmpeg's supervisor went away".into()),
                    };
                    // After `q` a failing exit still finished the file; before it,
                    // anything is an error.
                    let error = match (asked, error) {
                        (true, Some(error)) => {
                            tracing::debug!(%error, "ffmpeg exit after stop");
                            None
                        }
                        (false, None) => Some("ffmpeg stopped by itself".into()),
                        (_, error) => error,
                    };
                    return RunOutcome { estimate, last, error, asked, discard };
                }
            }
        }
    }
}

enum Attempt {
    Done(Option<Recording>),
    NothingRecorded(String),
}

/// Warns (at most every five minutes, with running totals) when the capture falls
/// behind: frames dropped, or repeated after the encoder's start-up second.
const LAG_WARNING_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Default)]
struct LagWatch {
    baseline_dups: Option<u64>,
    reported: (u64, u64),
    last_warning: Option<Instant>,
}

impl LagWatch {
    fn check(&mut self, report: &Progress) {
        let Some(out_time) = report.out_time else {
            return;
        };
        if out_time < Duration::from_secs(2) {
            return;
        }
        let baseline = *self.baseline_dups.get_or_insert(report.dup_frames);
        let lagged = (
            report.drop_frames,
            report.dup_frames.saturating_sub(baseline),
        );
        let quiet = self
            .last_warning
            .is_none_or(|at| at.elapsed() >= LAG_WARNING_INTERVAL);
        if lagged != self.reported && lagged != (0, 0) && quiet {
            tracing::warn!(
                dropped = lagged.0,
                duplicated = lagged.1,
                at_s = out_time.as_secs(),
                "capture fell behind (frames dropped or repeated)"
            );
            self.last_warning = Some(Instant::now());
            self.reported = lagged;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::capture::ffmpeg::ExitReport;

    /// How a fake ffmpeg run behaves.
    #[derive(Clone, Copy)]
    enum Script {
        /// Reports progress until asked to stop, then finishes cleanly.
        Healthy,
        /// Exits with an error after this many reports.
        DiesAfter(u64),
        /// Exits at once without a frame.
        NoFrames,
    }

    struct FakeBackend {
        /// `game_display` answers `None` this many times first.
        window_after: AtomicUsize,
        window: bool,
        encoder: Result<String, String>,
        scripts: StdMutex<Vec<Script>>,
        plans: StdMutex<Vec<CapturePlan>>,
        finished: StdMutex<Vec<PathBuf>>,
        forgotten: AtomicUsize,
    }

    impl FakeBackend {
        fn new(scripts: Vec<Script>) -> Arc<Self> {
            Arc::new(Self {
                window_after: AtomicUsize::new(2),
                window: true,
                encoder: Ok("h264_nvenc".into()),
                scripts: StdMutex::new(scripts),
                plans: StdMutex::new(Vec::new()),
                finished: StdMutex::new(Vec::new()),
                forgotten: AtomicUsize::new(0),
            })
        }

        fn display(output: u32) -> Display {
            Display {
                adapter: 0,
                output,
                adapter_name: "Test GPU".into(),
                device_name: format!(r"\\.\DISPLAY{output}"),
                width: 2560,
                height: 1440,
            }
        }
    }

    impl CaptureBackend for FakeBackend {
        fn game_display(&self) -> Option<Display> {
            let before = self
                .window_after
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            (self.window && !before).then(|| Self::display(1))
        }

        fn primary_display(&self) -> Option<Display> {
            Some(Self::display(9))
        }

        fn encoder(&self) -> BoxFuture<'_, Result<String, String>> {
            Box::pin(async move { self.encoder.clone() })
        }

        fn forget_encoder(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.forgotten.fetch_add(1, Ordering::SeqCst);
            })
        }

        fn start(&self, plan: CapturePlan) -> BoxFuture<'_, Result<CaptureRun, String>> {
            Box::pin(async move {
                // ffmpeg creates the file.
                std::fs::write(&plan.file, b"fragmented").unwrap();
                self.plans.lock().unwrap().push(plan);
                let script = {
                    let mut scripts = self.scripts.lock().unwrap();
                    if scripts.is_empty() {
                        Script::Healthy
                    } else {
                        scripts.remove(0)
                    }
                };
                Ok(fake_run(script))
            })
        }

        fn finish_file(&self, file: PathBuf) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                self.finished.lock().unwrap().push(file);
                Ok(())
            })
        }
    }

    /// A pretend ffmpeg: one report every 5 ms, timestamps from its own start.
    fn fake_run(script: Script) -> CaptureRun {
        let (progress_tx, progress) = mpsc::channel(64);
        let (stop, mut stop_rx) = oneshot::channel::<()>();
        let (exited_tx, exited) = oneshot::channel();
        tokio::spawn(async move {
            let began = Instant::now();
            let report = |frame: u64, end: bool| Progress {
                frame,
                out_time: Some(began.elapsed()),
                dup_frames: 1,
                end,
                ..Progress::default()
            };
            if let Script::NoFrames = script {
                let _ = exited_tx.send(ExitReport {
                    success: false,
                    detail: Some("exit code: 1: OpenEncodeSessionEx failed".into()),
                });
                return;
            }
            let mut frame = 0;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        let _ = progress_tx.try_send((Instant::now(), report(frame, true)));
                        let _ = exited_tx.send(ExitReport { success: true, detail: None });
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {
                        frame += 1;
                        let _ = progress_tx.try_send((Instant::now(), report(frame, false)));
                        if matches!(script, Script::DiesAfter(n) if frame >= n) {
                            let _ = exited_tx.send(ExitReport {
                                success: false,
                                detail: Some("exit code: 1: Capture source size changed".into()),
                            });
                            return;
                        }
                    }
                }
            }
        });
        CaptureRun {
            progress,
            stop,
            exited,
        }
    }

    struct Setup {
        dir: PathBuf,
        backend: Arc<FakeBackend>,
        controller: CaptureController,
    }

    fn setup(name: &str, backend: Arc<FakeBackend>, enabled: bool) -> Setup {
        let dir =
            std::env::temp_dir().join(format!("downplayed-capture-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = CaptureConfig {
            enabled,
            fps: 60,
            bitrate_kbps: 10_000,
            encoder: None,
            ffmpeg: None,
            matches_dir: dir.join("matches"),
            probe_file: dir.join("capture-probe.json"),
        };
        let controller =
            CaptureController::new(config, backend.clone()).with_timings(CaptureTimings {
                window_poll: Duration::from_millis(5),
                window_wait: Duration::from_secs(5),
                retry_after: Duration::from_millis(5),
            });
        Setup {
            dir,
            backend,
            controller,
        }
    }

    fn request() -> StartRequest {
        StartRequest {
            started_wall_ms: 1_792_080_720_000, // 2026-10-15 16:12:00 UTC
            queue: Some("Ranked Solo/Duo".into()),
        }
    }

    /// Recording, with a few frames written.
    async fn wait_for_frames(controller: &CaptureController) {
        wait_for_state(controller, CaptureState::Recording).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    async fn wait_for_state(controller: &CaptureController, state: CaptureState) {
        let mut status = controller.status();
        tokio::time::timeout(
            Duration::from_secs(5),
            status.wait_for(|s| s.state == state),
        )
        .await
        .unwrap_or_else(|_| panic!("never reached {state:?}"))
        .unwrap();
    }

    fn folders(dir: &Path) -> Vec<String> {
        match std::fs::read_dir(dir.join("matches")) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[tokio::test]
    async fn records_the_game_monitor_from_start_to_stop() {
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("record", FakeBackend::new(vec![]), true);
        assert_eq!(
            *controller.status().borrow(),
            CaptureStatus {
                state: CaptureState::Idle,
                ..CaptureStatus::default()
            }
        );
        let before = Instant::now();
        assert!(controller.start(request()));
        assert!(!controller.start(request()), "one recording at a time");
        wait_for_state(&controller, CaptureState::Recording).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        controller.stop(Duration::ZERO);
        let recording = controller.finish(true).await.expect("a recording");
        assert!(!controller.is_active());

        let plans = backend.plans.lock().unwrap().clone();
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].output, 1,
            "the game window's monitor, once it appeared"
        );
        assert_eq!((plans[0].fps, plans[0].bitrate_kbps), (60, 10_000));
        assert_eq!(plans[0].encoder, "h264_nvenc");
        assert_eq!(folders(&dir), ["2026-10-15_16-12-00_Ranked-Solo-Duo"]);
        let info = &recording.info;
        assert_eq!(info.file, "match.mp4");
        assert_eq!(plans[0].file, info.folder.join("match.mp4"));
        assert_eq!(
            *backend.finished.lock().unwrap(),
            [plans[0].file.clone()],
            "remuxed once"
        );
        assert!(info.frames > 0 && info.error.is_none());
        assert_eq!(
            (info.width, info.height, info.encoder.as_str()),
            (2560, 1440, "h264_nvenc")
        );
        assert!(info.end_wall_ms >= info.start_wall_ms);
        // Video time 0 is when the fake ffmpeg began, give or take the 5 ms ticks.
        assert!(
            recording.started_at >= before,
            "{:?}",
            recording.started_at - before
        );
        assert!(recording.started_at.elapsed() >= Duration::from_millis(60));
        assert_eq!(controller.status().borrow().state, CaptureState::Idle);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_match_that_ends_before_the_game_window_records_nothing() {
        let backend = Arc::new(FakeBackend {
            window: false,
            ..Arc::into_inner(FakeBackend::new(vec![])).unwrap()
        });
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("nowindow", backend, true);
        assert!(controller.start(request()));
        wait_for_state(&controller, CaptureState::Starting).await;
        controller.stop(Duration::ZERO);
        assert!(controller.finish(true).await.is_none());
        assert!(backend.plans.lock().unwrap().is_empty());
        assert!(
            folders(&dir).is_empty(),
            "no folder for a match that wasn't recorded"
        );
        assert_eq!(controller.status().borrow().state, CaptureState::Idle);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn without_a_game_window_the_primary_monitor_is_recorded() {
        let backend = Arc::new(FakeBackend {
            window: false,
            ..Arc::into_inner(FakeBackend::new(vec![])).unwrap()
        });
        let Setup {
            dir,
            backend,
            controller,
        } = setup("primary", backend, true);
        let mut controller = controller.with_timings(CaptureTimings {
            window_poll: Duration::from_millis(5),
            window_wait: Duration::from_millis(30),
            retry_after: Duration::from_millis(5),
        });
        controller.start(request());
        wait_for_frames(&controller).await;
        let recording = controller.finish(true).await;
        assert!(recording.is_some());
        assert_eq!(backend.plans.lock().unwrap()[0].output, 9);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn discarding_deletes_the_recording() {
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("discard", FakeBackend::new(vec![]), true);
        controller.start(request());
        wait_for_state(&controller, CaptureState::Recording).await;
        assert_eq!(folders(&dir).len(), 1);
        assert!(controller.discard());
        assert!(!controller.discard(), "already discarding");
        assert!(controller.finish(true).await.is_none());
        assert!(!controller.discard(), "nothing left to discard");
        assert!(folders(&dir).is_empty());
        assert!(
            backend.finished.lock().unwrap().is_empty(),
            "no remux for a deleted file"
        );

        // Not tracked after the recording had already finished: deleted too.
        controller.start(request());
        wait_for_frames(&controller).await;
        controller.stop(Duration::ZERO);
        wait_for_state(&controller, CaptureState::Idle).await;
        assert_eq!(folders(&dir).len(), 1);
        assert!(controller.finish(false).await.is_none());
        assert!(folders(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_stop_waits_for_the_tail() {
        let Setup {
            dir,
            mut controller,
            ..
        } = setup("tail", FakeBackend::new(vec![]), true);
        controller.start(request());
        wait_for_state(&controller, CaptureState::Recording).await;
        let stopped = Instant::now();
        controller.stop(Duration::from_millis(150));
        // A later stop request doesn't push the end back.
        controller.stop(Duration::from_secs(60));
        let recording = controller.finish(true).await.unwrap();
        assert!(stopped.elapsed() >= Duration::from_millis(150));
        assert!(stopped.elapsed() < Duration::from_secs(30));
        assert!(recording.info.error.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ffmpeg_dying_mid_match_keeps_what_was_recorded() {
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("dies", FakeBackend::new(vec![Script::DiesAfter(10)]), true);
        controller.start(request());
        wait_for_state(&controller, CaptureState::Failed).await;
        let recording = controller.finish(true).await.expect("the part recorded");
        assert!(recording
            .info
            .error
            .as_deref()
            .unwrap()
            .contains("Capture source size changed"));
        assert!(recording.info.frames >= 10);
        assert_eq!(
            backend.finished.lock().unwrap().len(),
            1,
            "still made playable"
        );
        assert_eq!(folders(&dir).len(), 1);
        let status = controller.status().borrow().clone();
        assert_eq!(status.state, CaptureState::Failed);
        assert!(status.error.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_start_that_records_nothing_is_retried_once_with_a_fresh_probe() {
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("retry", FakeBackend::new(vec![Script::NoFrames]), true);
        controller.start(request());
        let second_attempt = async {
            while backend.plans.lock().unwrap().len() < 2 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), second_attempt)
            .await
            .unwrap();
        wait_for_frames(&controller).await;
        let recording = controller.finish(true).await;
        assert!(recording.is_some());
        assert_eq!(backend.plans.lock().unwrap().len(), 2);
        assert_eq!(backend.forgotten.load(Ordering::SeqCst), 1);
        assert_eq!(
            folders(&dir).len(),
            1,
            "the failed attempt's folder is gone"
        );

        // Failing twice gives up for this match.
        let Setup {
            dir: dir2,
            backend,
            mut controller,
        } = setup(
            "retry2",
            FakeBackend::new(vec![Script::NoFrames, Script::NoFrames]),
            true,
        );
        controller.start(request());
        wait_for_state(&controller, CaptureState::Failed).await;
        assert!(controller.finish(true).await.is_none());
        assert_eq!(backend.plans.lock().unwrap().len(), 2);
        assert!(folders(&dir2).is_empty());
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[tokio::test]
    async fn no_encoder_no_recording() {
        let backend = Arc::new(FakeBackend {
            encoder: Err("this ffmpeg has no ddagrab".into()),
            ..Arc::into_inner(FakeBackend::new(vec![])).unwrap()
        });
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("noencoder", backend, true);
        controller.start(request());
        wait_for_state(&controller, CaptureState::Failed).await;
        assert!(controller.finish(true).await.is_none());
        assert!(backend.plans.lock().unwrap().is_empty());
        assert_eq!(
            controller.status().borrow().error.as_deref(),
            Some("this ffmpeg has no ddagrab")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn turned_off_means_never_recording() {
        let Setup {
            dir,
            backend,
            mut controller,
        } = setup("off", FakeBackend::new(vec![]), false);
        assert_eq!(controller.status().borrow().state, CaptureState::Off);
        assert!(!controller.is_enabled());
        controller.prewarm();
        assert!(!controller.start(request()));
        assert!(controller.finish(true).await.is_none());
        assert!(backend.plans.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lag_is_reported_after_the_start_up_second() {
        let report = |secs, dups, drops| Progress {
            frame: secs * 60,
            out_time: Some(Duration::from_secs(secs)),
            dup_frames: dups,
            drop_frames: drops,
            ..Progress::default()
        };
        let mut watch = LagWatch::default();
        watch.check(&report(1, 4, 0));
        watch.check(&report(2, 4, 0));
        assert_eq!(watch.reported, (0, 0), "start-up duplicates aren't lag");
        assert!(watch.last_warning.is_none());
        watch.check(&report(10, 7, 1));
        assert_eq!(watch.reported, (1, 3));
        assert!(watch.last_warning.is_some());
    }
}
