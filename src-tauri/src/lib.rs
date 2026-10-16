//! Downplayed: a lightweight, local-first League of Legends clip recorder.
//!
//! This crate is the Rust core: the tray app shell, settings, scrubbed logging, Riot's
//! local API clients ([`league`]), the highlight tracker ([`tracker`]), video capture
//! through an ffmpeg sidecar ([`capture`]) and the match library ([`library`]).

pub mod capture;
pub mod league;
pub mod library;
pub mod logging;
pub mod privacy;
pub mod recorder;
pub mod replay;
pub mod settings;
pub mod tracker;

mod app;
mod tray;

pub use app::run;
