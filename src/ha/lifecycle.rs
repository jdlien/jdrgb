//! One MQTT connection's lifecycle, with no I/O: what to send next, and what an
//! arriving packet means. See docs/home-assistant-plan.md, Phase 3, *Connecting*.
//!
//! A **stage machine**. `rumqttc`'s `try_*` calls return an error when the
//! request queue is full and don't retry, so each step here advances only when
//! the network thread reports its request accepted; a refused one keeps its
//! stage and is offered again on the next network event. Nothing can be skipped.
//! *Ready* — the only state in which the worker may publish — comes only after
//! every startup message has been accepted.

use crate::model::Ms;

/// How long the echo may take to come back.
pub const ECHO_TIMEOUT: Ms = 10_000;
/// How long before a refused subscription or a missing echo is tried again.
pub const RETRY: Ms = 5 * 60_000;

/// Something the network thread should hand to the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Availability: `online` or `offline`, retained.
    Availability(bool),
    /// Subscribe to the command topic, the echo topic and HA's birth topic.
    Subscribe,
    /// The access check: this nonce, not retained, to the echo topic.
    Echo(String),
    /// The retained discovery message.
    Discovery,
    /// Whatever the latest-state slot holds.
    State,
}

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    /// No connection.
    Down,
    /// Connected; working through the startup steps.
    Offline,
    Subscribe,
    AwaitSubAck { pkid: Option<u16> },
    Echo,
    AwaitEcho { sent: Ms },
    Discovery,
    Online,
    State,
    Ready,
    /// A refused subscription or a missing echo: availability stays `offline`
    /// and the step is tried again at `at`.
    Retry { step: Box<Stage>, at: Ms },
}

pub struct Lifecycle {
    stage: Stage,
    nonce: String,
    nonce_seed: String,
    nonces: u64,
    /// After HA's birth message: republish discovery, then state.
    birth: Vec<Request>,
}

/// What an arriving publish turned out to be.
#[derive(Debug, PartialEq)]
pub enum Arrived {
    /// A command for the worker.
    Command,
    /// A retained message on the command topic: ignored, because a command
    /// someone left retained would otherwise replay on every reconnect.
    RetainedCommand,
    Echo,
    Birth,
    Other,
}

impl Lifecycle {
    /// `seed` makes the echo nonce unique to this process: the time and pid.
    pub fn new(seed: String) -> Lifecycle {
        Lifecycle { stage: Stage::Down, nonce: String::new(), nonce_seed: seed, nonces: 0, birth: Vec::new() }
    }

    pub fn is_ready(&self) -> bool {
        self.stage == Stage::Ready
    }

    /// A new connection: everything from the last one is discarded.
    pub fn on_connack(&mut self) {
        self.stage = Stage::Offline;
        self.birth.clear();
    }

    /// The connection failed or dropped.
    pub fn on_down(&mut self) {
        self.stage = Stage::Down;
        self.birth.clear();
    }

    /// The request for the current step, if the step is one that sends.
    pub fn pending(&self) -> Option<Request> {
        if let Some(r) = self.birth.first() {
            return Some(r.clone());
        }
        match &self.stage {
            Stage::Offline => Some(Request::Availability(false)),
            Stage::Subscribe => Some(Request::Subscribe),
            Stage::Echo => Some(Request::Echo(self.nonce.clone())),
            Stage::Discovery => Some(Request::Discovery),
            Stage::Online => Some(Request::Availability(true)),
            Stage::State => Some(Request::State),
            _ => None,
        }
    }

    /// The client accepted `pending()`'s request.
    pub fn accepted(&mut self, now: Ms) {
        if !self.birth.is_empty() {
            self.birth.remove(0);
            return;
        }
        self.stage = match std::mem::replace(&mut self.stage, Stage::Down) {
            Stage::Offline => Stage::Subscribe,
            Stage::Subscribe => Stage::AwaitSubAck { pkid: None },
            Stage::Echo => Stage::AwaitEcho { sent: now },
            Stage::Discovery => Stage::Online,
            Stage::Online => Stage::State,
            Stage::State => Stage::Ready,
            other => other,
        };
    }

    /// `rumqttc` reports the SUBSCRIBE's packet id as it goes out.
    pub fn on_outgoing_subscribe(&mut self, pkid: u16) {
        if let Stage::AwaitSubAck { pkid: p @ None } = &mut self.stage {
            *p = Some(pkid);
        }
    }

