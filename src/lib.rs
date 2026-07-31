//! The parts of jdrgb that both binaries need.
//!
//! `jdrgb.exe` (console) is the whole CLI; `jdrgb-tray.exe` (windows subsystem)
//! is a notification-area menu that shells out to it. They can't be one binary
//! because a PE has exactly one subsystem, so what they share lives here.
//!
//! This is deliberately *only* colors and state — not the device layer. The
//! tray never touches hardware; it spawns the CLI and reads what the CLI
//! recorded. See docs/tray-plan.md for why.

pub mod palette;
