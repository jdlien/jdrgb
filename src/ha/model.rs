//! What the bridge decides, with no I/O: which `jdrgb.exe` command to run, and
//! when. See docs/home-assistant-plan.md, Phase 3 and 4 — this module is the
//! "Resolving a command", "The card follows the strip" and "Pacing" sections.
//!
//! Everything here takes the time as an argument, in milliseconds on a monotonic
//! clock, and every interval comes from `Timing`. So the tests drive it with a
//! fake clock and intervals of a few milliseconds, and the worker drives it with
//! the real ones.

use jdrgb::palette::{Last, Lit, MIN_LEVEL, Paint, State, hsv_of, recognise, scale, snap_hs, snap_kelvin};

/// Milliseconds on a monotonic clock.
pub type Ms = u64;

/// Every interval the bridge waits on. `Timing::default()` is the plan's.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// After the latest command, how long to wait for more before acting, so a
    /// burst from HA — brightness and color as two messages — becomes one leg.
    pub settle: Ms,
    /// How long the light must be quiet before the card follows the strip.
    pub quiet: Ms,
    /// The least time between the bridge's own GPU legs — and, as a courtesy,
    /// since anyone's GPU write. Writes *seconds* apart have glitched the
    /// display; writes minutes apart have not (docs/ups-wedge-incident.md).
    pub gpu_interval: Ms,
    /// A strip leg identical to the previous one is skipped within this window.
    pub repeat_window: Ms,
    /// The runaway guard refills one token per this long.
    pub refill: Ms,
    /// With the guard empty, one `off` still gets through per this long.
    pub off_allowance: Ms,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            settle: 250,
            quiet: 5_000,
            gpu_interval: 60_000,
            repeat_window: 10_000,
            refill: 60_000,
            off_allowance: 5 * 60_000,
        }
    }
}

/// The runaway guard's capacity, in strip legs.
pub const BUCKET_CAPACITY: u32 = 30;

// ---------------------------------------------------------------------------
// Commands in
// ---------------------------------------------------------------------------

/// How a command describes what to show. A preset by name, or a point on the
/// color wheel or the white-temperature slider, which is snapped to a preset.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    Effect(&'static str),
    /// Hue in degrees, saturation 0–100, as HA sends them.
    Hs(f64, f64),
    Kelvin(u32),
}

/// One message from HA, keeping which fields were present — its JSON light
/// sends only what changed, so "absent" and "unchanged" are the same thing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    /// `Some(true)` for `"state": "ON"`, `Some(false)` for `"OFF"`.
    pub on: Option<bool>,
    pub look: Option<Look>,
    /// HA's 1–255.
    pub brightness: Option<u8>,
}

/// What the commands since the last leg add up to.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Off,
    /// No look and no brightness is a *plain* `ON`, which means `jdrgb on`.
    On { look: Option<Look>, brightness: Option<u8> },
}

impl Intent {
    /// Fold one more message into what's pending.
    ///
    /// `OFF` replaces everything. `ON` after a pending `OFF` replaces it too.
    /// Otherwise present fields overwrite the same fields, and a look field
    /// replaces any other look field, so the most recent wins. Merging never
    /// turns a plain `ON` into "reapply the cached preset": a plain `ON` merged
    /// into a pending look leaves the look, and a plain `ON` on its own stays
    /// plain.
    pub fn merge(pending: Option<Intent>, msg: &Message) -> Option<Intent> {
        if msg.on == Some(false) {
            return Some(Intent::Off);
        }
        if msg.on.is_none() && msg.look.is_none() && msg.brightness.is_none() {
            return pending;
        }
        Some(match pending {
            Some(Intent::On { look, brightness }) => Intent::On {
                look: msg.look.clone().or(look),
                brightness: msg.brightness.or(brightness),
            },
            None | Some(Intent::Off) => Intent::On { look: msg.look.clone(), brightness: msg.brightness },
        })
    }
}

// ---------------------------------------------------------------------------
// Commands out
// ---------------------------------------------------------------------------

/// One `jdrgb.exe` command, before it is pointed at a device.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Off,
    On,
    Preset { name: &'static str, level: u8 },
}

impl Cmd {
    /// The arguments for the strip, or with `--gpu` for the card. A preset goes
    /// by *name*, so each device gets its own calibration — never as hex, which
    /// is literal and would skip the card's (the plan's trap 1).
    pub fn args(&self, gpu: bool) -> Vec<String> {
        let mut a: Vec<String> = match self {
            Cmd::Off => vec!["off".into()],
            Cmd::On => vec!["on".into()],
            Cmd::Preset { name, level } => {
                let mut a = vec![name.to_string()];
                if *level < 100 {
                    a.push("--level".into());
                    a.push(level.to_string());
                }
                a
            }
        };
        if gpu {
            a.push("--gpu".into());
        }
        a
    }
}

// ---------------------------------------------------------------------------
// Brightness <-> level
// ---------------------------------------------------------------------------

/// `round(n / d)` in integers.
fn div_round(n: u32, d: u32) -> u32 {
    (2 * n + d) / (2 * d)
}

/// HA's brightness, 1–255, onto `MIN_LEVEL..=100`.
pub fn level_for(brightness: u8) -> u8 {
    let b = u32::from(brightness.max(1));
    let span = u32::from(100 - MIN_LEVEL);
    (u32::from(MIN_LEVEL) + div_round((b - 1) * span, 254)) as u8
}

