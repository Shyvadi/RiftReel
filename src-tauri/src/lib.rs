//! Downplayed: a lightweight, local-first League of Legends clip recorder.
//!
//! This crate is the Rust core: the tray app shell, settings, scrubbed logging, Riot's
//! local API clients ([`league`]) and the highlight tracker ([`tracker`]). Capture
//! (phase 2) and the library (phase 3) build on top of it.

pub mod league;
pub mod logging;
pub mod privacy;
pub mod recorder;
pub mod replay;
pub mod settings;
pub mod tracker;

mod app;
mod tray;

pub use app::run;
