//! The ffmpeg sidecar: where the binary is, the command lines Downplayed runs, and its
//! `-progress` reports.
//!
//! Video goes from the GPU straight to disk: `ddagrab` hands D3D11 frames to the
//! hardware encoder and the MP4 muxer writes a fragment per keyframe (1 s), so ffmpeg
//! never holds more than about a second of video and a crash keeps everything written
//! so far. The Rust side never touches video bytes, only ffmpeg's text output.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use super::audio::{AudioInput, RATE};
use super::clips::{self, ClipPlan};

/// Tauri's sidecar file name. `tools/fetch-ffmpeg.ps1` installs it in
/// `src-tauri/binaries`; the installer puts it next to Downplayed as `ffmpeg.exe`.
pub const SIDECAR_FILE: &str = "ffmpeg-x86_64-pc-windows-msvc.exe";

/// The capture output: fragmented MP4, one fragment per keyframe, so a crash or a kill
/// leaves a playable file.
pub const FRAGMENTED_MP4: &str = "+frag_keyframe+empty_moov+default_base_moof";

/// How often ffmpeg reports progress (`-stats_period`).
pub const PROGRESS_PERIOD: Duration = Duration::from_millis(500);

/// Finds the ffmpeg to record with: the configured one, `ffmpeg.exe` beside Downplayed
/// (the bundled sidecar), or in debug builds the copy `tools/fetch-ffmpeg.ps1` installs.
/// Never `PATH`: an arbitrary ffmpeg may lack `ddagrab` (5.1 "essentials" does) or be a
/// GPL build.
pub fn locate(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return Some(path.to_owned());
    }
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("ffmpeg.exe")));
    let fetched = cfg!(debug_assertions).then(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(SIDECAR_FILE)
    });
    [beside_exe, fetched]
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
}

/// How an encoder takes ddagrab's D3D11 frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderFamily {
    /// NVIDIA: D3D11 frames as they are.
    Nvenc,
    /// AMD: D3D11 frames as they are.
    Amf,
    /// Intel: frames mapped into a QSV device derived from the D3D11 one.
    Qsv,
    /// CPU (`libx264`, `libopenh264`, …): frames copied to system memory first.
    Software,
}

impl EncoderFamily {
    pub fn of(encoder: &str) -> Self {
        if encoder.ends_with("_nvenc") {
            Self::Nvenc
        } else if encoder.ends_with("_amf") {
            Self::Amf
        } else if encoder.ends_with("_qsv") {
            Self::Qsv
        } else {
            Self::Software
        }
    }

    pub fn is_hardware(self) -> bool {
        self != Self::Software
    }
}

/// Everything one capture needs.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturePlan {
    /// DXGI adapter index (`-init_hw_device d3d11va=…:<adapter>`); ddagrab must run on
    /// the adapter that drives the monitor.
    pub adapter: u32,
    /// DXGI output index on that adapter (ddagrab's `output_idx`).
    pub output: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// An ffmpeg encoder name, e.g. `h264_nvenc`.
    pub encoder: String,
    /// Audio sources; empty for video only.
    pub audio: Vec<AudioInput>,
    pub file: PathBuf,
}

fn args<const N: usize>(items: [&str; N]) -> Vec<OsString> {
    items.into_iter().map(OsString::from).collect()
}

/// The audio filter graph: with one source its track is the only one; with more, track 0
/// is their mix (made here, as ffmpeg records) and each source keeps a track of its own.
/// Returns the graph and the labels to map, in track order.
fn audio_graph(inputs: &[AudioInput]) -> Option<(String, Vec<String>)> {
    let source = |i: usize, input: &AudioInput| {
        let resample = if input.rate == RATE {
            String::new()
        } else {
            format!("aresample={RATE},")
        };
        format!("[{i}:a]{resample}volume={:.3}", input.volume)
    };
    match inputs {
        [] => None,
        [only] => Some((
            format!("{}[track0]", source(0, only)),
            vec!["[track0]".into()],
        )),
        _ => {
            let mut chains = Vec::new();
            let mut mix = String::new();
            let mut labels = vec!["[mix]".to_owned()];
            for (i, input) in inputs.iter().enumerate() {
                chains.push(format!(
                    "{},asplit=2[in{i}][track{}]",
                    source(i, input),
                    i + 1
                ));
                mix.push_str(&format!("[in{i}]"));
                labels.push(format!("[track{}]", i + 1));
            }
            chains.push(format!(
                "{mix}amix=inputs={}:duration=longest:normalize=0[mix]",
                inputs.len()
            ));
            Some((chains.join(";"), labels))
        }
    }
}

