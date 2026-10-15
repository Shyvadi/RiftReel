//! Highlight tracker: Riot events → markers → moments, driven by the match state
//! machine (doc 02 Part B).
//!
//! - [`normalize`]: Live Client events → markers (Outplayed's windows by default)
//! - [`streaks`]: sprees and shutdowns derived from the kill sequence
//! - [`moments`]: overlapping marker windows merged and labelled by priority
//! - [`clock`]: game time ↔ wall clock alignment
//! - [`machine`]: Idle → Armed → InGame → Finalizing, reconnect-safe
//! - [`session`]: one match's pipeline; [`runtime`]: the async loop around it

pub mod clock;
pub mod machine;
pub mod moments;
pub mod normalize;
pub mod roster;
pub mod runtime;
pub mod session;
pub mod streaks;
