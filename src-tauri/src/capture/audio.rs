//! Audio for the recording (doc 03 §5): WASAPI sources captured in Rust and fed to the
//! ffmpeg sidecar as raw 32-bit float stereo PCM over named pipes, one pipe per source.
//! ffmpeg mixes them into track 0 as it records and keeps each source as its own track.
//!
//! ffmpeg reads a raw stream at its nominal rate and stamps it by counting samples, so each
//! source is placed on the QPC timeline here (the clock ddagrab stamps video with): silence
//! fills the gaps loopback capture leaves while nothing plays, and single frames are dropped
//! or repeated as the sound card's clock drifts from QPC. The tracks therefore stay locked
//! to the video however long the match. All streams share one origin and start before
//! ffmpeg's first video frame (ffmpeg holds a stream back until every stream has output),
//! and that head start is measured and taken out when the file is remuxed.
//!
//! Memory stays small and flat: about a second of audio queued per source, the rest on
//! its way to disk.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Every stream reaches ffmpeg as this.
pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// Where a track's sound comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Everything the output device plays (WASAPI loopback).
    Desktop,
    /// League's own sound (process loopback on `League of Legends.exe`).
    Game,
    Microphone,
}

impl SourceKind {
    /// The track's title in the file.
    pub fn title(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Game => "Game",
            Self::Microphone => "Microphone",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Game => "game",
            Self::Microphone => "mic",
        }
    }
}

/// One source to capture.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceConfig {
    pub kind: SourceKind,
    /// Part of a device's name (any case); `None`: the default device.
    pub device: Option<String>,
    pub volume: f32,
}

/// One audio input of the ffmpeg command line: raw f32le stereo on `pipe`, at `rate`
/// ([`RATE`] unless the device couldn't convert; ffmpeg resamples then).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInput {
    pub kind: SourceKind,
    pub pipe: String,
    pub rate: u32,
    pub volume: f32,
}

/// What a source did over a recording, for `match.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReport {
    pub kind: SourceKind,
    /// The device's name, or the process for game audio.
    pub device: String,
    #[serde(flatten)]
    pub stats: TimelineStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A running set of sources.
pub struct AudioRun {
    pub inputs: Vec<AudioInput>,
    /// The instant sample 0 of every stream was taken.
    pub origin: Instant,
    /// Stops the sources and closes their pipes (once ffmpeg has finished with them).
    pub stop: Box<dyn FnOnce() -> BoxFuture<'static, Vec<SourceReport>> + Send>,
}

/// A packet farther than this from where the stream has got to is a gap (silence goes in)
/// or an overlap (its start is dropped).
const JUMP_S: f64 = 0.020;
/// Closer than this, the stream simply follows on.
const DEADBAND_S: f64 = 0.001;
/// With nothing arriving (loopback while nothing plays), the stream is kept this close
/// behind real time. More than a packet's delivery delay, so late packets aren't cut.
pub const IDLE_LAG_S: f64 = 0.050;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineStats {
    /// Frames written to the stream in all.
    pub frames: u64,
    /// Frames of silence put in where nothing was captured.
    pub silence_frames: u64,
    /// Frames dropped (overlaps, a clock running fast) and repeated (a clock running slow).
    pub dropped_frames: u64,
    pub repeated_frames: u64,
}

/// Places captured packets (interleaved f32, [`CHANNELS`] wide) on a timeline that starts
/// at the capture origin, so a stream's sample count always matches elapsed QPC time.
#[derive(Debug, Clone)]
pub struct Timeline {
    rate: f64,
    written: u64,
    stats: TimelineStats,
}

impl Timeline {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: f64::from(rate),
            written: 0,
            stats: TimelineStats::default(),
        }
    }

    pub fn stats(&self) -> TimelineStats {
        TimelineStats {
            frames: self.written,
            ..self.stats
        }
    }

    fn silence(&mut self, frames: u64, out: &mut Vec<f32>) {
        out.resize(out.len() + frames as usize * usize::from(CHANNELS), 0.0);
        self.written += frames;
        self.stats.silence_frames += frames;
    }

    /// A packet whose first frame was captured `at_s` seconds after the origin (`None`:
    /// no usable timestamp, so it follows on). Appends the frames to put out to `out`.
    pub fn place(&mut self, at_s: Option<f64>, samples: &[f32], out: &mut Vec<f32>) {
        let width = usize::from(CHANNELS);
        let frames = samples.len() / width;
        if frames == 0 {
            return;
        }
        let mut skip = 0;
        let mut repeat = false;
        if let Some(at_s) = at_s {
            let error = at_s * self.rate - self.written as f64;
            if error > JUMP_S * self.rate {
                self.silence(error.round() as u64, out);
            } else if error < -JUMP_S * self.rate {
                skip = ((-error).round() as usize).min(frames);
            } else if error > DEADBAND_S * self.rate {
                repeat = true;
            } else if error < -DEADBAND_S * self.rate && frames > 1 {
                skip = 1;
            }
        }
        if repeat {
            out.extend_from_slice(&samples[..width]);
            self.written += 1;
            self.stats.repeated_frames += 1;
        }
        self.stats.dropped_frames += skip as u64;
        out.extend_from_slice(&samples[skip * width..frames * width]);
        self.written += (frames - skip) as u64;
    }

    /// Nothing arrived: extend the stream with silence to [`IDLE_LAG_S`] behind `now_s`.
    pub fn idle(&mut self, now_s: f64, out: &mut Vec<f32>) {
        let target = ((now_s - IDLE_LAG_S) * self.rate).floor();
        if target > self.written as f64 {
            self.silence(target as u64 - self.written, out);
        }
    }
}