/// Track titles, in the order of [`audio_graph`]'s labels.
pub fn audio_titles(inputs: &[AudioInput]) -> Vec<&'static str> {
    let mut titles = Vec::new();
    if inputs.len() > 1 {
        titles.push("Mix");
    }
    titles.extend(inputs.iter().map(|i| i.kind.title()));
    titles
}

/// The capture command line: ddagrab on the planned monitor at a constant frame rate, a
/// keyframe every second, the audio sources' pipes, and a fragmented MP4. Progress goes to
/// stdout (`-progress pipe:1`), warnings to stderr; a `q` on stdin stops it cleanly.
pub fn capture_args(plan: &CapturePlan) -> Vec<OsString> {
    let fps = plan.fps.max(1);
    let family = EncoderFamily::of(&plan.encoder);
    let bitrate = plan.bitrate_kbps.max(1);
    // ddagrab's frames are timestamped from its first frame; PTS-STARTPTS keeps it so.
    let mut graph = format!(
        "ddagrab=output_idx={}:framerate={fps},setpts=PTS-STARTPTS",
        plan.output
    );
    match family {
        EncoderFamily::Nvenc | EncoderFamily::Amf => {}
        EncoderFamily::Qsv => graph.push_str(",hwmap=derive_device=qsv,format=qsv"),
        EncoderFamily::Software => graph.push_str(",hwdownload,format=bgra"),
    }
    graph.push_str("[video]");

    let mut out = args([
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "warning",
        "-progress",
        "pipe:1",
        "-stats_period",
    ]);
    out.push(format!("{}", PROGRESS_PERIOD.as_secs_f64()).into());
    // The pipes are opened before the video graph starts, so audio already flows when the
    // first frame arrives (ffmpeg holds a stream back until every stream has output).
    // Formats are given in full, so nothing waits to probe them.
    for input in &plan.audio {
        out.extend(args(["-thread_queue_size", "1024", "-f", "f32le", "-ar"]));
        out.push(input.rate.to_string().into());
        out.extend(args([
            "-ac",
            "2",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-i",
        ]));
        out.push(input.pipe.clone().into());
    }
    out.extend(args(["-init_hw_device"]));
    out.push(format!("d3d11va=capture:{}", plan.adapter).into());
    out.extend(args(["-filter_hw_device", "capture", "-filter_complex"]));
    out.push(graph.into());
    // Audio gets a graph of its own, so it can't hold up the real-time video source.
    let audio = audio_graph(&plan.audio);
    if let Some((graph, _)) = &audio {
        out.extend(args(["-filter_complex"]));
        out.push(graph.clone().into());
    }
    out.extend(args(["-map", "[video]", "-fps_mode", "cfr", "-r"]));
    out.push(fps.to_string().into());
    if let Some((_, labels)) = &audio {
        for label in labels {
            out.extend(args(["-map"]));
            out.push(label.into());
        }
    }
    out.extend(args(["-c:v"]));
    out.push(plan.encoder.clone().into());
    match family {
        EncoderFamily::Nvenc => {
            out.extend(args(["-preset", "p4", "-tune", "hq", "-rc", "vbr"]));
            if plan.encoder.starts_with("h264") {
                out.extend(args(["-profile:v", "high"]));
            }
        }
        EncoderFamily::Amf => out.extend(args([
            "-usage",
            "transcoding",
            "-quality",
            "balanced",
            "-rc",
            "vbr_peak",
        ])),
        EncoderFamily::Qsv => out.extend(args(["-preset", "medium"])),
        EncoderFamily::Software => {
            if plan.encoder == "libx264" {
                out.extend(args(["-preset", "veryfast"]));
            }
            out.extend(args(["-pix_fmt", "yuv420p"]));
        }
    }
    out.extend(args(["-b:v"]));
    out.push(format!("{bitrate}k").into());
    out.extend(args(["-maxrate"]));
    out.push(format!("{}k", bitrate.saturating_mul(3) / 2).into());
    out.extend(args(["-bufsize"]));
    out.push(format!("{}k", bitrate.saturating_mul(2)).into());
    // A keyframe every second: clips cut with a stream copy land within 1 s. No B-frames:
    // packets leave the encoder at once, which keeps the video start estimate (and so the
    // A/V alignment) within a frame or two.
    out.extend(args(["-g"]));
    out.push(fps.to_string().into());
    out.extend(args(["-bf", "0"]));
    if audio.is_some() {
        out.extend(args(["-c:a", "aac", "-b:a", "160k", "-b:a:0", "192k"]));
        for (i, title) in audio_titles(&plan.audio).into_iter().enumerate() {
            out.push(format!("-metadata:s:a:{i}").into());
            out.push(format!("title={title}").into());
            out.push(format!("-metadata:s:a:{i}").into());
            out.push(format!("handler_name={title}").into());
            out.push(format!("-disposition:a:{i}").into());
            out.push(if i == 0 { "default" } else { "0" }.into());
        }
    }
    out.extend(args(["-movflags", FRAGMENTED_MP4, "-f", "mp4", "-n"]));
    out.push(plan.file.clone().into_os_string());
    out
}

