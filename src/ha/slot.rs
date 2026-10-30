//! The latest-state slot: the one place the light's state message lives, and
//! the one lock that orders its two publishers.
//!
//! The worker writes a new projection here and publishes it; the network thread
//! publishes whatever is here on readiness and on HA's birth. Each holds the
//! lock across choosing the payload **and** calling `try_publish`. Otherwise the
//! network thread could read an old `OFF`, the worker write and publish a newer
//! `ON`, and the network thread then enqueue its stale `OFF` last — leaving the
//! wrong state retained on the broker until something else changed. `try_publish`
//! never blocks, so the critical section stays short.

use std::sync::Mutex;

#[derive(Default)]
pub struct Slot {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    payload: Option<String>,
    /// Written but not yet accepted by the client, so the next tick tries again.
    unsent: bool,
}

impl Slot {
    /// The worker's side: store `payload` if it differs, and publish it while
    /// `ready`. `publish` returns whether the client accepted it.
    pub fn update(&self, payload: String, ready: bool, publish: impl FnOnce(&str) -> bool) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.payload.as_deref() != Some(payload.as_str()) {
            g.payload = Some(payload);
            g.unsent = true;
        }
        if g.unsent && ready {
            let accepted = publish(g.payload.as_deref().expect("set above"));
            g.unsent = !accepted;
        }
    }

    /// The network thread's side: publish what is here now. Returns whether the
    /// client accepted it — or there was nothing to publish yet, which also lets
    /// the lifecycle move on; the worker publishes its first projection itself.
    pub fn publish_current(&self, publish: impl FnOnce(&str) -> bool) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(payload) = g.payload.as_deref() else { return true };
        let accepted = publish(payload);
        if accepted {
            g.unsent = false;
        }
        accepted
    }

    #[cfg(test)]
    fn current(&self) -> Option<String> {
        self.inner.lock().unwrap().payload.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn an_unchanged_projection_isnt_published_twice() {
        let slot = Slot::default();
        let mut sent = Vec::new();
        slot.update("ON".into(), true, |p| {
            sent.push(p.to_string());
            true
        });
        slot.update("ON".into(), true, |p| {
            sent.push(p.to_string());
            true
        });
        assert_eq!(sent, ["ON"]);
    }

    #[test]
    fn a_refused_publish_is_retried_and_one_while_not_ready_waits() {
        let slot = Slot::default();
        slot.update("A".into(), true, |_| false);
        let mut sent = Vec::new();
        slot.update("A".into(), true, |p| {
            sent.push(p.to_string());
            true
        });
        assert_eq!(sent, ["A"], "the refused one went out on the next update");

        slot.update("B".into(), false, |_| panic!("published while not ready"));
        assert_eq!(slot.current().as_deref(), Some("B"));
        assert!(slot.publish_current(|p| {
            sent.push(p.to_string());
            true
        }));
        assert_eq!(sent, ["A", "B"], "readiness publishes what the worker left");
    }

    /// The interleaving the lock exists for, driven hard: the worker publishes
    /// rising numbers while the network thread republishes the current one. In
    /// the order things reached the client, a number must never be followed by
    /// a smaller one.
    #[test]
    fn an_older_projection_is_never_enqueued_after_a_newer_one() {
        let slot = Arc::new(Slot::default());
        let wire = Arc::new(Mutex::new(Vec::<u32>::new()));
        let network = {
            let (slot, wire) = (slot.clone(), wire.clone());
            std::thread::spawn(move || {
                for _ in 0..5_000 {
                    slot.publish_current(|p| {
                        wire.lock().unwrap().push(p.parse().unwrap());
                        true
                    });
                }
            })
        };
        for n in 0..5_000u32 {
            slot.update(n.to_string(), true, |p| {
                wire.lock().unwrap().push(p.parse().unwrap());
                true
            });
        }
        network.join().unwrap();
        let wire = wire.lock().unwrap();
        assert!(wire.windows(2).all(|w| w[0] <= w[1]), "an older state went out after a newer one");
        assert_eq!(*wire.last().unwrap(), 4_999);
    }
}