/// The reverse, for reporting. A level below `MIN_LEVEL` — set by hand, since
/// nothing the bridge sends goes that low — reports as the bottom of the slider.
pub fn brightness_for(level: u8) -> u8 {
    if level < MIN_LEVEL {
        return 1;
    }
    let span = u32::from(100 - MIN_LEVEL);
    (1 + div_round(u32::from(level.min(100) - MIN_LEVEL) * 254, span)) as u8
}

// ---------------------------------------------------------------------------
// What a device shows, in the terms a command resolves against
// ---------------------------------------------------------------------------

/// The look a device is showing — or, when it's off, what `on` would bring
/// back — as far as a command needs to know.
#[derive(Clone, Debug, PartialEq)]
enum Base {
    /// A preset at a level, recognised from the recorded value.
    Preset(&'static str, u8),
    /// A solid that isn't any preset at any level: a hand-tuned hex.
    Hex((u8, u8, u8)),
    /// A per-LED frame, which has no single preset to scale.
    Frame,
    /// Nothing recorded, or off with nothing kept.
    Nothing,
}

fn base_of(st: &State, gpu: bool) -> (bool, Base) {
    let solid = |rgb: (u8, u8, u8)| match recognise(rgb, gpu) {
        Some((name, level)) => Base::Preset(name, level),
        None => Base::Hex(rgb),
    };
    match st.slot(gpu) {
        Some(Last::Color(rgb)) => (true, solid(rgb)),
        Some(Last::Multi) => (true, Base::Frame),
        Some(Last::Off) | None => (
            false,
            match st.lit(gpu) {
                Some(Lit::Color(rgb)) => solid(*rgb),
                Some(Lit::Frame(_)) => Base::Frame,
                None => Base::Nothing,
            },
        ),
    }
}

fn lit(slot: Option<Last>) -> bool {
    matches!(slot, Some(Last::Color(_)) | Some(Last::Multi))
}

/// Which device a command resolves against, by the same rule as what is
/// reported: while a follow is pending, the strip — the card is converging on
/// it, so its record is stale by design. Otherwise the strip, unless it is
/// dark and the card is lit.
pub fn base_is_gpu(st: &State, follow_pending: bool) -> bool {
    !follow_pending && !lit(st.mb) && lit(st.gpu)
}

/// Turn an intent into one command, or `None` when there is nothing sensible to
/// do — a brightness for a lit per-LED frame, which has no preset to scale.
pub fn resolve(intent: &Intent, st: &State, follow_pending: bool) -> Option<Cmd> {
    let (look, brightness) = match intent {
        Intent::Off => return Some(Cmd::Off),
        Intent::On { look: None, brightness: None } => return Some(Cmd::On),
        Intent::On { look, brightness } => (look, brightness),
    };
    let (is_lit, base) = base_of(st, base_is_gpu(st, follow_pending));
    let name = match look {
        Some(Look::Effect(name)) => *name,
        Some(Look::Hs(h, s)) => snap_hs(*h, *s),
        Some(Look::Kelvin(k)) => snap_kelvin(*k),
        // A brightness alone scales whatever the base shows.
        None => match &base {
            Base::Preset(name, _) => name,
            // A lit hex snaps to the nearest preset first. Scaling it directly
            // would compound rounding and never recognise again, so the slider
            // would jump back to 100% every time.
            Base::Hex(rgb) if is_lit => {
                let (h, s) = hsv_of(*rgb);
                snap_hs(h, s * 100.0)
            }
            // A frame has no preset to scale: lit, the brightness is ignored.
            Base::Frame if is_lit => return None,
            // Off with a lit record nothing recognises — a hex or a frame —
            // or nothing kept: `on` brings back what was there, as it was, and
            // the brightness is ignored. Snapping a saved hex here would throw
            // away the color `off` kept.
            Base::Hex(_) | Base::Frame | Base::Nothing => return Some(Cmd::On),
        },
    };
    let level = match (brightness, &base) {
        (Some(b), _) => level_for(*b),
        (None, Base::Preset(_, level)) => *level,
        (None, _) => 100,
    };
    Some(Cmd::Preset { name, level })
}

// ---------------------------------------------------------------------------
// What gets reported
// ---------------------------------------------------------------------------

/// What one device's record looks like from outside.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Preset { name: &'static str, level: u8 },
    /// No preset at any level: the wire value, reported as an approximation.
    Approx((u8, u8, u8)),
}

/// The one entity's state, projected from two devices.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// `None` is unknown — nothing recorded for either device.
    pub on: Option<bool>,
    pub look: Option<Shown>,
}

fn shown(st: &State, gpu: bool) -> Option<Shown> {
    match st.slot(gpu)? {
        Last::Color(rgb) => Some(match recognise(rgb, gpu) {
            Some((name, level)) => Shown::Preset { name, level },
            None => Shown::Approx(rgb),
        }),
        // A frame reports its first LED, the way a hex reports its value.
        Last::Multi => Some(Shown::Approx(st.pattern(gpu).and_then(|p| p.first().copied()).unwrap_or((0, 0, 0)))),
        Last::Off => None,
    }
}