/// Rewrites a finished fragmented recording as a regular MP4 with a stream copy (no
/// re-encode). A fragmented file makes a player walk every fragment before it can show
/// the length: for a 30-minute match Chromium read 1.8 GB in 1800 requests to open it,
/// against 1.4 MB in 2 for the same video with one index (`moov`) at the end.
///
/// `audio_lead_s`: how long before the first video frame the audio started. The audio
/// tracks are moved back by that much (an edit list on each, nothing re-encoded), which
/// puts them in sync with the video.
pub fn remux_args(input: &Path, output: &Path, audio_lead_s: Option<f64>) -> Vec<OsString> {
    let mut out = args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"]);
    out.push(input.into());
    match audio_lead_s.filter(|lead| lead.abs() >= 0.000_5) {
        Some(lead) => {
            out.extend(args(["-itsoffset"]));
            out.push(format!("{:.6}", -lead).into());
            out.extend(args(["-i"]));
            out.push(input.into());
            out.extend(args(["-map", "0:v", "-map", "1:a"]));
        }
        None => out.extend(args(["-map", "0"])),
    }
    out.extend(args(["-c", "copy", "-f", "mp4", "-n"]));
    out.push(output.into());
    out
}

/// One `-progress` report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// Frames written so far.
    pub frame: u64,
    /// Output timestamp reached (the last frame muxed).
    pub out_time: Option<Duration>,
    pub fps: Option<f64>,
    pub speed: Option<f64>,
    pub total_size: Option<u64>,
    /// Frames repeated to keep a constant frame rate: the capture fell behind.
    pub dup_frames: u64,
    /// Frames thrown away to keep a constant frame rate.
    pub drop_frames: u64,
    /// `progress=end`: ffmpeg is finishing.
    pub end: bool,
}

/// Turns `-progress` output (`key=value` lines, each report ending with `progress=…`)
/// into [`Progress`] reports.
#[derive(Debug, Default)]
pub struct ProgressParser {
    current: Progress,
}

impl ProgressParser {
    /// Feeds one line; returns the report it completes, if any.
    pub fn push(&mut self, line: &str) -> Option<Progress> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        let number = || value.parse::<u64>().ok();
        let float = || {
            value
                .trim_end_matches('x')
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
        };
        let current = &mut self.current;
        match key.trim() {
            "frame" => current.frame = number().unwrap_or(current.frame),
            "fps" => current.fps = float(),
            "out_time_us" => {
                current.out_time = value
                    .parse::<i64>()
                    .ok()
                    .and_then(|us| u64::try_from(us).ok())
                    .map(Duration::from_micros)
            }
            "total_size" => current.total_size = number(),
            "dup_frames" => current.dup_frames = number().unwrap_or(current.dup_frames),
            "drop_frames" => current.drop_frames = number().unwrap_or(current.drop_frames),
            "speed" => current.speed = float(),
            "progress" => {
                current.end = value == "end";
                return Some(current.clone());
            }
            _ => {}
        }
        None
    }
}

/// When video time 0 happened on the monotonic clock, from progress reports.
///
/// A report that arrives at `received` saying `frame` video frames were written means
/// the last of them, at video time `(frame - 1) / fps`, was captured no later than
/// `received` minus that. Encoding, muxing and pipe latency only push that later, so the
/// earliest such bound is the best estimate; with no B-frames it lands within a frame or
/// two of the truth. (`out_time` won't do: ffmpeg reports the trailing stream's time,
/// which with audio can be the audio's.) ffmpeg stamps ddagrab frames with QPC time, the
/// clock [`Instant`] uses on Windows, so the estimate doesn't drift over a long match.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StartEstimate {
    fps: f64,
    best: Option<Instant>,
}

