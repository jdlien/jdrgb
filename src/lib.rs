//! The parts of jdrgb that more than one binary needs.
//!
//! `jdrgb.exe` (console) is the whole CLI. `jdrgb-tray.exe` (a notification-area
//! menu) and `jdrgb-ha.exe` (a Home Assistant bridge) are windows-subsystem
//! programs that shell out to it. They can't be one binary because a PE has
//! exactly one subsystem, so what they share lives here.
//!
//! This is deliberately *only* colors, state, and how to run the CLI — not the
//! device layer. The tray and the bridge never touch hardware; they spawn the CLI
//! and read what the CLI recorded. See docs/tray-plan.md for why.

pub mod palette;
pub mod spawn;