/// The state table of the plan's *State out*. While a follow is pending the
/// strip alone is reported, on/off as well as look — otherwise, after an `OFF`,
/// the card still lit would make the light read `ON`, and a toggle would send
/// another `OFF`.
pub fn project(st: &State, follow_pending: bool) -> Projection {
    if follow_pending {
        return match st.mb {
            None => Projection { on: None, look: None },
            Some(Last::Off) => Projection { on: Some(false), look: None },
            Some(_) => Projection { on: Some(true), look: shown(st, false) },
        };
    }
    match (st.mb, st.gpu) {
        (None, None) => Projection { on: None, look: None },
        (mb, _) if lit(mb) => Projection { on: Some(true), look: shown(st, false) },
        (_, gpu) if lit(gpu) => Projection { on: Some(true), look: shown(st, true) },
        _ => Projection { on: Some(false), look: None },
    }
}

// ---------------------------------------------------------------------------
// The decisions over time
// ---------------------------------------------------------------------------

/// What the worker sees each time it looks: the state file, and the stash.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observed {
    pub state: State,
    /// The stash's identity (`palette::stash_identity`), or `None` if absent.
    pub stash: Option<String>,
    /// How long ago anyone last wrote the card, from the CLI's machine-wide
    /// marker. `None` when the marker is missing or unreadable — ignored.
    pub gpu_marker_age: Option<Ms>,
}

/// The card's half of the latest command, waiting for the light to settle.
#[derive(Clone, Debug, PartialEq)]
pub struct Follow {
    pub cmd: Cmd,
    /// What the state file's `rev` and the stash were once the strip leg was
    /// done. Any difference later means someone else acted since.
    pub rev: u64,
    pub stash: Option<String>,
}

impl Follow {
    fn still_valid(&self, obs: &Observed) -> bool {
        obs.state.rev == self.rev && obs.stash == self.stash
    }
}

/// A leg to run.
#[derive(Clone, Debug, PartialEq)]
pub enum Leg {
    Strip(Cmd),
    Gpu(Cmd),
}

impl Leg {
    pub fn args(&self) -> Vec<String> {
        match self {
            Leg::Strip(c) => c.args(false),
            Leg::Gpu(c) => c.args(true),
        }
    }
}

/// One step of the worker: run a leg, or note something and look again.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Run(Leg),
    Note(String),
    Idle,
}

/// The bridge's last strip leg that *succeeded*. A failed one leaves none: it
/// may have changed the lights live without recording anything, so nothing
/// about the record can be trusted to say what is showing.
struct StripDone {
    at: Ms,
    rev_after: u64,
    /// What that leg's command put on the record.
    wrote: Last,
}

/// What the strip's record would say after `cmd` landed, for the repeat filter.
/// `None` for `on`, which depends on the lit record and is decided elsewhere.
fn expected_strip(cmd: &Cmd) -> Option<Last> {
    match cmd {
        Cmd::Off => Some(Last::Off),
        Cmd::On => None,
        Cmd::Preset { name, level } => Some(Last::Color(scale(Paint::Preset(name).rgb(false), *level))),
    }
}

/// Token bucket for strip legs, counted on attempt.
struct Bucket {
    tokens: u32,
    refilled_at: Ms,
}

