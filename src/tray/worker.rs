//! Runs `jdrgb.exe` off the UI thread.
//!
//! The tray never touches the hardware itself. That keeps the shared code down
//! to colour tables, and it means a wedged HID enumeration can't take the menu
//! with it — but only if the child is time-bounded, because one worker feeding a
//! queue would otherwise stall behind it forever. See `CHILD_TIMEOUT`.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

/// No console window for the child. Without this every menu click flashes one.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Comfortably above the CLI's own `--wait` ceiling (120 tries x 500ms = 60s),
/// so a legitimately slow controller wake finishes instead of being killed,
/// while a genuinely hung child still releases the queue.
const CHILD_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Job {
    pub args: Vec<String>,
    /// What the menu called this, echoed back so the balloon can name it.
    pub label: String,
}

pub struct Outcome {
    pub label: String,
    pub ok: bool,
    /// Empty on success. On failure, the CLI's own stderr where there was any —
    /// it is already written for humans.
    pub message: String,
}

/// Start the worker. Returns the queue; dropping it ends the thread.
///
/// Jobs are serialised deliberately: two `jdrgb.exe` processes writing the same
/// controller at once is a race with no winner worth having.
pub fn spawn(hwnd: HWND, done_msg: u32) -> Sender<Job> {
    let (tx, rx) = channel::<Job>();

    // HWND isn't Send, but the numeric handle is, and the window outlives the
    // worker: the message loop only exits after the queue is dropped.
    let target = hwnd as usize;

    std::thread::spawn(move || {
        for job in rx {
            let boxed = Box::into_raw(Box::new(run(&job)));
            let posted = unsafe { PostMessageW(target as HWND, done_msg, 0, boxed as LPARAM) };
            if posted == 0 {
                // Window already gone; we still own the box.
                drop(unsafe { Box::from_raw(boxed) });
            }
        }
    });

    tx
}

fn run(job: &Job) -> Outcome {
    run_with(&cli_path(), job)
}

/// Split from `run` so tests can point it at a known binary: a test executable
/// lives in `deps/`, so `cli_path`'s "next to me" rule doesn't find the CLI.
fn run_with(exe: &std::path::Path, job: &Job) -> Outcome {
    let fail = |message: String| Outcome {
        label: job.label.clone(),
        ok: false,
        message,
    };

    let mut child = match Command::new(exe)
        .args(&job.args)
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

    // Safe to read only now: jdrgb's diagnostics are one short line, far below
    // the pipe buffer, so nothing can have blocked on a full pipe.
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let stderr = stderr.trim().to_string();

    match status {
        None => fail(format!(
            "timed out after {}s and was stopped",
            CHILD_TIMEOUT.as_secs()
        )),
        Some(s) if s.success() => Outcome {
            label: job.label.clone(),
            ok: true,
            message: String::new(),
        },
        Some(s) => fail(if stderr.is_empty() {
            format!("jdrgb.exe exited with {}", s.code().unwrap_or(-1))
        } else {
            stderr
        }),
    }
}

/// Prefer the CLI sitting next to us — install.ps1 puts both in one directory —
/// and fall back to PATH for a development build run from anywhere.
fn cli_path() -> PathBuf {
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

    fn job(arg: &str) -> Job {
        Job { args: vec![arg.to_string()], label: arg.to_string() }
    }

    /// End-to-end over the real plumbing — spawn flags, stdio wiring, exit
    /// handling — using a subcommand that touches no hardware.
    #[test]
    fn a_successful_run_reports_ok() {
        let out = run_with(&built_cli(), &job("--version"));
        assert!(out.ok, "expected success, got: {}", out.message);
        assert!(out.message.is_empty());
        assert_eq!(out.label, "--version");
    }

    /// The balloon shows the CLI's own words, so the pipe has to actually carry
    /// them. Piping stderr while discarding stdout is easy to get backwards.
    #[test]
    fn a_failing_run_carries_the_clis_own_stderr() {
        let out = run_with(&built_cli(), &job("not-a-color"));
        assert!(!out.ok, "a bad colour name should fail");
        assert!(
            out.message.contains("not a color"),
            "stderr did not survive the pipe: {:?}",
            out.message
        );
    }

    /// A missing binary must produce a message rather than a silent no-op —
    /// this is what the user sees if jdrgb.exe isn't installed beside us.
    #[test]
    fn a_missing_binary_is_reported_not_swallowed() {
        let out = run_with(std::path::Path::new("no-such-jdrgb.exe"), &job("red"));
        assert!(!out.ok);
        assert!(out.message.contains("could not run"), "{}", out.message);
    }
}
