//! `jdrgb-ha.exe --unpublish`: remove the light from Home Assistant.
//!
//! Run by uninstall.ps1 once the resident bridge has been stopped. Empty retained
//! payloads on the discovery, state and availability topics delete the entity and
//! leave nothing retained behind. See docs/home-assistant-plan.md, *Remove*.

use std::time::{Duration, Instant};

use rumqttc::{Client, Event, MqttOptions, Outgoing, Packet, QoS};

use crate::config::Config;
use crate::log::Log;

/// One deadline for the whole operation: connect, three acknowledged
/// publishes, and the disconnect.
pub const DEADLINE: Duration = Duration::from_secs(15);

pub fn run(cfg: &Config, log: &Log) -> Result<(), String> {
    // Its own client id, so it can't kick a resident off the broker — though by
    // the time this runs, uninstall has stopped the resident and this process
    // holds the singleton, so none can start mid-cleanup.
    let mut opts = MqttOptions::new(format!("{}-cleanup", cfg.client_id()), cfg.host.clone(), cfg.port);
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_clean_session(true);
    if let Some(user) = &cfg.user {
        opts.set_credentials(user.clone(), cfg.password.clone().unwrap_or_default());
    }
    // No will, and none of the normal startup: nothing here may republish the
    // entity it is deleting.
    let (client, mut conn) = Client::new(opts, 10);

    let topics = [cfg.discovery_topic(), cfg.topic("state"), cfg.topic("availability")];
    let deadline = Instant::now() + DEADLINE;
    let mut queued = false;
    let mut sent: Vec<u16> = Vec::new();
    let mut acked = 0usize;
    let mut disconnecting = false;

    loop {
        let left = deadline.checked_duration_since(Instant::now()).ok_or("timed out")?;
        // A long receive is safe here, unlike in the resident: if it fires, the
        // whole operation is past its deadline anyway.
        let event = match conn.recv_timeout(left) {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => return Err(format!("broker connection: {e}")),
            Err(_) => return Err("timed out".into()),
        };
        match event {
            Event::Incoming(Packet::ConnAck(_)) if !queued => {
                for t in &topics {
                    client.try_publish(t.clone(), QoS::AtLeastOnce, true, Vec::new()).map_err(|e| e.to_string())?;
                }
                queued = true;
            }
            Event::Outgoing(Outgoing::Publish(pkid)) => sent.push(pkid),
            Event::Incoming(Packet::PubAck(ack)) if sent.contains(&ack.pkid) => {
                acked += 1;
                if acked == topics.len() && !disconnecting {
                    client.try_disconnect().map_err(|e| e.to_string())?;
                    disconnecting = true;
                }
            }
            // Per MQTT the client closes the connection after DISCONNECT; it
            // doesn't wait for the broker to. Seeing it go out is the end.
            Event::Outgoing(Outgoing::Disconnect) => {
                log.line(&format!("unpublished: {}", topics.join(", ")));
                return Ok(());
            }
            _ => {}
        }
    }
}
