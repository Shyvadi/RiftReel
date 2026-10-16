//! Capture (doc 03 §5–6): video and sound of each match, straight to disk.
//!
//! An ffmpeg sidecar records the monitor League runs on with Desktop Duplication
//! (`ddagrab`: nothing is injected into the game, which matters under Vanguard) and a
//! hardware encoder, straight to a fragmented MP4 in the match's library folder. Sound is
//! captured in Rust with WASAPI and handed to the same ffmpeg over named pipes; ffmpeg
//! mixes it into track 0 as it records. When the match ends the file is remuxed (stream
//! copy) into a regular MP4 so the review opens it instantly, with the audio moved into
//! sync.
//!
//! - [`ffmpeg`]: finding the binary, its command lines, `-progress` reports, the process
//! - [`probe`]: which encoder works here (NVENC → AMF → QSV → CPU), found once and stored
//! - [`display`]: the DXGI adapter and output of League's monitor
//! - [`audio`]: sources, the QPC timeline that keeps them locked to the video, the pipes
//! - [`wasapi`]: the capture threads (loopback, microphone, League's process loopback)
//! - [`clips`]: each moment cut from the video (stream copy) with a thumbnail
//! - [`controller`]: start with the match, stop at its end, hand back the recording

pub mod audio;
pub mod clips;
pub mod controller;
pub mod display;
pub mod ffmpeg;
pub mod probe;
pub mod wasapi;

pub use controller::{
    AudioInfo, CaptureConfig, CaptureController, CaptureState, CaptureStatus, Recording,
    RecordingInfo, StartRequest,
};