impl StartEstimate {
    pub fn new(fps: u32) -> Self {
        Self {
            fps: f64::from(fps.max(1)),
            best: None,
        }
    }

    pub fn add(&mut self, received: Instant, progress: &Progress) {
        let Some(last) = progress.frame.checked_sub(1) else {
            return;
        };
        let video_time = Duration::from_secs_f64(last as f64 / self.fps);
        if let Some(candidate) = received.checked_sub(video_time) {
            self.best = Some(self.best.map_or(candidate, |best| best.min(candidate)));
        }
    }

    pub fn get(&self) -> Option<Instant> {
        self.best
    }
}

/// A progress report and when it arrived.
pub type ProgressReport = (Instant, Progress);

/// How a sidecar process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    /// It exited with status 0 (after `q`, or a finished remux).
    pub success: bool,
    /// The exit status and ffmpeg's last stderr lines, when it didn't succeed.
    pub detail: Option<String>,
}

/// A running capture process.
pub struct CaptureRun {
    /// Progress reports; closes when ffmpeg exits.
    pub progress: mpsc::Receiver<ProgressReport>,
    /// Ask ffmpeg to finish (`q`). Dropping it does the same.
    pub stop: oneshot::Sender<()>,
    pub exited: oneshot::Receiver<ExitReport>,
}

/// How long ffmpeg gets to flush and close the file after `q` before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
/// ffmpeg stderr lines kept for error reports.
const STDERR_TAIL: usize = 12;
/// stderr lines logged (at debug) per process; ffmpeg can repeat a warning endlessly.
const STDERR_LOGGED: usize = 40;

// Windows process creation flags.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x0000_8000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// CPU priority of a sidecar process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Hardware captures: a few % of one core, but frame pacing mustn't wait behind the
    /// game's threads.
    AboveNormal,
    Normal,
    /// Remuxing and CPU encoding: never at the game's expense.
    BelowNormal,
}

fn command(ffmpeg: &Path, args: &[OsString], priority: Priority) -> Command {
    let mut command = Command::new(ffmpeg);
    command.args(args).kill_on_drop(true);
    let class = match priority {
        Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        Priority::Normal => 0,
        Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
    };
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | class);
    #[cfg(not(windows))]
    let _ = (CREATE_NO_WINDOW, class);
    command
}

/// Starts a capture. ffmpeg runs in a kill-on-close job, so it never outlives Downplayed
/// (a crash still leaves a playable fragmented file).
pub fn spawn_capture(
    ffmpeg: &Path,
    args: &[OsString],
    priority: Priority,
) -> io::Result<CaptureRun> {
    let mut child = command(ffmpeg, args, priority)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let job = job::KillOnClose::adopt(&child)?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (progress_tx, progress) = mpsc::channel(64);
    if let Some(stdout) = stdout {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut parser = ProgressParser::default();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(report) = parser.push(&line) {
                    tracing::trace!(?report, "ffmpeg progress");
                    // Never block ffmpeg on a slow reader: a missed report costs nothing.
                    let _ = progress_tx.try_send((Instant::now(), report));
                }
            }
        });
    }
    let tail = stderr.map(|stderr| tokio::spawn(read_stderr(stderr)));

    let (stop, stop_rx) = oneshot::channel();
    let (exited_tx, exited) = oneshot::channel();
    tokio::spawn(async move {
        let _job = job;
        let status = supervise(&mut child, stdin, stop_rx).await;
        let tail = match tail {
            Some(task) => task.await.unwrap_or_default(),
            None => VecDeque::new(),
        };
        let _ = exited_tx.send(exit_report(status, tail));
    });
    Ok(CaptureRun {
        progress,
        stop,
        exited,
    })
}

/// Waits for the stop request (or the process ending by itself), sends `q`, and kills
/// ffmpeg if it hasn't finished the file in time.
async fn supervise(
    child: &mut Child,
    stdin: Option<tokio::process::ChildStdin>,
    stop: oneshot::Receiver<()>,
) -> io::Result<std::process::ExitStatus> {
    let mut stdin = stdin;
    tokio::select! {
        status = child.wait() => return status,
        _ = stop => {}
    }
    if let Some(mut pipe) = stdin.take() {
        // ffmpeg polls its stdin pipe for keys; `q` finishes the file and exits.
        let _ = pipe.write_all(b"q").await;
        let _ = pipe.flush().await;
    }
    match tokio::time::timeout(STOP_TIMEOUT, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            tracing::warn!(
                "ffmpeg did not stop in time; killing it (the fragmented file stays playable)"
            );
            child.kill().await?;
            child.wait().await
        }
    }
}

