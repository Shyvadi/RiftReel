//! Video capture (doc 03 §5–6), milestone 1: `full_match` mode, video only.
//!
//! An ffmpeg sidecar records the monitor League runs on with Desktop Duplication
//! (`ddagrab`: nothing is injected into the game, which matters under Vanguard) and a
//! hardware encoder, straight to a fragmented MP4 in the match's library folder. When
//! the match ends the file is remuxed (stream copy) into a regular MP4 so the review
//! opens it instantly.
//!
//! - [`ffmpeg`]: finding the binary, its command lines, `-progress` reports, the process
//! - [`probe`]: which encoder works here (NVENC → AMF → QSV → CPU), found once and stored
//! - [`display`]: the DXGI adapter and output of League's monitor
//! - [`controller`]: start with the match, stop at its end, hand back the recording
//!
//! Later milestones: audio (WASAPI loopback + microphone as extra ffmpeg inputs), the
//! `highlights`/`manual` disk ring buffer of MPEG-TS segments, and clip export.

pub mod controller;
pub mod display;
pub mod ffmpeg;
pub mod probe;

pub use controller::{
    CaptureConfig, CaptureController, CaptureState, CaptureStatus, Recording, RecordingInfo,
    StartRequest,
};