impl Bucket {
    fn take(&mut self, now: Ms, refill: Ms) -> bool {
        if refill > 0 {
            let earned = (now.saturating_sub(self.refilled_at) / refill) as u32;
            if earned > 0 {
                self.tokens = (self.tokens + earned).min(BUCKET_CAPACITY);
                self.refilled_at += u64::from(earned) * refill;
            }
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

pub struct Model {
    timing: Timing,
    pending: Option<Intent>,
    last_message: Option<Ms>,
    /// The later of the last message and the last strip leg: "quiet" is
    /// measured from here.
    last_activity: Option<Ms>,
    follow: Option<Follow>,
    last_strip: Option<StripDone>,
    last_gpu: Option<Ms>,
    bucket: Bucket,
    last_off_allowance: Option<Ms>,
    running: Option<Leg>,
}

impl Model {
    pub fn new(timing: Timing, now: Ms) -> Model {
        Model {
            timing,
            pending: None,
            last_message: None,
            last_activity: None,
            follow: None,
            last_strip: None,
            last_gpu: None,
            bucket: Bucket { tokens: BUCKET_CAPACITY, refilled_at: now },
            last_off_allowance: None,
            running: None,
        }
    }

    #[cfg(test)]
    pub fn follow(&self) -> Option<&Follow> {
        self.follow.as_ref()
    }

    pub fn follow_pending(&self) -> bool {
        self.follow.is_some()
    }

    /// A command arrived. It joins whatever is pending; a child already running
    /// is not cancelled, and this becomes the next leg.
    pub fn on_message(&mut self, msg: &Message, now: Ms) {
        self.pending = Intent::merge(self.pending.take(), msg);
        self.last_message = Some(now);
        self.last_activity = Some(now);
    }

    /// Check the pending follow against what is on disk now. Called every
    /// tick, not just before a spawn, so a follow someone else overtook stops
    /// being reported the moment it is overtaken.
    pub fn observe(&mut self, obs: &Observed) -> Option<String> {
        let f = self.follow.as_ref()?;
        if f.still_valid(obs) {
            return None;
        }
        let what = format!("{:?}", f.cmd);
        self.follow = None;
        Some(format!("dropped the card's follow ({what}): something else changed the lights or the stash"))
    }

    fn set_follow(&mut self, cmd: Cmd, obs: &Observed) {
        self.follow = Some(Follow { cmd, rev: obs.state.rev, stash: obs.stash.clone() });
    }

    /// The next thing to do, given what is on disk now.
    pub fn next(&mut self, obs: &Observed, now: Ms) -> Step {
        if self.running.is_some() {
            return Step::Idle;
        }
        if let Some(note) = self.observe(obs) {
            return Step::Note(note);
        }

        let settled = self.last_message.is_some_and(|t| now.saturating_sub(t) >= self.timing.settle);
        if self.pending.is_some() && settled {
            let intent = self.pending.take().expect("checked above");
            return self.strip_step(&intent, obs, now);
        }

        if self.follow.is_some() && self.pending.is_none() {
            return self.gpu_step(obs, now);
        }
        Step::Idle
    }

    fn strip_step(&mut self, intent: &Intent, obs: &Observed, now: Ms) -> Step {
        let Some(cmd) = resolve(intent, &obs.state, self.follow.is_some()) else {
            return Step::Note("ignored a brightness for a per-LED pattern, which has no preset to scale".into());
        };

        // A plain `ON` for a strip that isn't off: `jdrgb on` would change
        // nothing and record nothing. Replacing a still-valid follow with
        // `on --gpu` would strand the card on its old color, since `on` acts
        // only on a device recorded off — so a valid follow is kept.
        if cmd == Cmd::On && obs.state.mb != Some(Last::Off) {
            if self.follow.as_ref().is_some_and(|f| f.still_valid(obs)) {
                return Step::Note("plain ON with the strip already lit; the card's follow stands".into());
            }
            self.set_follow(Cmd::On, obs);
            return Step::Note("plain ON with the strip already lit; the card follows with `on`".into());
        }

        let args = cmd.args(false);
        // A repeat is a command whose result is already on the record — the
        // same wire value, so `black` at any level is a repeat of `black` — with
        // nothing written since the bridge's own last strip leg, inside the
        // window, and no stash. Comparing against the *record* rather than
        // the last command is what lets a retry after a failed apply through:
        // that apply recorded nothing, so the record still shows the old color.
        //
        // And it must repeat the bridge's previous *successful* leg. Matching
        // the record alone isn't enough: a Red that failed part-way can have
        // changed the lights live without recording, so the record still says
        // Blue — and a Blue sent to undo it would look like a repeat.
        let repeat = obs.stash.is_none()
            && expected_strip(&cmd).is_some_and(|want| {
                obs.state.mb == Some(want)
                    && self.last_strip.as_ref().is_some_and(|l| {
                        l.wrote == want
                            && now.saturating_sub(l.at) < self.timing.repeat_window
                            && obs.state.rev == l.rev_after
                    })
            });
        if repeat {
            self.set_follow(cmd, obs);
            return Step::Note(format!("skipped a repeat of `jdrgb {}`", args.join(" ")));
        }

        if !self.bucket.take(now, self.timing.refill) {
            let off_allowed = cmd == Cmd::Off
                && self.last_off_allowance.is_none_or(|t| now.saturating_sub(t) >= self.timing.off_allowance);
            if off_allowed {
                self.last_off_allowance = Some(now);
            } else {
                self.set_follow(cmd, obs);
                return Step::Note(format!(
                    "runaway guard: dropped `jdrgb {}` — more than {BUCKET_CAPACITY} strip changes in too short a time",
                    args.join(" ")
                ));
            }
        }

        let leg = Leg::Strip(cmd);
        self.running = Some(leg.clone());
        Step::Run(leg)
    }

    fn gpu_step(&mut self, obs: &Observed, now: Ms) -> Step {
        let t = self.timing;
        let quiet = self.last_activity.is_none_or(|a| now.saturating_sub(a) >= t.quiet);
        let own_ok = self.last_gpu.is_none_or(|g| now.saturating_sub(g) >= t.gpu_interval);
        let marker_ok = obs.gpu_marker_age.is_none_or(|age| age >= t.gpu_interval);
        if !(quiet && own_ok && marker_ok) {
            return Step::Idle;
        }
        let f = self.follow.take().expect("gpu_step needs a follow");
        // `on` acts only on a device recorded off; for a lit card it would
        // open nothing and do nothing, so there is no point spawning it.
        if f.cmd == Cmd::On && obs.state.gpu != Some(Last::Off) {
            return Step::Note("the card isn't off; its `on` follow has nothing to do".into());
        }
        let leg = Leg::Gpu(f.cmd);
        self.running = Some(leg.clone());
        Step::Run(leg)
    }

    /// A leg's child exited, `ok` or not; each is attempted once. `obs` is what
    /// was on disk afterwards.
    pub fn on_done(&mut self, obs: &Observed, now: Ms, ok: bool) {
        match self.running.take() {
            Some(Leg::Strip(cmd)) => {
                self.last_strip = match expected_strip(&cmd) {
                    Some(wrote) if ok => Some(StripDone { at: now, rev_after: obs.state.rev, wrote }),
                    _ => None,
                };
                self.last_activity = Some(now);
                // Observed *after* the strip leg, so its own write — and a
                // stash it cleared — don't count as someone else acting.
                self.set_follow(cmd, obs);
            }
            Some(Leg::Gpu(_)) => self.last_gpu = Some(now),
            None => {}
        }
    }

    /// How long the worker may sleep before something here could change.
    pub fn wake_in(&self, now: Ms, tick: Ms) -> Ms {
        let mut wait = tick;
        if self.pending.is_some() {
            if let Some(t) = self.last_message {
                wait = wait.min((t + self.timing.settle).saturating_sub(now));
            }
        }
        wait.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jdrgb::palette::{Paint, scale};

    fn fast() -> Timing {
        Timing { settle: 10, quiet: 50, gpu_interval: 200, repeat_window: 100, refill: 1_000, off_allowance: 5_000 }
    }

    fn on(look: Option<Look>, brightness: Option<u8>) -> Message {
        Message { on: Some(true), look, brightness }
    }
    fn off() -> Message {
        Message { on: Some(false), ..Default::default() }
    }
    fn effect(name: &'static str) -> Message {
        on(Some(Look::Effect(name)), None)
    }

    fn color(name: &str, gpu: bool, level: u8) -> Last {
        Last::Color(scale(Paint::Preset(jdrgb::palette::PRESETS.iter().find(|(n, _)| *n == name).unwrap().0).rgb(gpu), level))
    }

    /// A stand-in for jdrgb.exe recording what it did — enough to drive the
    /// model through whole sequences.
    fn apply(st: &mut State, leg: &Leg) {
        let (gpu, cmd) = match leg {
            Leg::Strip(c) => (false, c),
            Leg::Gpu(c) => (true, c),
        };
        match cmd {
            Cmd::Off => st.record(gpu, Last::Off),
            Cmd::On => {
                if st.slot(gpu) == Some(Last::Off) {
                    let back = match st.lit(gpu) {
                        Some(Lit::Color(rgb)) => Last::Color(*rgb),
                        _ => color("coolwhite", gpu, 100),
                    };
                    st.record(gpu, back);
                }
            }
            Cmd::Preset { name, level } => st.record(gpu, color(name, gpu, *level)),
        }
        st.rev += 1;
    }

    /// Drive the model to quiescence at `now`, applying every leg it runs.
    fn run(m: &mut Model, obs: &mut Observed, now: Ms) -> Vec<Leg> {
        let mut legs = Vec::new();
        for _ in 0..20 {
            match m.next(obs, now) {
                Step::Run(leg) => {
                    apply(&mut obs.state, &leg);
                    m.on_done(obs, now, true);
                    legs.push(leg);
                }
                Step::Note(_) => {}
                Step::Idle => break,
            }
        }
        legs
    }

    fn both(name: &str, level: u8) -> Observed {
        let mut st = State::default();
        st.record(false, color(name, false, level));
        st.record(true, color(name, true, level));
        st.rev = 1;
        Observed { state: st, ..Default::default() }
    }

    // ---- merging ---------------------------------------------------------

    #[test]
    fn off_replaces_and_on_after_off_replaces_it() {
        let p = Intent::merge(None, &effect("red"));
        let p = Intent::merge(p, &off());
        assert_eq!(p, Some(Intent::Off));
        let p = Intent::merge(p, &on(None, Some(100)));
        assert_eq!(p, Some(Intent::On { look: None, brightness: Some(100) }));
    }

    #[test]
    fn present_fields_overwrite_and_the_latest_look_wins() {
        let p = Intent::merge(None, &on(None, Some(50)));
        let p = Intent::merge(p, &effect("red"));
        assert_eq!(p, Some(Intent::On { look: Some(Look::Effect("red")), brightness: Some(50) }));
        let p = Intent::merge(p, &on(Some(Look::Kelvin(2700)), None));
        assert_eq!(p, Some(Intent::On { look: Some(Look::Kelvin(2700)), brightness: Some(50) }));
    }

    /// A plain ON never becomes "reapply the cached preset", and merging one
    /// into a pending look leaves the look.
    #[test]
    fn a_plain_on_stays_plain_and_leaves_a_pending_look() {
        assert_eq!(Intent::merge(None, &on(None, None)), Some(Intent::On { look: None, brightness: None }));
        let p = Intent::merge(None, &effect("red"));
        assert_eq!(Intent::merge(p.clone(), &on(None, None)), p);
    }

    // ---- resolving -------------------------------------------------------

    #[test]
    fn effects_wheel_and_kelvin_resolve_to_named_presets() {
        let st = both("blue", 100).state;
        let r = |look| resolve(&Intent::On { look: Some(look), brightness: None }, &st, false);
        assert_eq!(r(Look::Effect("warmwhite")), Some(Cmd::Preset { name: "warmwhite", level: 100 }));
        assert_eq!(r(Look::Hs(323.0, 100.0)), Some(Cmd::Preset { name: "hotpink", level: 100 }));
        assert_eq!(r(Look::Kelvin(4255)), Some(Cmd::Preset { name: "coolwhite", level: 100 }));
    }

    #[test]
    fn a_look_keeps_the_current_level_and_a_brightness_keeps_the_current_look() {
        let st = both("red", 40).state;
        let look = resolve(&Intent::On { look: Some(Look::Effect("green")), brightness: None }, &st, false);
        assert_eq!(look, Some(Cmd::Preset { name: "green", level: 40 }));
        let b = resolve(&Intent::On { look: None, brightness: Some(255) }, &st, false);
        assert_eq!(b, Some(Cmd::Preset { name: "red", level: 100 }));
    }

    #[test]
    fn a_brightness_on_a_hex_snaps_it_first() {
        let mut st = State::default();
        st.record(false, Last::Color((0xFF, 0x10, 0x10))); // nearly red, not a preset
        let cmd = resolve(&Intent::On { look: None, brightness: Some(128) }, &st, false).unwrap();
        assert!(matches!(cmd, Cmd::Preset { name: "red", .. }), "{cmd:?}");
    }

    #[test]
    fn a_brightness_on_a_lit_frame_is_ignored_and_on_an_off_one_turns_it_on() {
        let mut st = State::default();
        st.record_pattern(false, &[(1, 2, 3); 38]);
        assert_eq!(resolve(&Intent::On { look: None, brightness: Some(100) }, &st, false), None);
        st.record(false, Last::Off);
        assert_eq!(resolve(&Intent::On { look: None, brightness: Some(100) }, &st, false), Some(Cmd::On));
    }

    /// Off with a saved color nothing recognises: `on` brings it back as it
    /// was. Snapping it to the nearest preset would throw away what `off` kept.
    #[test]
    fn a_brightness_while_off_over_a_saved_hex_is_a_plain_on() {
        let mut st = State::default();
        st.record(false, Last::Color((0xFF, 0x10, 0x10)));
        st.record(true, Last::Color((0xFF, 0x10, 0x10)));
        st.record(false, Last::Off);
        st.record(true, Last::Off);
        assert_eq!(resolve(&Intent::On { look: None, brightness: Some(128) }, &st, false), Some(Cmd::On));
    }

    #[test]
    fn a_brightness_while_off_scales_what_was_lit() {
        let mut st = both("red", 100).state;
        st.record(false, Last::Off);
        st.record(true, Last::Off);
        let cmd = resolve(&Intent::On { look: None, brightness: Some(1) }, &st, false);
        assert_eq!(cmd, Some(Cmd::Preset { name: "red", level: MIN_LEVEL }));
    }

    #[test]
    fn the_card_is_the_base_only_when_the_strip_is_dark_and_no_follow_pends() {
        let mut st = both("blue", 100).state;
        st.record(false, Last::Off);
        assert!(base_is_gpu(&st, false));
        assert!(!base_is_gpu(&st, true), "a pending follow makes the strip the base");
    }

    // ---- brightness mapping ---------------------------------------------

    #[test]
    fn brightness_maps_onto_min_level_to_100_and_back_within_its_quantisation() {
        assert_eq!(level_for(1), MIN_LEVEL);
        assert_eq!(level_for(255), 100);
        assert_eq!(brightness_for(100), 255);
        assert_eq!(brightness_for(MIN_LEVEL), 1);
        assert_eq!(brightness_for(MIN_LEVEL - 1), 1, "a hand-set low level reports at the bottom");
        // Half a level's width plus half a step, from MIN_LEVEL — not a fixed
        // "one step", which doesn't hold for every floor.
        let span = 254.0 / f64::from(100 - MIN_LEVEL);
        let bound = (span / 2.0 + 0.5).floor() as i32 + 1;
        for b in 1..=255u8 {
            let back = brightness_for(level_for(b));
            assert!((i32::from(back) - i32::from(b)).abs() <= bound, "{b} → {} → {back}", level_for(b));
        }
    }

    /// A scene restore sends back the brightness the bridge reported, and the
    /// bridge turns it into a level again. That round trip must be exact, or
    /// every off/on cycle through a scene would drift the level a step.
    #[test]
    fn a_reported_brightness_comes_back_as_the_same_level() {
        for level in MIN_LEVEL..=100 {
            assert_eq!(level_for(brightness_for(level)), level, "level {level} drifted");
        }
    }

    // ---- args -------------------------------------------------------------

    #[test]
    fn commands_go_out_by_name_with_the_level_only_when_dimmed() {
        assert_eq!(Cmd::Preset { name: "warmwhite", level: 40 }.args(true), ["warmwhite", "--level", "40", "--gpu"]);
        assert_eq!(Cmd::Preset { name: "red", level: 100 }.args(false), ["red"]);
        assert_eq!(Cmd::On.args(true), ["on", "--gpu"]);
    }

    // ---- projection -----------------------------------------------------

    #[test]
    fn the_state_table() {
        let none = State::default();
        assert_eq!(project(&none, false).on, None, "unknown is not ON");

        let mut s = State::default();
        s.record(false, Last::Off);
        assert_eq!(project(&s, false), Projection { on: Some(false), look: None });

        let lit = both("warmwhite", 40).state;
        assert_eq!(project(&lit, false).look, Some(Shown::Preset { name: "warmwhite", level: 40 }));

        let mut card_only = lit.clone();
        card_only.record(false, Last::Off);
        let p = project(&card_only, false);
        assert_eq!(p.on, Some(true));
        assert_eq!(p.look, Some(Shown::Preset { name: "warmwhite", level: 40 }), "look from the card");
        assert_eq!(project(&card_only, true).on, Some(false), "a pending follow reports the strip alone");
    }

    #[test]
    fn a_hex_or_a_frame_reports_its_wire_value_as_an_approximation() {
        let mut s = State::default();
        s.record(false, Last::Color((0x12, 0x34, 0x56)));
        assert_eq!(project(&s, false).look, Some(Shown::Approx((0x12, 0x34, 0x56))));
        s.record_pattern(false, &[(9, 8, 7), (1, 1, 1)]);
        assert_eq!(project(&s, false).look, Some(Shown::Approx((9, 8, 7))));
    }

    // ---- sequences: the plan's worked cases -------------------------------

    #[test]
    fn a_command_goes_to_the_strip_at_once_and_the_card_follows_once_quiet() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        assert_eq!(run(&mut m, &mut obs, 5), vec![], "not settled yet");
        assert_eq!(run(&mut m, &mut obs, 10), vec![Leg::Strip(Cmd::Preset { name: "red", level: 100 })]);
        assert!(m.follow_pending());
        assert_eq!(run(&mut m, &mut obs, 40), vec![], "not quiet yet");
        assert_eq!(run(&mut m, &mut obs, 60), vec![Leg::Gpu(Cmd::Preset { name: "red", level: 100 })]);
        assert!(!m.follow_pending());
    }

    /// Red, then brightness 40% before the follow runs: the second command
    /// resolves against the strip, already Red.
    #[test]
    fn a_brightness_while_the_card_lags_resolves_against_the_strip() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&on(None, Some(level_brightness(40))), 20);
        let legs = run(&mut m, &mut obs, 30);
        assert_eq!(legs, vec![Leg::Strip(Cmd::Preset { name: "red", level: 40 })]);
        assert_eq!(m.follow().unwrap().cmd, Cmd::Preset { name: "red", level: 40 });
    }

    fn level_brightness(level: u8) -> u8 {
        (1..=255).find(|&b| level_for(b) == level).unwrap()
    }

    #[test]
    fn a_ups_hook_in_between_drops_the_follow_on_the_next_tick() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        // The hook: `amber --all --stash`.
        obs.stash = Some("1-1".into());
        obs.state.record(false, color("amber", false, 100));
        obs.state.record(true, color("amber", true, 100));
        obs.state.rev += 2;
        assert!(matches!(m.next(&obs, 15), Step::Note(_)), "dropped at once, not when pacing allows");
        assert!(!m.follow_pending());
        assert_eq!(run(&mut m, &mut obs, 500), vec![], "and the card is never written");
    }

