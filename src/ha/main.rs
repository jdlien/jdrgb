//! jdrgb-ha — the LEDs as a Home Assistant light, over MQTT.
//!
//! A resident, windowless process for one configured user. It connects *out* to
//! the house's MQTT broker — nothing listens on the PC — and turns Home
//! Assistant's commands into `jdrgb.exe` invocations, exactly as the tray does.
//! It never touches hardware itself. docs/home-assistant-plan.md is the design;
//! the module docs below say which part of it each one is.
//!
//!   jdrgb-ha.exe                       run, reading %LOCALAPPDATA%\jdrgb\ha.conf
//!   jdrgb-ha.exe --config <file>       run with another config
//!   jdrgb-ha.exe --unpublish [--config <file>]
//!                                      remove the light from Home Assistant

#![windows_subsystem = "windows"]

mod config;
mod lifecycle;
mod log;
mod model;
mod net;
mod payload;
mod slot;
mod unpublish;
mod win;
mod worker;

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::Instant;

use rumqttc::QoS;

use config::Config;
use log::Log;
use model::{Model, Timing};
use slot::Slot;
use worker::{Cli, Disk, TICK, Worker};

/// Milliseconds since this process started, on a monotonic clock — the time
/// every interval in `model` and `lifecycle` is measured on.
pub fn now_ms() -> model::Ms {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as model::Ms
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let unpublish = args.iter().any(|a| a == "--unpublish");
    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from)
        .or_else(config::default_path);
    let log = Arc::new(Log::open(config::dir(), unpublish));

    // One bridge per machine — and `--unpublish` takes the same singleton, so a
    // resident can't start while it is deleting the entity.
    if let Err(e) = win::take_singleton() {
        log.line(&format!("not starting: {e}"));
        return ExitCode::from(2);
    }

    let Some(path) = config_path else {
        log.line("not starting: no LOCALAPPDATA, so no ha.conf");
        return ExitCode::FAILURE;
    };
    let cfg = match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            log.line(&format!("not starting: {e}"));
            return ExitCode::FAILURE;
        }
    };

    if unpublish {
        return match unpublish::run(&cfg, &log) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log.line(&format!("unpublish failed: {e} — delete the device under HA's MQTT integration instead"));
                ExitCode::FAILURE
            }
        };
    }

    // Its children die with it — and if that can't be arranged, it doesn't run
    // with children nobody would be watching.
    if let Err(e) = win::join_kill_on_close_job() {
        log.line(&format!("not starting: {e}"));
        return ExitCode::FAILURE;
    }

    log.line(&format!("jdrgb-ha {} starting for \"{}\"", env!("CARGO_PKG_VERSION"), cfg.name));
    // Exactly what the broker's owner needs, so a mismatch is visible here
    // rather than derived by hand and got wrong.
    log.line(&format!("broker ACL needed: {}", cfg.acl_lines().join(" | ")));
    let (client, connection) = net::client(&cfg);
    let slot = Arc::new(Slot::default());
    let ready = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();

    let worker = Worker {
        model: Model::new(Timing::default(), now_ms()),
        runner: Cli,
        observer: Disk,
        slot: slot.clone(),
        ready: ready.clone(),
        publish: {
            let (client, topic) = (client.clone(), cfg.topic("state"));
            Box::new(move |p: &str| client.try_publish(topic.clone(), QoS::AtLeastOnce, true, p.as_bytes().to_vec()).is_ok())
        },
        mode: cfg.mode,
        log: log.clone(),
        tick: TICK,
    };
    let worker = std::thread::spawn(move || worker.run(rx));

    net::run(connection, client, cfg, ready, slot, tx, log.clone());
    let _ = worker.join();
    log.line("stopped");
    ExitCode::SUCCESS
}