/// How a device delivers samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    I32,
}

/// Interleaved device samples (`channels` wide, little-endian) as interleaved stereo f32.
/// Mono is doubled; surround keeps the front pair with the centre mixed in.
pub fn to_stereo(bytes: &[u8], format: SampleFormat, channels: usize, out: &mut Vec<f32>) {
    let size = match format {
        SampleFormat::F32 | SampleFormat::I32 => 4,
        SampleFormat::I16 => 2,
    };
    let channels = channels.max(1);
    let sample = |frame: &[u8], channel: usize| -> f32 {
        let at = channel * size;
        let raw = &frame[at..at + size];
        match format {
            SampleFormat::F32 => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            SampleFormat::I16 => f32::from(i16::from_le_bytes([raw[0], raw[1]])) / 32_768.0,
            SampleFormat::I32 => {
                i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f32 / 2_147_483_648.0
            }
        }
    };
    const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for frame in bytes.chunks_exact(size * channels) {
        let (left, right) = match channels {
            1 => {
                let s = sample(frame, 0);
                (s, s)
            }
            2..=4 => (sample(frame, 0), sample(frame, 1)),
            // 5.1 / 7.1: FL FR FC LFE …
            _ => {
                let centre = sample(frame, 2) * CENTRE;
                (sample(frame, 0) + centre, sample(frame, 1) + centre)
            }
        };
        out.push(left);
        out.push(right);
    }
}

/// A unique pipe for one source of one recording.
pub fn pipe_name(kind: SourceKind) -> String {
    static RUN: AtomicU64 = AtomicU64::new(0);
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\downplayed-{}-{run}-{}",
        std::process::id(),
        kind.slug()
    )
}

/// How long a capture thread waits for room in its queue before checking for a stop.
const QUEUE_RETRY: Duration = Duration::from_millis(5);
/// Chunks (about 10 ms each) queued per source while ffmpeg isn't reading.
pub const QUEUE_CHUNKS: usize = 256;
/// How long to wait for ffmpeg to open a pipe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Hands chunks from a capture thread to the pipe writer without ever dropping one: a
/// dropped chunk would shift the rest of the stream. Gives up only when told to stop.
pub fn send_chunk(tx: &mpsc::Sender<Vec<u8>>, mut chunk: Vec<u8>, stop: &AtomicBool) -> bool {
    loop {
        match tx.try_send(chunk) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(back)) => {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                chunk = back;
                std::thread::sleep(QUEUE_RETRY);
            }
        }
    }
}

/// f32 samples as the little-endian bytes ffmpeg's `f32le` expects.
pub fn to_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[cfg(windows)]
type PipeServer = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(not(windows))]
type PipeServer = ();

/// Creates a source's pipe. It must exist before ffmpeg starts, which opens it at once.
#[cfg(windows)]
fn create_pipe(name: &str) -> std::io::Result<PipeServer> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(true)
        .access_inbound(false)
        .access_outbound(true)
        .max_instances(1)
        .reject_remote_clients(true)
        .out_buffer_size(256 * 1024)
        .create(name)
}

#[cfg(not(windows))]
fn create_pipe(_name: &str) -> std::io::Result<PipeServer> {
    Err(std::io::Error::other("audio pipes need Windows"))
}

