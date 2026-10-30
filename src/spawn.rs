//! Running `jdrgb.exe` from a resident process: the tray, and the Home Assistant
//! bridge.
//!
//! Neither touches the hardware itself. That keeps the shared code down to colour
//! tables and this, and it means a wedged HID enumeration can't take a resident
//! process with it — but only if the child is time-bounded, because one worker
//! feeding a queue would otherwise stall behind it forever. See `CHILD_TIMEOUT`.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// No console window for the child. Without this every menu click flashes one.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The parent's own ceiling on a child. Comfortably above what the CLI's
/// `--wait` usually needs — 120 tries x 500 ms, plus each attempt's own work —
/// so a legitimately slow controller wake finishes instead of being killed,
/// while a genuinely hung child still releases the queue.
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(90);

/// How a child run ended.
pub struct Output {
    pub ok: bool,
    /// Empty on success. On failure, the CLI's own stderr where there was any —
    /// it is already written for humans — or what went wrong running it.
    pub message: String,
    /// The child's stderr, trimmed, **on success as well as failure**. A
    /// successful run can still have something to say: a hardware apply that
    /// landed while its record didn't is exit 0 with a warning, and it is the
    /// reason a reader of the state file would then be wrong. The tray ignores
    /// this; the bridge logs it.
    pub stderr: String,
}

/// Run the CLI with `args`, bounded by `CHILD_TIMEOUT`.
pub fn run(args: &[String]) -> Output {
    run_with(&cli_path(), args)
}

/// Split from `run` so tests can point it at a known binary: a test executable
/// lives in `deps/`, so `cli_path`'s "next to me" rule doesn't find the CLI.
pub fn run_with(exe: &Path, args: &[String]) -> Output {
    let fail = |message: String| Output { ok: false, message, stderr: String::new() };

    let mut child = match Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        // Discarded rather than inherited: a windows-subsystem parent has no
        // console to inherit, and Stdio::null() hands the child a valid handle.
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("could not run jdrgb.exe: {e}")),
    };

    let deadline = Instant::now() + CHILD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => return fail(format!("lost track of jdrgb.exe: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    // Safe to read only now: jdrgb's diagnostics are a line or two, far below
    // the pipe buffer, so nothing can have blocked on a full pipe.
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let stderr = stderr.trim().to_string();

    match status {
        None => Output {
            ok: false,
            message: format!("timed out after {}s and was stopped", CHILD_TIMEOUT.as_secs()),
            stderr,
        },
        Some(s) if s.success() => Output { ok: true, message: String::new(), stderr },
        Some(s) => Output {
            ok: false,
            message: if stderr.is_empty() {
                format!("jdrgb.exe exited with {}", s.code().unwrap_or(-1))
            } else {
                stderr.clone()
            },
            stderr,
        },
    }
}

/// Prefer the CLI sitting next to us — install.ps1 puts them all in one
/// directory — and fall back to PATH for a development build run from anywhere.
pub fn cli_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("jdrgb.exe");
            if beside.is_file() {
                return beside;
            }
        }
    }
    PathBuf::from("jdrgb.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI as built by this same `cargo test` run. A test binary sits in
    /// `target/<profile>/deps/`, so walk up until jdrgb.exe turns up.
    fn built_cli() -> PathBuf {
        let mut dir = std::env::current_exe().expect("current_exe");
        while dir.pop() {
            let candidate = dir.join("jdrgb.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!("jdrgb.exe not found; run `cargo build` first");
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// End-to-end over the real plumbing — spawn flags, stdio wiring, exit
    /// handling — using a subcommand that touches no hardware.
    #[test]
    fn a_successful_run_reports_ok() {
        let out = run_with(&built_cli(), &args(&["--version"]));
        assert!(out.ok, "expected success, got: {}", out.message);
        assert!(out.message.is_empty());
    }

    /// The balloon shows the CLI's own words, so the pipe has to actually carry
    /// them. Piping stderr while discarding stdout is easy to get backwards.
    #[test]
    fn a_failing_run_carries_the_clis_own_stderr() {
        let out = run_with(&built_cli(), &args(&["not-a-color"]));
        assert!(!out.ok, "a bad colour name should fail");
        assert!(out.message.contains("not a color"), "stderr did not survive the pipe: {:?}", out.message);
        assert_eq!(out.message, out.stderr);
    }

    /// A successful child's warning must survive too: it is how the bridge
    /// learns that an apply landed while its record didn't. `cmd /c` stands in
    /// for such a child — harmless, exit 0, one line on stderr.
    #[test]
    fn a_successful_childs_stderr_is_kept() {
        let out = run_with(Path::new("cmd.exe"), &args(&["/c", "echo record skipped 1>&2"]));
        assert!(out.ok, "{}", out.message);
        assert!(out.message.is_empty(), "a success carried a failure message");
        assert_eq!(out.stderr, "record skipped");
    }

    /// A missing binary must produce a message rather than a silent no-op —
    /// this is what the user sees if jdrgb.exe isn't installed beside us.
    #[test]
    fn a_missing_binary_is_reported_not_swallowed() {
        let out = run_with(Path::new("no-such-jdrgb.exe"), &args(&["red"]));
        assert!(!out.ok);
        assert!(out.message.contains("could not run"), "{}", out.message);
    }
}
