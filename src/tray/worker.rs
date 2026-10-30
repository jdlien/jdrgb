//! Runs `jdrgb.exe` off the UI thread.
//!
//! The running itself — the time bound, the stdio wiring, finding the CLI — is
//! `jdrgb::spawn`, shared with the Home Assistant bridge. What is the tray's own
//! is the queue, and handing each result back to the window.

use std::sync::mpsc::{Sender, channel};

use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

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
/// controller at once is a race with no winner worth having. (The CLI now also
/// serialises across processes, but there is no point queueing a click behind a
/// lock it will only time out on.)
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
    let out = jdrgb::spawn::run(&job.args);
    Outcome { label: job.label.clone(), ok: out.ok, message: out.message }
}