/// Serves one pipe: waits for ffmpeg to open it, then writes every chunk to it. Ends when
/// the capture side stops or ffmpeg closes its end.
#[cfg(windows)]
async fn serve_pipe(mut server: PipeServer, mut chunks: mpsc::Receiver<Vec<u8>>) {
    use tokio::io::AsyncWriteExt;

    match tokio::time::timeout(CONNECT_TIMEOUT, server.connect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "ffmpeg didn't open an audio pipe");
            return;
        }
        Err(_) => {
            tracing::warn!("ffmpeg didn't open an audio pipe in time");
            return;
        }
    }
    while let Some(chunk) = chunks.recv().await {
        if server.write_all(&chunk).await.is_err() {
            // ffmpeg has finished (or died); the capture side stops soon.
            return;
        }
    }
    let _ = server.flush().await;
}

#[cfg(not(windows))]
async fn serve_pipe(_server: PipeServer, _chunks: mpsc::Receiver<Vec<u8>>) {}

/// Starts every source that opens (the others are reported and left out) with one shared
/// origin, each with its pipe. Windows only; elsewhere there is no audio.
pub async fn start(sources: Vec<SourceConfig>) -> Result<AudioRun, String> {
    if sources.is_empty() {
        return Err("no audio sources".into());
    }
    let origin = super::wasapi::Origin::now();
    let stop = Arc::new(AtomicBool::new(false));
    let mut inputs = Vec::new();
    let mut threads = Vec::new();
    let mut failures = Vec::new();
    for source in sources {
        let (tx, rx) = mpsc::channel(QUEUE_CHUNKS);
        let started = super::wasapi::spawn(source.clone(), origin, tx, stop.clone()).await;
        match started {
            Ok((thread, rate)) => {
                let pipe = pipe_name(source.kind);
                match create_pipe(&pipe) {
                    Ok(server) => {
                        tokio::spawn(serve_pipe(server, rx));
                        inputs.push(AudioInput {
                            kind: source.kind,
                            pipe,
                            rate,
                            volume: source.volume,
                        });
                    }
                    // The thread sees its queue close and stops; it's joined with the rest.
                    Err(error) => {
                        tracing::warn!(kind = ?source.kind, %error, "cannot create an audio pipe");
                        drop(rx);
                    }
                }
                threads.push(thread);
            }
            Err(error) => {
                tracing::warn!(kind = ?source.kind, %error, "audio source unavailable");
                failures.push(SourceReport {
                    kind: source.kind,
                    device: source.device.clone().unwrap_or_default(),
                    stats: TimelineStats::default(),
                    error: Some(error),
                });
            }
        }
    }
    if inputs.is_empty() {
        return Err(failures
            .into_iter()
            .filter_map(|f| f.error)
            .collect::<Vec<_>>()
            .join("; "));
    }
    let stop_all = Box::new(move || -> BoxFuture<'static, Vec<SourceReport>> {
        Box::pin(async move {
            stop.store(true, Ordering::Relaxed);
            let mut reports = failures;
            for thread in threads {
                if let Ok(report) = tokio::task::spawn_blocking(move || thread.join()).await {
                    reports.push(report);
                }
            }
            reports.sort_by_key(|r| r.kind as u8);
            reports
        })
    });
    Ok(AudioRun {
        inputs,
        origin: origin.instant,
        stop: stop_all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE_F: f64 = RATE as f64;

    fn tone(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * usize::from(CHANNELS)]
    }

    fn frames(out: &[f32]) -> usize {
        out.len() / usize::from(CHANNELS)
    }

    #[test]
    fn packets_on_time_pass_straight_through() {
        let mut timeline = Timeline::new(RATE);
        let mut out = Vec::new();
        for i in 0..100 {
            let at = i as f64 * 480.0 / RATE_F; // 10 ms packets, exactly on time
            timeline.place(Some(at), &tone(480, 0.5), &mut out);
        }
        assert_eq!(frames(&out), 48_000);
        assert!(out.iter().all(|s| *s == 0.5));
        let stats = timeline.stats();
        assert_eq!(
            (
                stats.silence_frames,
                stats.dropped_frames,
                stats.repeated_frames
            ),
            (0, 0, 0)
        );
        assert_eq!(stats.frames, 48_000);
    }

    #[test]
    fn gaps_become_silence_and_overlaps_are_dropped() {
        let mut timeline = Timeline::new(RATE);
        let mut out = Vec::new();
        timeline.place(Some(0.0), &tone(480, 1.0), &mut out);
        // Loopback went quiet for 500 ms.
        timeline.place(Some(0.51), &tone(480, 1.0), &mut out);
        assert_eq!(frames(&out), 480 + 24_000 + 480);
        assert_eq!(timeline.stats().silence_frames, 24_000);
        assert!(out[480 * 2..(480 + 24_000) * 2].iter().all(|s| *s == 0.0));
        // A packet stamped 100 ms in the past: its first 100 ms go.
        let before = out.len();
        timeline.place(Some(0.52 - 0.1), &tone(9_600, 1.0), &mut out);
        assert_eq!(frames(&out[before..]), 9_600 - 4_800);
        assert_eq!(timeline.stats().dropped_frames, 4_800);
    }

    #[test]
    fn clock_drift_is_followed_one_frame_at_a_time() {
        // The sound card runs 200 ppm slow: each 10 ms packet is a little late.
        let mut timeline = Timeline::new(RATE);
        let mut out = Vec::new();
        let mut device_frames = 0u64;
        for _ in 0..30_000 {
            // 300 s of packets; QPC time of each packet's first frame.
            let at = device_frames as f64 / RATE_F * (1.0 + 200e-6);
            timeline.place(Some(at), &tone(480, 0.1), &mut out);
            device_frames += 480;
            // Where the packet ended on the clock vs. where the stream has got to.
            let lag = at + 480.0 / RATE_F - timeline.stats().frames as f64 / RATE_F;
            assert!(lag.abs() < 0.0021, "stream drifted {lag} s from the clock");
            out.clear();
        }
        let stats = timeline.stats();
        // 200 ppm of 300 s = 60 ms = 2880 frames repeated, a frame at a time.
        assert!(
            (2_800..=2_900).contains(&stats.repeated_frames),
            "{stats:?}"
        );
        assert_eq!(stats.silence_frames, 0);
    }

    #[test]
    fn idle_fills_to_just_behind_real_time() {
        let mut timeline = Timeline::new(RATE);
        let mut out = Vec::new();
        timeline.idle(0.03, &mut out);
        assert!(out.is_empty(), "within the lag allowance");
        timeline.idle(1.0, &mut out);
        assert_eq!(frames(&out), ((1.0 - IDLE_LAG_S) * RATE_F) as usize);
        // A packet delivered 20 ms after it was captured still lands whole, after the
        // silence up to where it belongs.
        let before = timeline.stats().frames;
        timeline.place(Some(0.98), &tone(480, 1.0), &mut out);
        let gap = (0.98 * RATE_F).round() as u64 - before;
        assert_eq!(timeline.stats().frames - before, gap + 480);
        assert_eq!(timeline.stats().dropped_frames, 0);
    }

    #[test]
    fn untimed_packets_follow_on() {
        let mut timeline = Timeline::new(RATE);
        let mut out = Vec::new();
        timeline.place(None, &tone(480, 1.0), &mut out);
        timeline.place(None, &tone(480, 1.0), &mut out);
        assert_eq!(frames(&out), 960);
    }

    #[test]
    fn device_formats_become_stereo_f32() {
        let mut out = Vec::new();
        let mono: Vec<u8> = [0.25f32, -0.5]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        to_stereo(&mono, SampleFormat::F32, 1, &mut out);
        assert_eq!(out, [0.25, 0.25, -0.5, -0.5]);

        out.clear();
        let stereo16: Vec<u8> = [16_384i16, -32_768]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        to_stereo(&stereo16, SampleFormat::I16, 2, &mut out);
        assert_eq!(out, [0.5, -1.0]);

        out.clear();
        // 5.1: FL FR FC LFE BL BR; the centre goes to both sides.
        let surround: Vec<u8> = [0.1f32, 0.2, 0.5, 0.9, 0.3, 0.3]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        to_stereo(&surround, SampleFormat::F32, 6, &mut out);
        let centre = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - (0.1 + centre)).abs() < 1e-6 && (out[1] - (0.2 + centre)).abs() < 1e-6);

        out.clear();
        to_stereo(
            &i32::MAX.to_le_bytes().repeat(2),
            SampleFormat::I32,
            2,
            &mut out,
        );
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert_eq!(to_bytes(&[1.0]), 1.0f32.to_le_bytes());
    }

    #[test]
    fn pipes_are_unique_and_local() {
        let a = pipe_name(SourceKind::Desktop);
        let b = pipe_name(SourceKind::Desktop);
        assert_ne!(a, b);
        assert!(a.starts_with(r"\\.\pipe\downplayed-") && a.ends_with("-desktop"));
        assert!(pipe_name(SourceKind::Microphone).ends_with("-mic"));
    }

    #[test]
    fn a_full_queue_waits_and_a_stop_ends_the_wait() {
        let (tx, mut rx) = mpsc::channel(1);
        let stop = AtomicBool::new(false);
        assert!(send_chunk(&tx, vec![1], &stop));
        stop.store(true, Ordering::Relaxed);
        assert!(!send_chunk(&tx, vec![2], &stop), "full and stopping");
        assert_eq!(rx.try_recv().unwrap(), [1]);
        drop(rx);
        assert!(!send_chunk(&tx, vec![3], &AtomicBool::new(false)), "closed");
    }
}