    /// A SUBACK. `granted[i]` is whether filter `i` of our SUBSCRIBE — the
    /// command topic, the echo topic, the birth topic — was granted. `rumqttc`
    /// doesn't look at these; this does. Returns a line worth logging.
    ///
    /// This catches brokers that report a denial. The house's Mosquitto grants
    /// an ACL-denied subscription and enforces the ACL at delivery instead,
    /// which is what the echo is for.
    pub fn on_suback(&mut self, pkid: u16, granted: &[bool], now: Ms) -> Option<String> {
        let Stage::AwaitSubAck { pkid: Some(p) } = self.stage else { return None };
        if p != pkid {
            return None;
        }
        if granted.first() != Some(&true) || granted.get(1) != Some(&true) {
            self.stage = Stage::Retry { step: Box::new(Stage::Subscribe), at: now + RETRY };
            return Some(
                "the broker refused the subscription to the command topic — check its ACL; retrying in 5 minutes"
                    .into(),
            );
        }
        self.nonces += 1;
        self.nonce = format!("{}-{}", self.nonce_seed, self.nonces);
        self.stage = Stage::Echo;
        None
    }

    /// What an arriving publish is, given its topic kind and payload.
    pub fn on_publish(&mut self, kind: TopicKind, payload: &[u8], retain: bool) -> Arrived {
        match kind {
            TopicKind::Command if retain => Arrived::RetainedCommand,
            TopicKind::Command => Arrived::Command,
            TopicKind::Echo => {
                if matches!(self.stage, Stage::AwaitEcho { .. }) && payload == self.nonce.as_bytes() {
                    self.stage = Stage::Discovery;
                }
                Arrived::Echo
            }
            TopicKind::Birth => {
                if payload == b"online" && self.stage == Stage::Ready && self.birth.is_empty() {
                    self.birth = vec![Request::Discovery, Request::State];
                }
                Arrived::Birth
            }
            TopicKind::Other => Arrived::Other,
        }
    }

    /// Timers. Called on every network event — keepalive pings guarantee one at
    /// least every 30 s — so expiry is handled on the first event after it,
    /// normally well within the keepalive.
    pub fn tick(&mut self, now: Ms) -> Option<String> {
        match &self.stage {
            Stage::AwaitEcho { sent } if now.saturating_sub(*sent) >= ECHO_TIMEOUT => {
                self.stage = Stage::Retry { step: Box::new(Stage::Echo), at: now + RETRY };
                Some(
                    "the echo didn't come back: the broker's ACL appears to deny this bridge's topics — \
                     check its readwrite line; retrying in 5 minutes"
                        .into(),
                )
            }
            Stage::Retry { at, .. } if now >= *at => {
                let Stage::Retry { step, .. } = std::mem::replace(&mut self.stage, Stage::Down) else { unreachable!() };
                self.stage = *step;
                if self.stage == Stage::Echo {
                    self.nonces += 1;
                    self.nonce = format!("{}-{}", self.nonce_seed, self.nonces);
                }
                None
            }
            _ => None,
        }
    }
}

