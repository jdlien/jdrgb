//! The network thread: `rumqttc`'s connection, and the lifecycle around it.
//!
//! It runs the blocking `connection.iter()` — never `recv_timeout`, which drops
//! the poll future when it fires and with it any handshake still in flight — and
//! makes only non-blocking `try_*` calls, so it can never wait on the request
//! queue it is the one to drain. See docs/home-assistant-plan.md, *Threads*.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use rumqttc::{Client, Connection, Event, LastWill, MqttOptions, Outgoing, Packet, QoS, SubscribeFilter, SubscribeReasonCode};

use crate::config::{BIRTH_TOPIC, Config};
use crate::lifecycle::{Arrived, Lifecycle, Request, TopicKind};
use crate::log::Log;
use crate::model::Message;
use crate::slot::Slot;
use crate::{now_ms, payload};

/// Keepalive: often enough that a sleeping PC goes `unavailable` within ~45 s,
/// rare enough to be nothing on the wire.
pub const KEEP_ALIVE: Duration = Duration::from_secs(30);

/// The request queue between the client handles and the event loop.
pub const QUEUE: usize = 32;

/// The client and its connection, configured but not yet connected.
pub fn client(cfg: &Config) -> (Client, Connection) {
    let mut opts = MqttOptions::new(cfg.client_id(), cfg.host.clone(), cfg.port);
    opts.set_keep_alive(KEEP_ALIVE);
    // A clean session: commands sent while the bridge is away are not queued
    // for it. An hour-old `off` replayed on wake would be worse than lost.
    opts.set_clean_session(true);
    opts.set_last_will(LastWill::new(cfg.topic("availability"), "offline", QoS::AtLeastOnce, true));
    if let Some(user) = &cfg.user {
        opts.set_credentials(user.clone(), cfg.password.clone().unwrap_or_default());
    }
    Client::new(opts, QUEUE)
}

fn kind(cfg: &Config, topic: &str) -> TopicKind {
    if topic == cfg.topic("set") {
        TopicKind::Command
    } else if topic == cfg.topic("echo") {
        TopicKind::Echo
    } else if topic == BIRTH_TOPIC {
        TopicKind::Birth
    } else {
        TopicKind::Other
    }
}

/// Hand one lifecycle request to the client. `true` if it was accepted.
fn send(client: &Client, cfg: &Config, slot: &Slot, req: &Request) -> bool {
    let publish = |topic: String, qos: QoS, retain: bool, payload: Vec<u8>| {
        client.try_publish(topic, qos, retain, payload).is_ok()
    };
    match req {
        Request::Availability(on) => publish(
            cfg.topic("availability"),
            QoS::AtLeastOnce,
            true,
            (if *on { "online" } else { "offline" }).into(),
        ),
        Request::Subscribe => client
            .try_subscribe_many(vec![
                SubscribeFilter::new(cfg.topic("set"), QoS::AtMostOnce),
                SubscribeFilter::new(cfg.topic("echo"), QoS::AtMostOnce),
                SubscribeFilter::new(BIRTH_TOPIC.into(), QoS::AtMostOnce),
            ])
            .is_ok(),
        Request::Echo(nonce) => publish(cfg.topic("echo"), QoS::AtMostOnce, false, nonce.clone().into_bytes()),
        Request::Discovery => publish(cfg.discovery_topic(), QoS::AtLeastOnce, true, payload::discovery(cfg).into_bytes()),
        Request::State => slot.publish_current(|p| publish(cfg.topic("state"), QoS::AtLeastOnce, true, p.as_bytes().to_vec())),
    }
}