async fn read_stderr(stderr: tokio::process::ChildStderr) -> VecDeque<String> {
    let mut lines = BufReader::new(stderr).lines();
    let mut tail = VecDeque::with_capacity(STDERR_TAIL);
    let mut logged = 0;
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        if logged < STDERR_LOGGED {
            tracing::debug!(%line, "ffmpeg");
            logged += 1;
        }
        if tail.len() == STDERR_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    tail
}

fn exit_report(status: io::Result<std::process::ExitStatus>, tail: VecDeque<String>) -> ExitReport {
    let success = matches!(&status, Ok(status) if status.success());
    let detail = (!success).then(|| {
        let status = match status {
            Ok(status) => status.to_string(),
            Err(e) => e.to_string(),
        };
        let tail: Vec<String> = tail.into();
        if tail.is_empty() {
            status
        } else {
            format!("{status}: {}", tail.join(" | "))
        }
    });
    ExitReport { success, detail }
}

/// Runs ffmpeg to completion (probes, remuxes) and returns how it ended plus its stdout.
pub async fn run_to_end(
    ffmpeg: &Path,
    args: &[OsString],
    priority: Priority,
    timeout: Duration,
) -> io::Result<(ExitReport, String)> {
    let child = command(ffmpeg, args, priority)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let job = job::KillOnClose::adopt(&child)?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => output?,
        // Dropping the future kills the process (kill_on_drop), and the job with it.
        Err(_) => {
            drop(job);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ffmpeg took too long",
            ));
        }
    };
    drop(job);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(STDERR_TAIL)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    Ok((
        exit_report(Ok(output.status), tail),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

/// Replaces a fragmented recording with a regular MP4 of the same streams, the audio
/// moved into sync (see [`remux_args`]). On failure the fragmented file stays as it was;
/// it plays, with a slower start and the audio `audio_lead_s` late.
pub async fn remux_to_mp4(
    ffmpeg: &Path,
    file: &Path,
    audio_lead_s: Option<f64>,
) -> Result<(), String> {
    let temp = file.with_extension("remux.mp4");
    let _ = tokio::fs::remove_file(&temp).await;
    let outcome = run_to_end(
        ffmpeg,
        &remux_args(file, &temp, audio_lead_s),
        Priority::BelowNormal,
        Duration::from_secs(15 * 60),
    )
    .await;
    let result = match outcome {
        Ok((exit, _)) if exit.success => replace(&temp, file).await,
        Ok((exit, _)) => Err(exit.detail.unwrap_or_else(|| "ffmpeg failed".into())),
        Err(e) => Err(e.to_string()),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// Cuts one clip and its thumbnail into `folder` (see `clips`). Returns whether the
/// thumbnail was made; a clip without one still counts.
pub async fn cut_clip(
    ffmpeg: &Path,
    video: &Path,
    folder: &Path,
    plan: &ClipPlan,
) -> Result<bool, String> {
    let clip = folder.join(&plan.file);
    let thumbnail = folder.join(&plan.thumbnail);
    for dir in [clip.parent(), thumbnail.parent()].into_iter().flatten() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| e.to_string())?;
    }
    let cut = clips::cut_args(video, plan.from_s, plan.to_s, &clip);
    let (exit, _) = run_to_end(
        ffmpeg,
        &cut,
        Priority::BelowNormal,
        Duration::from_secs(120),
    )
    .await
    .map_err(|e| e.to_string())?;
    if !exit.success {
        let _ = tokio::fs::remove_file(&clip).await;
        return Err(exit.detail.unwrap_or_else(|| "ffmpeg failed".into()));
    }
    let frame = clips::thumbnail_args(video, plan.thumbnail_at_s, &thumbnail);
    match run_to_end(
        ffmpeg,
        &frame,
        Priority::BelowNormal,
        Duration::from_secs(30),
    )
    .await
    {
        Ok((exit, _)) if exit.success => Ok(true),
        _ => {
            let _ = tokio::fs::remove_file(&thumbnail).await;
            Ok(false)
        }
    }
}

async fn replace(temp: &Path, file: &Path) -> Result<(), String> {
    let (old, new) = match (
        tokio::fs::metadata(file).await,
        tokio::fs::metadata(temp).await,
    ) {
        (Ok(old), Ok(new)) => (old.len(), new.len()),
        (Err(e), _) | (_, Err(e)) => return Err(e.to_string()),
    };
    // A stream copy only changes the index; anything much smaller lost data.
    if new < old / 10 * 9 {
        return Err(format!("remuxed file is {new} bytes, the recording {old}"));
    }
    // A player may have the file open for a moment (Windows refuses to replace it then).
    let mut attempt = 0;
    loop {
        match tokio::fs::rename(temp, file).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 10 => {
                tracing::debug!(error = %e, "recording busy; retrying the replace");
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Kill-on-close job objects: a sidecar dies with Downplayed, however Downplayed ends.
mod job {
    use std::io;

    use tokio::process::Child;

    pub struct KillOnClose {
        #[cfg(windows)]
        handle: windows::Win32::Foundation::HANDLE,
    }

    // The job handle is only closed, from whichever thread drops it.
    unsafe impl Send for KillOnClose {}
    unsafe impl Sync for KillOnClose {}

    impl KillOnClose {
        #[cfg(windows)]
        pub fn adopt(child: &Child) -> io::Result<Self> {
            use windows::core::PCWSTR;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };

            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("process already exited"))?;
            // SAFETY: plain Win32 calls on a job handle this function owns (closed by
            // Drop, also on the error paths) and the child's live process handle.
            unsafe {
                let job = Self {
                    handle: CreateJobObjectW(None, PCWSTR::null()).map_err(io::Error::other)?,
                };
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .map_err(io::Error::other)?;
                AssignProcessToJobObject(job.handle, HANDLE(process)).map_err(io::Error::other)?;
                Ok(job)
            }
        }

        #[cfg(not(windows))]
        pub fn adopt(_child: &Child) -> io::Result<Self> {
            Ok(Self {})
        }
    }

    impl Drop for KillOnClose {
        fn drop(&mut self) {
            #[cfg(windows)]
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::audio::SourceKind;

    fn plan(encoder: &str) -> CapturePlan {
        CapturePlan {
            adapter: 1,
            output: 2,
            fps: 60,
            bitrate_kbps: 10_000,
            encoder: encoder.into(),
            audio: Vec::new(),
            file: PathBuf::from(r"C:\lib\2026-10-15_19-02-11_Ranked\match.mp4"),
        }
    }

    fn input(kind: SourceKind, rate: u32, volume: f32) -> AudioInput {
        AudioInput {
            kind,
            pipe: format!(r"\\.\pipe\dp-{}", kind.title()),
            rate,
            volume,
        }
    }

    /// Every value that follows `flag`, in order.
    fn values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.iter()
            .enumerate()
            .filter(|(_, a)| *a == flag)
            .map(|(i, _)| args[i + 1].as_str())
            .collect()
    }

    fn text(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The value after `flag`.
    fn value<'a>(args: &'a [String], flag: &str) -> &'a str {
        let at = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("no {flag} in {args:?}"));
        &args[at + 1]
    }

    #[test]
    fn nvenc_capture_command_line() {
        let args = text(&capture_args(&plan("h264_nvenc")));
        assert_eq!(value(&args, "-init_hw_device"), "d3d11va=capture:1");
        assert_eq!(value(&args, "-filter_hw_device"), "capture");
        assert_eq!(
            value(&args, "-filter_complex"),
            "ddagrab=output_idx=2:framerate=60,setpts=PTS-STARTPTS[video]",
            "D3D11 frames go to NVENC without a copy"
        );
        assert_eq!(value(&args, "-map"), "[video]");
        assert_eq!(value(&args, "-fps_mode"), "cfr");
        assert_eq!(value(&args, "-r"), "60");
        assert_eq!(value(&args, "-c:v"), "h264_nvenc");
        assert_eq!(value(&args, "-g"), "60", "a keyframe every second");
        assert_eq!(value(&args, "-b:v"), "10000k");
        assert_eq!(value(&args, "-maxrate"), "15000k");
        assert_eq!(value(&args, "-bufsize"), "20000k");
        assert_eq!(value(&args, "-profile:v"), "high");
        assert_eq!(
            value(&args, "-movflags"),
            "+frag_keyframe+empty_moov+default_base_moof"
        );
        assert_eq!(value(&args, "-progress"), "pipe:1");
        assert_eq!(value(&args, "-stats_period"), "0.5");
        assert!(args.contains(&"-n".to_owned()), "never overwrite a file");
        assert!(
            !args.contains(&"-nostdin".to_owned()),
            "stdin carries the q that stops it"
        );
        assert_eq!(
            args.last().unwrap(),
            r"C:\lib\2026-10-15_19-02-11_Ranked\match.mp4"
        );
    }

    #[test]
    fn keyframe_interval_follows_the_frame_rate() {
        let mut thirty = plan("hevc_nvenc");
        thirty.fps = 30;
        let args = text(&capture_args(&thirty));
        assert_eq!(value(&args, "-g"), "30");
        assert_eq!(value(&args, "-r"), "30");
        assert!(value(&args, "-filter_complex").contains("framerate=30"));
        assert!(
            !args.contains(&"-profile:v".to_owned()),
            "high is an H.264 profile"
        );
    }

    #[test]
    fn other_encoders_get_their_frames_the_way_they_take_them() {
        let qsv = text(&capture_args(&plan("h264_qsv")));
        assert!(
            value(&qsv, "-filter_complex").ends_with(",hwmap=derive_device=qsv,format=qsv[video]")
        );
        let amf = text(&capture_args(&plan("h264_amf")));
        assert!(value(&amf, "-filter_complex").ends_with("setpts=PTS-STARTPTS[video]"));
        assert_eq!(value(&amf, "-rc"), "vbr_peak");
        for software in ["libx264", "libopenh264"] {
            let args = text(&capture_args(&plan(software)));
            assert!(value(&args, "-filter_complex").ends_with(",hwdownload,format=bgra[video]"));
            assert_eq!(value(&args, "-pix_fmt"), "yuv420p");
        }
        assert_eq!(
            value(&text(&capture_args(&plan("libx264"))), "-preset"),
            "veryfast"
        );
        assert_eq!(EncoderFamily::of("av1_nvenc"), EncoderFamily::Nvenc);
        assert!(!EncoderFamily::of("libopenh264").is_hardware());
    }

    #[test]
    fn remux_is_a_stream_copy_that_never_overwrites() {
        let args = text(&remux_args(
            Path::new("match.mp4"),
            Path::new("match.remux.mp4"),
            None,
        ));
        assert_eq!(value(&args, "-i"), "match.mp4");
        assert_eq!(value(&args, "-c"), "copy");
        assert_eq!(value(&args, "-map"), "0");
        assert!(args.contains(&"-n".to_owned()) && args.contains(&"-nostdin".to_owned()));
        assert!(
            !args.contains(&"-movflags".to_owned()),
            "a plain MP4: one index at the end"
        );
        assert_eq!(args.last().unwrap(), "match.remux.mp4");
    }

    #[test]
    fn progress_reports_parse() {
        let output = "frame=16\nfps=0.00\nstream_0_0_q=22.0\nbitrate=   1.0kbits/s\ntotal_size=28\nout_time_us=216667\nout_time_ms=216667\nout_time=00:00:00.216667\ndup_frames=4\ndrop_frames=0\nspeed=0.425x\nprogress=continue\nframe=300\nfps=59.53\nout_time_us=N/A\ntotal_size=N/A\ndup_frames=5\ndrop_frames=1\nspeed=N/A\nprogress=end\n";
        let mut parser = ProgressParser::default();
        let reports: Vec<Progress> = output.lines().filter_map(|l| parser.push(l)).collect();
        assert_eq!(reports.len(), 2);
        assert_eq!(
            reports[0],
            Progress {
                frame: 16,
                out_time: Some(Duration::from_micros(216_667)),
                fps: Some(0.0),
                speed: Some(0.425),
                total_size: Some(28),
                dup_frames: 4,
                drop_frames: 0,
                end: false,
            }
        );
        let last = &reports[1];
        assert_eq!((last.frame, last.dup_frames, last.drop_frames), (300, 5, 1));
        assert_eq!(
            (last.out_time, last.total_size, last.speed),
            (None, None, None)
        );
        assert!(last.end);
        assert_eq!(parser.push("garbage without equals"), None);
    }

    #[test]
    fn start_estimate_takes_the_least_delayed_report() {
        let t0 = Instant::now();
        let report = |frame, ms| Progress {
            frame,
            out_time: Some(Duration::from_millis(ms)),
            ..Progress::default()
        };
        let mut estimate = StartEstimate::new(60);
        estimate.add(t0 + Duration::from_millis(900), &report(0, 0));
        assert_eq!(estimate.get(), None, "nothing written yet");
        // Frame 61 (video time 1.0 s) is reported 103 ms after it was captured, frame 91
        // (1.5 s) only 23 ms after. The audio's out_time doesn't come into it.
        estimate.add(t0 + Duration::from_millis(1103), &report(61, 9_000));
        estimate.add(t0 + Duration::from_millis(1523), &report(91, 0));
        estimate.add(t0 + Duration::from_millis(2183), &report(121, 2000));
        let ms = (estimate.get().unwrap() - t0).as_secs_f64() * 1000.0;
        assert!((ms - 23.0).abs() < 0.5, "{ms} ms");
    }

    #[test]
    fn one_audio_source_is_one_track() {
        let mut plan = plan("h264_nvenc");
        plan.audio = vec![input(SourceKind::Desktop, 48_000, 1.0)];
        let args = text(&capture_args(&plan));
        // The pipe is an input, its format given in full, before the video graph.
        let pipe = args.iter().position(|a| a.ends_with("dp-Desktop")).unwrap();
        let graph = args.iter().position(|a| a.starts_with("ddagrab=")).unwrap();
        assert!(pipe < graph);
        assert_eq!(
            &args[pipe - 13..pipe],
            [
                "-thread_queue_size",
                "1024",
                "-f",
                "f32le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-i"
            ]
        );
        let filters = values(&args, "-filter_complex");
        assert_eq!(
            filters[1], "[0:a]volume=1.000[track0]",
            "a graph of its own"
        );
        assert_eq!(values(&args, "-map"), ["[video]", "[track0]"]);
        assert_eq!(value(&args, "-c:a"), "aac");
        assert_eq!(
            values(&args, "-metadata:s:a:0"),
            ["title=Desktop", "handler_name=Desktop"]
        );
        assert_eq!(value(&args, "-disposition:a:0"), "default");
        assert_eq!(value(&args, "-bf"), "0");
    }

    #[test]
    fn two_sources_are_mixed_into_track_0_and_kept_apart() {
        let mut plan = plan("h264_nvenc");
        plan.audio = vec![
            input(SourceKind::Game, 48_000, 0.8),
            input(SourceKind::Microphone, 44_100, 1.5),
        ];
        let args = text(&capture_args(&plan));
        assert_eq!(
            values(&args, "-filter_complex")[1],
            "[0:a]volume=0.800,asplit=2[in0][track1];\
             [1:a]aresample=48000,volume=1.500,asplit=2[in1][track2];\
             [in0][in1]amix=inputs=2:duration=longest:normalize=0[mix]"
        );
        assert_eq!(
            values(&args, "-map"),
            ["[video]", "[mix]", "[track1]", "[track2]"]
        );
        assert_eq!(value(&args, "-b:a:0"), "192k", "the mix gets the most bits");
        assert_eq!(values(&args, "-metadata:s:a:1")[0], "title=Game");
        assert_eq!(values(&args, "-metadata:s:a:2")[0], "title=Microphone");
        assert_eq!(
            value(&args, "-disposition:a:1"),
            "0",
            "only the mix plays by default"
        );
        assert_eq!(audio_titles(&plan.audio), ["Mix", "Game", "Microphone"]);
        let silent = text(&capture_args(&self::plan("h264_nvenc")));
        assert!(
            !silent.contains(&"-c:a".to_owned()),
            "no audio, no audio codec"
        );
    }

    #[test]
    fn the_remux_moves_the_audio_back_by_its_head_start() {
        let args = text(&remux_args(
            Path::new("m.mp4"),
            Path::new("out.mp4"),
            Some(0.4123),
        ));
        assert_eq!(value(&args, "-itsoffset"), "-0.412300");
        assert_eq!(
            values(&args, "-i"),
            ["m.mp4", "m.mp4"],
            "video from the first, audio from the shifted second"
        );
        assert_eq!(values(&args, "-map"), ["0:v", "1:a"]);
        assert_eq!(value(&args, "-c"), "copy");
        // Nothing worth moving: a plain copy.
        let plain = text(&remux_args(
            Path::new("m.mp4"),
            Path::new("out.mp4"),
            Some(0.0001),
        ));
        assert!(!plain.contains(&"-itsoffset".to_owned()));
    }
}
