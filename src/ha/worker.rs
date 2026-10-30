//! The worker thread: the model's decisions, carried out.
//!
//! It owns the model, runs one child at a time, and publishes the projection
//! through the latest-state slot. A 90-second child blocks only this thread; the
//! network thread keeps the connection alive meanwhile.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};

use jdrgb::palette::{current_stash_identity, load_state_full};
use jdrgb::spawn::Output;

use crate::config::Mode;
use crate::log::Log;
use crate::model::{Message, Model, Ms, Observed, Step, project};
use crate::slot::Slot;
use crate::{now_ms, payload, win};

/// How often the worker looks at the state file when nothing else wakes it.
pub const TICK: Ms = 1_000;

/// Runs a leg's `jdrgb.exe`.
pub trait Runner {
    fn run(&mut self, args: &[String]) -> Output;
}

/// Reads what is on disk.
pub trait Observer {
    fn observe(&mut self) -> Observed;
}

/// The real CLI, beside this binary.
pub struct Cli;

impl Runner for Cli {
    fn run(&mut self, args: &[String]) -> Output {
        jdrgb::spawn::run(args)
    }
}

/// The real state file, stash and GPU marker.
pub struct Disk;

impl Observer for Disk {
    fn observe(&mut self) -> Observed {
        Observed { state: load_state_full(), stash: current_stash_identity(), gpu_marker_age: win::gpu_marker_age_ms() }
    }
}

pub struct Worker<R, O> {
    pub model: Model,
    pub runner: R,
    pub observer: O,
    pub slot: Arc<Slot>,
    pub ready: Arc<AtomicBool>,
    /// Hands a state payload to the client; `true` if accepted.
    pub publish: Box<dyn Fn(&str) -> bool + Send>,
    pub mode: Mode,
    pub log: Arc<Log>,
    pub tick: Ms,
}

impl<R: Runner, O: Observer> Worker<R, O> {
    fn publish_state(&self, obs: &Observed) {
        let p = payload::state(&project(&obs.state, self.model.follow_pending()), self.mode);
        self.slot.update(p, self.ready.load(Ordering::SeqCst), |s| (self.publish)(s));
    }

    /// Act on everything that's due, publishing as things change.
    fn step(&mut self) {
        let mut obs = self.observer.observe();
        for _ in 0..16 {
            match self.model.next(&obs, now_ms()) {
                Step::Run(leg) => {
                    let args = leg.args();
                    self.log.line(&format!("jdrgb {}", args.join(" ")));
                    let out = self.runner.run(&args);
                    if !out.ok {
                        self.log.line(&format!("  failed: {}", out.message));
                    } else if !out.stderr.is_empty() {
                        // A success can still carry a warning — notably a
                        // change that landed without being recorded.
                        self.log.line(&format!("  warning: {}", out.stderr));
                    }
                    obs = self.observer.observe();
                    self.model.on_done(&obs, now_ms());
                    self.publish_state(&obs);
                }
                Step::Note(note) => {
                    self.log.line(&note);
                    self.publish_state(&obs);
                }
                Step::Idle => break,
            }
        }
        self.publish_state(&obs);
    }

    /// Run until the network thread hangs up.
    pub fn run(mut self, commands: Receiver<Message>) {
        loop {
            self.step();
            match commands.recv_timeout(std::time::Duration::from_millis(self.model.wake_in(now_ms(), self.tick))) {
                Ok(msg) => {
                    self.model.on_message(&msg, now_ms());
                    while let Ok(more) = commands.try_recv() {
                        self.model.on_message(&more, now_ms());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Look, Timing};
    use jdrgb::palette::{Last, Paint, State, scale};
    use std::sync::Mutex;

    /// A stand-in for jdrgb.exe: records the arguments it was given and what
    /// the state file would say afterwards.
    #[derive(Clone, Default)]
    struct Fake {
        state: Arc<Mutex<State>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Runner for Fake {
        fn run(&mut self, args: &[String]) -> Output {
            self.calls.lock().unwrap().push(args.join(" "));
            let gpu = args.iter().any(|a| a == "--gpu");
            let mut st = self.state.lock().unwrap();
            let last = match args[0].as_str() {
                "off" => Last::Off,
                "on" => match st.lit(gpu) {
                    Some(jdrgb::palette::Lit::Color(rgb)) => Last::Color(*rgb),
                    _ => return Output { ok: true, message: String::new(), stderr: String::new() },
                },
                name => {
                    let name = jdrgb::palette::PRESETS.iter().find(|(n, _)| *n == name).unwrap().0;
                    let level = args.iter().position(|a| a == "--level").map_or(100, |i| args[i + 1].parse().unwrap());
                    Last::Color(scale(Paint::Preset(name).rgb(gpu), level))
                }
            };
            st.record(gpu, last);
            st.rev += 1;
            Output { ok: true, message: String::new(), stderr: "a warning".into() }
        }
    }

    impl Observer for Fake {
        fn observe(&mut self) -> Observed {
            Observed { state: self.state.lock().unwrap().clone(), ..Default::default() }
        }
    }

    /// The real loop, with a fake CLI, a fake broker and the plan's intervals
    /// shrunk to milliseconds: a command reaches the strip, the card follows,
    /// the published state goes from OFF to Red, and a success's stderr is
    /// logged rather than lost.
    #[test]
    fn the_loop_runs_the_strip_then_the_card_and_publishes_as_it_goes() {
        let fake = Fake::default();
        {
            let mut st = fake.state.lock().unwrap();
            st.record(false, Last::Off);
            st.record(true, Last::Off);
            st.rev = 1;
        }
        let published = Arc::new(Mutex::new(Vec::<String>::new()));
        let dir = std::env::temp_dir().join(format!("jdrgb-ha-worker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Arc::new(Log::open(Some(dir.clone()), false));
        let timing = Timing { settle: 5, quiet: 30, gpu_interval: 60, repeat_window: 50, refill: 1_000, off_allowance: 1_000 };
        let worker = Worker {
            model: Model::new(timing, now_ms()),
            runner: fake.clone(),
            observer: fake.clone(),
            slot: Arc::new(Slot::default()),
            ready: Arc::new(AtomicBool::new(true)),
            publish: {
                let published = published.clone();
                Box::new(move |p: &str| {
                    published.lock().unwrap().push(p.to_string());
                    true
                })
            },
            mode: Mode::Wheel,
            log,
            tick: 5,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || worker.run(rx));
        tx.send(Message { on: Some(true), look: Some(Look::Effect("red")), brightness: None }).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fake.calls.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(tx);
        handle.join().unwrap();

        assert_eq!(*fake.calls.lock().unwrap(), ["red", "red --gpu"]);
        let published = published.lock().unwrap();
        assert!(published.first().unwrap().contains("\"OFF\""), "{published:?}");
        assert!(published.last().unwrap().contains("\"Red\""), "{published:?}");
        let log = std::fs::read_to_string(dir.join("ha.log")).unwrap();
        assert!(log.contains("warning: a warning"), "a success's stderr was lost: {log}");
    }
}