/// Run the connection forever. Returns only if every client handle is gone.
pub fn run(
    mut connection: Connection,
    client: Client,
    cfg: Config,
    ready: Arc<AtomicBool>,
    slot: Arc<Slot>,
    worker: Sender<Message>,
    log: Arc<Log>,
) {
    let seed = format!("{}-{}", now_ms(), std::process::id());
    let mut lc = Lifecycle::new(seed);
    log.line(&format!("connecting to {}:{} as {}", cfg.host, cfg.port, cfg.client_id()));

    // An explicit `recv()` loop rather than `iter()`, so the connection can be
    // reached between polls — see the error branch.
    loop {
        // `recv` errs only once every client handle is gone: nothing left to do.
        let Ok(event) = connection.recv() else { return };
        let now = now_ms();
        match event {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                lc.on_connack();
                log.reset_once();
                log.line("connected to the broker");
            }
            Ok(Event::Outgoing(Outgoing::Subscribe(pkid))) => lc.on_outgoing_subscribe(pkid),
            Ok(Event::Incoming(Packet::SubAck(ack))) => {
                let granted: Vec<bool> =
                    ack.return_codes.iter().map(|c| matches!(c, SubscribeReasonCode::Success(_))).collect();
                if let Some(note) = lc.on_suback(ack.pkid, &granted, now) {
                    log.line(&note);
                }
            }
            Ok(Event::Incoming(Packet::Publish(p))) => match lc.on_publish(kind(&cfg, &p.topic), &p.payload, p.retain) {
                Arrived::Command => match payload::parse_command(&p.payload, cfg.mode) {
                    Ok((msg, notes)) => {
                        for n in notes {
                            log.line(&n);
                        }
                        if worker.send(msg).is_err() {
                            return; // the worker is gone; so is the point
                        }
                    }
                    Err(e) => log.line(&format!("ignored a command: {e}")),
                },
                Arrived::RetainedCommand => log.line("ignored a retained message on the command topic"),
                Arrived::Echo | Arrived::Birth | Arrived::Other => {}
            },
            Ok(_) => {}
            Err(e) => {
                ready.store(false, Ordering::SeqCst);
                lc.on_down();
                // rumqttc keeps events it buffered for the dead connection and
                // hands them out after reconnecting — `MqttState::clean` doesn't
                // clear them. A stale `Outgoing::Subscribe` would then be taken
                // for the new connection's, and its real SUBACK ignored. Nothing
                // the old connection queued means anything now.
                connection.eventloop.state.events.clear();
                log.once(&format!("broker connection: {e}"));
                // rumqttc reconnects on the next poll with no delay of its own.
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        }

        if let Some(note) = lc.tick(now) {
            log.line(&note);
        }
        while let Some(req) = lc.pending() {
            if !send(&client, &cfg, &slot, &req) {
                break; // the queue is full; this step is offered again next event
            }
            lc.accepted(now);
        }
        let was = ready.swap(lc.is_ready(), Ordering::SeqCst);
        if lc.is_ready() && !was {
            log.line("online: the echo came back, discovery and state are published");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use rumqttc::{ConnAck, ConnectReturnCode, PingResp, PubAck, Publish, SubAck};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::sync::mpsc::Receiver;

    /// How the fake broker behaves — the cases this house's broker can present.
    #[derive(Clone, Copy, PartialEq)]
    enum Broker {
        /// Grants everything and routes the bridge's echo back to it.
        Healthy,
        /// Grants the subscription but delivers nothing on it — Mosquitto 2.0.22
        /// with a missing `read` in its ACL, as measured on the house's broker.
        GrantsButDeaf,
        /// Refuses the command subscription outright, as some brokers do.
        Refuses,
        /// Drops the first connection the moment it sees the SUBSCRIBE, then
        /// behaves — a reconnect in the middle of the lifecycle.
        DropsFirstSubscribe,
    }

    #[derive(Default)]
    struct Seen {
        connect: Option<rumqttc::Connect>,
        connects: usize,
        /// (topic, payload, retain) in the order they arrived.
        publishes: Vec<(String, String, bool)>,
        filters: Vec<String>,
    }

    /// A broker for one client at a time, framed with rumqttc's codec. The
    /// returned sender pushes a publish to the client once it subscribes.
    fn fake_broker(mode: Broker) -> (u16, Arc<Mutex<Seen>>, std::sync::mpsc::Sender<Publish>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (inject_tx, inject_rx) = std::sync::mpsc::channel::<Publish>();
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for (n, conn) in listener.incoming().enumerate() {
                let Ok(mut sock) = conn else { return };
                sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
                let this = if mode == Broker::DropsFirstSubscribe && n > 0 { Broker::Healthy } else { mode };
                serve(&mut sock, this, &seen2, &inject_rx);
            }
        });
        (port, seen, inject_tx)
    }

    fn send_packet(sock: &mut TcpStream, write: impl FnOnce(&mut BytesMut)) {
        let mut out = BytesMut::new();
        write(&mut out);
        let _ = sock.write_all(&out);
    }

    fn serve(sock: &mut TcpStream, mode: Broker, seen: &Mutex<Seen>, inject: &Receiver<Publish>) {
        let mut buf = BytesMut::new();
        let mut chunk = [0u8; 4096];
        let mut subscribed = false;
        loop {
            if subscribed {
                while let Ok(p) = inject.try_recv() {
                    send_packet(sock, |o| {
                        p.write(o).unwrap();
                    });
                }
            }
            match sock.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                Err(_) => return,
            }
            while let Ok(packet) = Packet::read(&mut buf, 1 << 20) {
                match packet {
                    Packet::Connect(c) => {
                        let mut seen = seen.lock().unwrap();
                        seen.connect = Some(c);
                        seen.connects += 1;
                        drop(seen);
                        send_packet(sock, |o| {
                            ConnAck::new(ConnectReturnCode::Success, false).write(o).unwrap();
                        });
                    }
                    Packet::Subscribe(_) if mode == Broker::DropsFirstSubscribe => return,
                    Packet::Subscribe(s) => {
                        let codes = (0..s.filters.len())
                            .map(|i| {
                                if mode == Broker::Refuses && i == 0 {
                                    SubscribeReasonCode::Failure
                                } else {
                                    SubscribeReasonCode::Success(QoS::AtMostOnce)
                                }
                            })
                            .collect();
                        seen.lock().unwrap().filters = s.filters.iter().map(|f| f.path.clone()).collect();
                        send_packet(sock, |o| {
                            SubAck::new(s.pkid, codes).write(o).unwrap();
                        });
                        subscribed = true;
                    }
                    Packet::Publish(p) => {
                        let text = String::from_utf8_lossy(&p.payload).to_string();
                        seen.lock().unwrap().publishes.push((p.topic.clone(), text, p.retain));
                        if p.qos == QoS::AtLeastOnce {
                            send_packet(sock, |o| {
                                PubAck::new(p.pkid).write(o).unwrap();
                            });
                        }
                        if p.topic.ends_with("/echo") && mode == Broker::Healthy {
                            let back = Publish::new(p.topic.clone(), QoS::AtMostOnce, p.payload.to_vec());
                            send_packet(sock, |o| {
                                back.write(o).unwrap();
                            });
                        }
                    }
                    Packet::PingReq => send_packet(sock, |o| {
                        PingResp.write(o).unwrap();
                    }),
                    _ => {}
                }
            }
        }
    }

    struct Running {
        seen: Arc<Mutex<Seen>>,
        inject: std::sync::mpsc::Sender<Publish>,
        ready: Arc<AtomicBool>,
        commands: Receiver<Message>,
        cfg: Config,
    }

    /// The real network thread against the fake broker.
    fn start(mode: Broker, state: Option<&str>) -> Running {
        let (port, seen, inject) = fake_broker(mode);
        let cfg = Config::parse(&format!("host=127.0.0.1\nport={port}\n"), "DESK").unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(Slot::default());
        if let Some(s) = state {
            slot.update(s.into(), false, |_| unreachable!());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = std::env::temp_dir().join(format!("jdrgb-ha-net-{}-{port}", std::process::id()));
        let log = Arc::new(Log::open(Some(dir), false));
        let (client, connection) = client(&cfg);
        {
            let (cfg, ready) = (cfg.clone(), ready.clone());
            std::thread::spawn(move || run(connection, client, cfg, ready, slot, tx, log));
        }
        Running { seen, inject, ready, commands: rx, cfg }
    }

    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_healthy_broker_sees_the_will_then_offline_echo_discovery_online_state_in_order() {
        let r = start(Broker::Healthy, Some(r#"{"state":"OFF"}"#));
        wait_for("ready", || r.ready.load(Ordering::SeqCst));
        // `ready` flips after the state request is accepted; let it reach the wire.
        wait_for("state", || r.seen.lock().unwrap().publishes.len() >= 5);

        let seen = r.seen.lock().unwrap();
        let c = seen.connect.as_ref().expect("a CONNECT");
        assert_eq!(c.client_id, "jdrgb-ha-desk");
        assert_eq!(c.keep_alive, 30);
        assert!(c.clean_session);
        assert!(c.login.is_none(), "anonymous: no secret on the PC");
        let will = c.last_will.as_ref().expect("a will");
        assert_eq!(will.topic, "jdrgb/jdrgb_desk/availability");
        assert_eq!(&will.message[..], b"offline");
        assert!(will.retain);

        assert_eq!(seen.filters, ["jdrgb/jdrgb_desk/set", "jdrgb/jdrgb_desk/echo", "homeassistant/status"]);
        let order: Vec<(&str, bool)> = seen.publishes.iter().map(|(t, _, r)| (t.as_str(), *r)).collect();
        assert_eq!(
            order,
            [
                ("jdrgb/jdrgb_desk/availability", true),
                ("jdrgb/jdrgb_desk/echo", false),
                ("homeassistant/light/jdrgb_desk/leds/config", true),
                ("jdrgb/jdrgb_desk/availability", true),
                ("jdrgb/jdrgb_desk/state", true),
            ]
        );
        assert_eq!(seen.publishes[0].1, "offline", "a retained offline first, every connection");
        assert_eq!(seen.publishes[3].1, "online");
        assert_eq!(seen.publishes[4].1, r#"{"state":"OFF"}"#, "the slot's state, not the raw file's");
    }

    #[test]
    fn a_command_reaches_the_worker_and_a_retained_one_does_not() {
        let r = start(Broker::Healthy, None);
        wait_for("ready", || r.ready.load(Ordering::SeqCst));
        let mut retained = Publish::new(r.cfg.topic("set"), QoS::AtMostOnce, br#"{"state":"OFF"}"#.to_vec());
        retained.retain = true;
        r.inject.send(retained).unwrap();
        let red = Publish::new(r.cfg.topic("set"), QoS::AtMostOnce, br#"{"state":"ON","effect":"Red"}"#.to_vec());
        r.inject.send(red).unwrap();
        let got = r.commands.recv_timeout(Duration::from_secs(5)).expect("a command");
        assert_eq!(got, Message { on: Some(true), look: Some(crate::model::Look::Effect("red")), brightness: None });
        assert!(r.commands.recv_timeout(Duration::from_millis(200)).is_err(), "the retained OFF got through");
    }

    /// The house's broker with a missing `read`: subscriptions granted, nothing
    /// delivered. The bridge must stay offline rather than go online and deaf.
    #[test]
    fn a_broker_that_grants_but_never_delivers_never_sees_online() {
        let r = start(Broker::GrantsButDeaf, None);
        wait_for("the echo to go out", || r.seen.lock().unwrap().publishes.iter().any(|(t, _, _)| t.ends_with("/echo")));
        std::thread::sleep(Duration::from_millis(500));
        assert!(!r.ready.load(Ordering::SeqCst));
        let seen = r.seen.lock().unwrap();
        assert!(!seen.publishes.iter().any(|(_, p, _)| p == "online"), "went online while deaf");
        assert!(!seen.publishes.iter().any(|(t, _, _)| t.contains("/config")), "published discovery while deaf");
    }

    /// uninstall.ps1's cleanup, end to end: its own client id, no will, three
    /// empty retained payloads that delete the entity, each acknowledged, then
    /// a clean exit — and none of the normal startup, which would republish it.
    #[test]
    fn unpublish_deletes_the_entity_and_publishes_nothing_else() {
        let (port, seen, _inject) = fake_broker(Broker::Healthy);
        let cfg = Config::parse(&format!("host=127.0.0.1\nport={port}\n"), "DESK").unwrap();
        let log = Log::open(None, false);
        crate::unpublish::run(&cfg, &log).expect("unpublish");

        let seen = seen.lock().unwrap();
        let c = seen.connect.as_ref().unwrap();
        assert_eq!(c.client_id, "jdrgb-ha-desk-cleanup", "must not kick a resident off the broker");
        assert!(c.last_will.is_none(), "a will could republish availability");
        let got: Vec<(&str, &str, bool)> = seen.publishes.iter().map(|(t, p, r)| (t.as_str(), p.as_str(), *r)).collect();
        assert_eq!(
            got,
            [
                ("homeassistant/light/jdrgb_desk/leds/config", "", true),
                ("jdrgb/jdrgb_desk/state", "", true),
                ("jdrgb/jdrgb_desk/availability", "", true),
            ]
        );
        assert!(seen.filters.is_empty(), "the cleanup subscribed to something");
    }

    #[test]
    fn unpublish_against_no_broker_fails_within_its_deadline() {
        // A port nothing listens on: the connection is refused at once.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cfg = Config::parse(&format!("host=127.0.0.1\nport={port}\n"), "DESK").unwrap();
        let started = std::time::Instant::now();
        assert!(crate::unpublish::run(&cfg, &Log::open(None, false)).is_err());
        assert!(started.elapsed() < crate::unpublish::DEADLINE + Duration::from_secs(1));
    }

    /// A connection dropped mid-lifecycle — after the SUBSCRIBE, before its
    /// SUBACK — must recover on the next one: offline first again, a fresh
    /// subscription, and online, with nothing from the dead connection in the way.
    #[test]
    fn a_reconnect_mid_subscribe_recovers_and_goes_online() {
        let r = start(Broker::DropsFirstSubscribe, None);
        wait_for("ready after the reconnect", || r.ready.load(Ordering::SeqCst));
        let seen = r.seen.lock().unwrap();
        assert_eq!(seen.connects, 2);
        let avail: Vec<&str> =
            seen.publishes.iter().filter(|(t, _, _)| t.ends_with("/availability")).map(|(_, p, _)| p.as_str()).collect();
        assert_eq!(avail, ["offline", "offline", "online"], "offline first on each connection, online only on the live one");
    }

    #[test]
    fn a_refused_subscription_never_reaches_the_echo_or_online() {
        let r = start(Broker::Refuses, None);
        wait_for("the subscription", || !r.seen.lock().unwrap().filters.is_empty());
        std::thread::sleep(Duration::from_millis(500));
        assert!(!r.ready.load(Ordering::SeqCst));
        let seen = r.seen.lock().unwrap();
        assert_eq!(seen.publishes.len(), 1, "only the initial offline: {:?}", seen.publishes);
        assert_eq!(seen.publishes[0].1, "offline");
    }
}