    /// The values match what the follow observed — only `rev` shows the tray
    /// picked Blue for the card in between.
    #[test]
    fn a_repeated_value_by_someone_else_still_drops_the_follow() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        obs.state.record(true, color("blue", true, 100)); // already Blue: same value
        obs.state.rev += 1;
        run(&mut m, &mut obs, 15);
        assert!(!m.follow_pending());
    }

    #[test]
    fn red_then_a_plain_on_keeps_the_cards_red_follow() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&on(None, None), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![], "the strip is lit, so `on` has nothing to do");
        assert_eq!(m.follow().unwrap().cmd, Cmd::Preset { name: "red", level: 100 });
        assert_eq!(run(&mut m, &mut obs, 100), vec![Leg::Gpu(Cmd::Preset { name: "red", level: 100 })]);
    }

    #[test]
    fn red_then_a_ups_hook_then_a_plain_on_leaves_the_stash_alone() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        obs.stash = Some("1-1".into());
        obs.state.record(false, color("amber", false, 100));
        obs.state.record(true, color("amber", true, 100));
        obs.state.rev += 2;
        m.on_message(&on(None, None), 20);
        let legs = run(&mut m, &mut obs, 500);
        assert!(legs.iter().all(|l| matches!(l, Leg::Gpu(Cmd::On) | Leg::Strip(Cmd::On))), "{legs:?}");
        assert!(!legs.iter().any(|l| matches!(l, Leg::Gpu(Cmd::Preset { .. }))), "red reached the card");
    }

    #[test]
    fn off_then_on_before_the_follow_leaves_a_lit_card_untouched() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("red", 100);
        m.on_message(&off(), 0);
        assert_eq!(run(&mut m, &mut obs, 10), vec![Leg::Strip(Cmd::Off)]);
        m.on_message(&on(None, None), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![Leg::Strip(Cmd::On)]);
        assert_eq!(run(&mut m, &mut obs, 500), vec![], "the card's `on` has nothing to do");
        assert_eq!(obs.state.gpu, Some(color("red", true, 100)));
    }

    #[test]
    fn off_then_on_with_the_card_already_off_brings_the_card_back() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("red", 100);
        obs.state.record(true, Last::Off); // card already off, red kept
        m.on_message(&off(), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&on(None, None), 20);
        run(&mut m, &mut obs, 30);
        assert_eq!(run(&mut m, &mut obs, 500), vec![Leg::Gpu(Cmd::On)]);
        assert_eq!(obs.state.gpu, Some(color("red", true, 100)));
    }

    /// Both Blue; Red at 40%; OFF; ON + brightness — all before the follow:
    /// the base is the strip's lit Red, never the card's lagging Blue.
    #[test]
    fn a_command_while_the_card_lags_never_resurrects_its_old_look() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&on(Some(Look::Effect("red")), Some(level_brightness(40))), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&off(), 20);
        run(&mut m, &mut obs, 30);
        m.on_message(&on(None, Some(level_brightness(60))), 40);
        let legs = run(&mut m, &mut obs, 50);
        assert_eq!(legs, vec![Leg::Strip(Cmd::Preset { name: "red", level: 60 })]);
    }

    #[test]
    fn an_effect_while_the_card_lags_keeps_the_strips_level() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&on(Some(Look::Effect("red")), Some(level_brightness(40))), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&effect("green"), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![Leg::Strip(Cmd::Preset { name: "green", level: 40 })]);
    }

    /// Off → a scene within 5 s: one card command, Relax's, and the card's
    /// `off` never runs.
    #[test]
    fn off_then_a_scene_collapses_the_card_to_one_command() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&off(), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&effect("burntorange"), 20);
        let mut legs = run(&mut m, &mut obs, 30);
        legs.extend(run(&mut m, &mut obs, 1_000));
        assert_eq!(
            legs,
            vec![
                Leg::Strip(Cmd::Preset { name: "burntorange", level: 100 }),
                Leg::Gpu(Cmd::Preset { name: "burntorange", level: 100 }),
            ],
            "the card's `off` never ran"
        );
    }

    #[test]
    fn the_card_waits_out_its_interval_and_anyones_recent_write() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        obs.gpu_marker_age = Some(10); // someone wrote the card just now
        assert_eq!(run(&mut m, &mut obs, 100), vec![]);
        obs.gpu_marker_age = Some(250);
        assert_eq!(run(&mut m, &mut obs, 100).len(), 1);

        // And its own previous write paces the next one.
        m.on_message(&effect("green"), 110);
        run(&mut m, &mut obs, 120);
        obs.gpu_marker_age = None;
        assert_eq!(run(&mut m, &mut obs, 200), vec![], "only 100 ms since the bridge's own card write");
        assert_eq!(run(&mut m, &mut obs, 300), vec![Leg::Gpu(Cmd::Preset { name: "green", level: 100 })]);
    }

    #[test]
    fn a_minute_of_presses_is_one_card_write() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("red", 10);
        let mut legs = Vec::new();
        for i in 0..20u64 {
            m.on_message(&on(None, Some(level_brightness(10 + 4 * i as u8))), i * 20);
            legs.extend(run(&mut m, &mut obs, i * 20 + 15));
        }
        legs.extend(run(&mut m, &mut obs, 2_000));
        let gpu: Vec<_> = legs.iter().filter(|l| matches!(l, Leg::Gpu(_))).collect();
        assert_eq!(gpu.len(), 1, "{legs:?}");
        assert_eq!(legs.iter().filter(|l| matches!(l, Leg::Strip(_))).count(), 20);
    }

    #[test]
    fn a_repeat_within_the_window_is_skipped_but_not_with_a_stash() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&effect("red"), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![], "a repeat within the window");

        obs.stash = Some("1-1".into());
        m.on_message(&effect("red"), 40);
        let legs = run(&mut m, &mut obs, 50);
        assert_eq!(legs, vec![Leg::Strip(Cmd::Preset { name: "red", level: 100 })], "a stash means it must reach the CLI");
    }

    /// A strip leg that failed recorded nothing, so the record still shows the
    /// old color. A retry of the same command must reach the CLI, not be taken
    /// for a repeat of one that landed.
    #[test]
    fn a_retry_after_a_failed_apply_is_not_a_repeat() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("red"), 0);
        assert_eq!(m.next(&obs, 10), Step::Run(Leg::Strip(Cmd::Preset { name: "red", level: 100 })));
        m.on_done(&obs, 10, false); // the child failed: nothing recorded, rev unchanged
        m.on_message(&effect("red"), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![Leg::Strip(Cmd::Preset { name: "red", level: 100 })]);
    }

    /// A Red that failed part-way — the color changed live, the commit or the
    /// record didn't land — leaves the record saying Blue. A Blue sent to undo
    /// it matches the record but is no repeat of anything the bridge did: it
    /// must reach the strip.
    #[test]
    fn a_correction_after_a_partial_failure_is_not_a_repeat() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        // A successful Blue first, so a "previous leg" exists to be confused with.
        m.on_message(&effect("blue"), 0);
        run(&mut m, &mut obs, 10);
        m.on_message(&effect("red"), 20);
        assert_eq!(m.next(&obs, 30), Step::Run(Leg::Strip(Cmd::Preset { name: "red", level: 100 })));
        m.on_done(&obs, 30, false); // failed; the record still says Blue
        m.on_message(&effect("blue"), 40);
        assert_eq!(run(&mut m, &mut obs, 50), vec![Leg::Strip(Cmd::Preset { name: "blue", level: 100 })]);
    }

    /// The plan's rule is the wire value: `black` at 50% and `black` at full are
    /// the same bytes, so the second is a repeat.
    #[test]
    fn a_repeat_is_judged_by_wire_value_not_arguments() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        m.on_message(&effect("black"), 0);
        assert_eq!(run(&mut m, &mut obs, 10), vec![Leg::Strip(Cmd::Preset { name: "black", level: 100 })]);
        m.on_message(&on(Some(Look::Effect("black")), Some(level_brightness(50))), 20);
        assert_eq!(run(&mut m, &mut obs, 30), vec![], "black at 50% is the same bytes as black");
    }

    #[test]
    fn the_guard_drops_strip_legs_but_still_lets_off_through_and_the_card_follow() {
        let mut m = Model::new(fast(), 0);
        let mut obs = both("blue", 100);
        let names = ["red", "green"];
        for i in 0..BUCKET_CAPACITY as u64 {
            m.on_message(&effect(names[(i % 2) as usize]), i);
            assert_eq!(run(&mut m, &mut obs, i + 10).len(), 1, "leg {i} within capacity");
        }
        m.on_message(&effect("amber"), 100);
        assert_eq!(run(&mut m, &mut obs, 110), vec![], "the 31st strip leg is dropped");
        assert_eq!(m.follow().unwrap().cmd, Cmd::Preset { name: "amber", level: 100 }, "but the card still follows");

        m.on_message(&off(), 120);
        assert_eq!(run(&mut m, &mut obs, 130), vec![Leg::Strip(Cmd::Off)], "an off always gets through once");
        m.on_message(&on(None, None), 140);
        run(&mut m, &mut obs, 150);
        m.on_message(&off(), 160);
        assert_eq!(run(&mut m, &mut obs, 170), vec![], "but only once per allowance");
    }
}