/// Which of the bridge's subscriptions a topic belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TopicKind {
    Command,
    Echo,
    Birth,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accept everything offered, as an empty queue would, until nothing is.
    fn drain(lc: &mut Lifecycle, now: Ms) -> Vec<Request> {
        let mut sent = Vec::new();
        while let Some(r) = lc.pending() {
            sent.push(r);
            lc.accepted(now);
        }
        sent
    }

    fn connect(lc: &mut Lifecycle) -> String {
        lc.on_connack();
        assert_eq!(drain(lc, 0), vec![Request::Availability(false), Request::Subscribe]);
        lc.on_outgoing_subscribe(7);
        assert_eq!(lc.on_suback(7, &[true, true, true], 0), None);
        let Some(Request::Echo(nonce)) = lc.pending() else { panic!("expected the echo") };
        nonce
    }

    #[test]
    fn a_healthy_connection_goes_offline_subscribe_echo_discovery_online_state() {
        let mut lc = Lifecycle::new("t".into());
        let nonce = connect(&mut lc);
        assert_eq!(drain(&mut lc, 0), vec![Request::Echo(nonce.clone())]);
        assert!(!lc.is_ready(), "not ready before the echo returns");
        assert_eq!(lc.on_publish(TopicKind::Echo, nonce.as_bytes(), false), Arrived::Echo);
        assert_eq!(drain(&mut lc, 1), vec![Request::Discovery, Request::Availability(true), Request::State]);
        assert!(lc.is_ready());
    }

    /// With a one-entry queue that refuses as often as it accepts, every step
    /// still goes out, in order, exactly once.
    #[test]
    fn a_refused_request_keeps_its_stage_and_nothing_is_skipped() {
        let mut lc = Lifecycle::new("t".into());
        lc.on_connack();
        let mut sent = Vec::new();
        let mut refuse = true;
        for _ in 0..20 {
            let Some(r) = lc.pending() else { break };
            if refuse {
                refuse = false; // the queue was full this time
                continue;
            }
            refuse = true;
            sent.push(r);
            lc.accepted(0);
        }
        assert_eq!(sent, vec![Request::Availability(false), Request::Subscribe]);
        assert!(!lc.is_ready());
    }

    #[test]
    fn a_refused_subscription_waits_and_retries_and_never_goes_online() {
        let mut lc = Lifecycle::new("t".into());
        lc.on_connack();
        drain(&mut lc, 0);
        lc.on_outgoing_subscribe(3);
        assert!(lc.on_suback(3, &[false, true, true], 100).is_some());
        assert_eq!(lc.pending(), None);
        assert_eq!(lc.tick(100 + RETRY - 1), None);
        assert_eq!(lc.pending(), None, "not before the retry");
        lc.tick(100 + RETRY);
        assert_eq!(lc.pending(), Some(Request::Subscribe));
    }

    #[test]
    fn a_suback_for_some_other_packet_is_not_ours() {
        let mut lc = Lifecycle::new("t".into());
        lc.on_connack();
        drain(&mut lc, 0);
        lc.on_outgoing_subscribe(3);
        assert_eq!(lc.on_suback(4, &[true, true, true], 0), None);
        assert_eq!(lc.pending(), None, "still waiting for packet 3");
    }

    /// The house's broker grants a denied subscription and drops the messages:
    /// the echo never comes back, and the bridge must stay not-ready.
    #[test]
    fn a_silent_echo_times_out_and_retries_with_a_fresh_nonce() {
        let mut lc = Lifecycle::new("t".into());
        let first = connect(&mut lc);
        drain(&mut lc, 1_000);
        assert_eq!(lc.tick(1_000 + ECHO_TIMEOUT - 1), None);
        assert!(lc.tick(1_000 + ECHO_TIMEOUT).is_some(), "logged");
        assert!(!lc.is_ready());
        assert_eq!(lc.pending(), None);
        lc.tick(1_000 + ECHO_TIMEOUT + RETRY);
        let Some(Request::Echo(second)) = lc.pending() else { panic!("expected a new echo") };
        assert_ne!(first, second);
    }

    #[test]
    fn someone_elses_echo_or_a_stale_one_doesnt_count() {
        let mut lc = Lifecycle::new("t".into());
        let nonce = connect(&mut lc);
        drain(&mut lc, 0);
        lc.on_publish(TopicKind::Echo, b"t-999", false);
        assert!(!lc.is_ready());
        lc.on_publish(TopicKind::Echo, nonce.as_bytes(), false);
        assert_eq!(lc.pending(), Some(Request::Discovery));
    }

    #[test]
    fn has_birth_republishes_discovery_and_state_once_ready() {
        let mut lc = Lifecycle::new("t".into());
        let nonce = connect(&mut lc);
        drain(&mut lc, 0);
        lc.on_publish(TopicKind::Birth, b"online", false);
        assert_eq!(lc.pending(), None, "not ready yet: the birth is not acted on");
        lc.on_publish(TopicKind::Echo, nonce.as_bytes(), false);
        drain(&mut lc, 0);
        lc.on_publish(TopicKind::Birth, b"online", false);
        assert_eq!(drain(&mut lc, 0), vec![Request::Discovery, Request::State]);
        assert!(lc.is_ready(), "republishing doesn't make it unready");
        lc.on_publish(TopicKind::Birth, b"offline", false);
        assert_eq!(lc.pending(), None, "HA going offline is not a birth");
    }

    #[test]
    fn a_retained_command_is_never_a_command() {
        let mut lc = Lifecycle::new("t".into());
        assert_eq!(lc.on_publish(TopicKind::Command, b"{}", true), Arrived::RetainedCommand);
        assert_eq!(lc.on_publish(TopicKind::Command, b"{}", false), Arrived::Command);
    }

    #[test]
    fn a_drop_discards_everything_and_a_reconnect_starts_over() {
        let mut lc = Lifecycle::new("t".into());
        let nonce = connect(&mut lc);
        drain(&mut lc, 0);
        lc.on_publish(TopicKind::Echo, nonce.as_bytes(), false);
        drain(&mut lc, 0);
        assert!(lc.is_ready());
        lc.on_down();
        assert!(!lc.is_ready());
        assert_eq!(lc.pending(), None);
        lc.on_connack();
        assert_eq!(lc.pending(), Some(Request::Availability(false)), "offline first, every connection");
    }
}
