# Plan: driving the lights from a Stream Deck

Status: **planned, not started.** Written 2026-09-06 while designing the
consumer, which is itself deferred. Nothing here is urgent; it exists so the
three small changes jdrgb owes a second consumer are written down while the
reasoning is fresh.

The consumer is the Stream Deck plugin in `../streamdeck-now-playing` — a
resident C# process that already owns dials for media, volume, deck brightness
and monitor brightness, and would gain one for the LEDs. (That plugin is being
renamed; "Sound and Light" is the leading candidate, and an LED module is part
of why.)

## Goal

Let a Stream Deck key or dial set the LEDs and show what they are currently
showing, **without jdrgb growing a daemon, an IPC surface, or a second
implementation of the device layer.**

Explicitly *not* a general remote-control API. The plugin is a second tray: it
spawns the CLI and reads what the CLI recorded. If that shape is not enough for
something, the answer is a better CLI, not a socket.

## Decision summary

| Question | Decision |
|---|---|
| How does the plugin apply a color? | Spawns `jdrgb.exe`, exactly as `src/tray/worker.rs` does |
| How does it know the current color? | Watches `%LOCALAPPDATA%\jdrgb\last` — no polling, no spawning |
| Does jdrgb gain a daemon or IPC? | **No.** See the README's first paragraph |
| Does jdrgb ship a `cdylib` for P/Invoke? | **No.** `lib.rs` keeps the device layer out of the library on purpose |
| What does jdrgb actually owe? | Three things: a HID lock, `presets --json`, and a documented state-file format |

## Why the plugin is a consumer, not a second implementation

`src/lib.rs` states the shape already: *"the tray never touches hardware; it
spawns the CLI and reads what the CLI recorded."* Everything a second consumer
needs is the same set of things the tray needed, and all of it exists:

- **Apply** — `jdrgb <preset|hex> [--gpu|--all]`, already the whole CLI surface.
- **Read back** — `load_last()` (`src/palette.rs:533`) over the state file at
  `state_path()` (`src/palette.rs:324`), documented at `src/palette.rs:482` as
  *"The tray reads this file but never writes it."*
- **Borrow and return** — `--stash` / `restore`, already built for exactly this.
- **Off vs black** — the `Last::Off` variant (`src/palette.rs:338`) that the
  tray build added means a second reader gets that distinction for free.

So the plugin is a port of `src/tray/worker.rs` into C#: same
`CREATE_NO_WINDOW`, same piped stderr carrying the CLI's own human-readable
message, same time-bounded child, same serialised queue. That file is the
specification; read it before writing the C# one.

### One thing the plugin does better than the tray

The tray re-reads the state file every time the menu opens (tray-plan.md,
Phase 5) because it has a natural moment to do so. A Stream Deck key is always
on screen and has no such moment, so it should put a `FileSystemWatcher` on
`%LOCALAPPDATA%\jdrgb\last` instead. That gives it push-based state with zero
polling and zero process spawns, and it picks up a `jdrgb red` typed in a
terminal as fast as it picks up its own writes.

This is the whole reason no `status --json` command is needed. See Phase 3.

## Work

### Phase 1 — serialise the HID path across processes

**The only correctness gap, and it exists today.**

`src/gpu.rs:263` already takes a named cross-process mutex, `Global\jdrgb-gpu-i2c`,
with `BusLock` and an honest account of what it does and does not cover. The
HID path in `src/hid.rs` takes nothing.

Today that is survivable: the tray serialises its own jobs on one worker thread
(`src/tray/worker.rs`, *"two `jdrgb.exe` processes writing the same controller
at once is a race with no winner worth having"*), and it is usually the only
thing driving the CLI. Add a second resident process and that assumption is
gone — the tray and the plugin can spawn `jdrgb.exe` at the same instant, and
neither knows about the other's queue.

Do the same thing `gpu.rs` does:

- A `Global\jdrgb-hid` mutex acquired around the device write, released on drop.
- Busy is a **transient** error, so `--wait` retries it — same reasoning as
  `BusLock::acquire`, where treating busy as a free pass reintroduced the exact
  race the lock exists to prevent.
- The same cross-account caveat applies verbatim: an object created by the
  SYSTEM boot task carries that token's default DACL, so serialisation between
  the boot task and a logged-in user is not guaranteed. Between processes of one
  user — the tray, the plugin, a hand-run command — it is. That is the case that
  matters here.

This is worth doing whether or not the plugin ever gets built. It closes a race
that is currently prevented only by there being one caller.

### Phase 2 — `presets --json`

The property inspector needs to render its own swatches, which means it needs,
per preset: the name, the strip value, the GPU value where one is calibrated,
and the swatch appearance (`swatch_rgb`) — which the README notes is *not*
derivable from the wire value, since `coolwhite`'s `#FFB0D0` draws as pink.

`list_presets()` (`src/main.rs:1810`) prints that for humans, with terminal
swatches and a `gpu` column. Scraping it from C# would be brittle, and
hard-coding the table into the plugin would drift the next time a color is
tuned by eye — the same argument tray-plan.md made for putting the tray in this
crate rather than beside it.

So: `jdrgb presets --json`, one array of objects, stable field names. This is
the one genuinely new output mode in the whole plan.

### Phase 3 — write down the state file as a contract

`%LOCALAPPDATA%\jdrgb\last` currently has two readers — `load_last()` and the
tray — both inside this crate, so its format has never needed to be a promise.
A C# consumer parsing it makes that dependency real and external.

Cheaper than adding a `status --json` command, and it preserves the
watch-instead-of-spawn design in Phase 1 of the consumer's own work. Document in
the README:

- the path, and that it is **per-user**;
- the encoding, one record per device, and what `Off` and `Multi` mean;
- that a slot never recorded is absent rather than guessed
  (`src/main.rs:1000`);
- and the load-bearing caveat at `src/main.rs:994` — **this file reports what
  jdrgb recorded, not what the hardware is showing.** The strip's controller
  offers no color readback, so the record is the only source there. Anything
  else that repaints the strip makes the file quietly wrong.

Adding `status --json` stays open if a consumer ever turns up that cannot watch
a file. Nothing needs it today.

## Traps the consumer will hit

1. **A preset name resolves per device; an explicit hex is literal.** If the
   property inspector shows a color picker and sends hex with `--all`, the GPU
   will look wrong — it bypasses the entire calibration table in `GPU_PRESETS`.
   Pass names through for the calibrated path and treat hex as an advanced
   option. This is the single easiest thing to get wrong.
2. **A partial `--all` failure is a real state, not an error.** Each leg applies
   and records independently (`src/main.rs:383`) before either failure is
   reported, so a non-zero exit can still mean the strip changed. Re-read state
   after every child regardless of exit code. The tray learned this the same
   way (tray-plan.md, Phase 4).
3. **A dial is not a menu.** One detent means one process spawn plus one HID
   write. At a fast spin that is a queue of writers on a device that takes real
   time to answer. The plugin already debounces track-skip to one action per
   200 ms; the LED dial needs the same, or apply-on-settle.
4. **Balloon-free error reporting.** The CLI's stderr is one short line written
   for humans, and the plugin has `showAlert` and a strip that can display text.
   Use the CLI's own words rather than inventing a second vocabulary for the
   same failures.

## Known issue, inherited: the state file is per-user

Documented in tray-plan.md and unchanged here. The boot task runs as SYSTEM and
writes `C:\Windows\System32\config\systemprofile\AppData\Local\jdrgb\last`; the
Stream Deck app runs as the logged-in user and reads a different file.

Consequence for the plugin is identical to the tray's: it agrees with anything
the user does interactively and with its own actions, but not with the boot
apply, so at logon it shows nothing until something changes. Accepted, for the
same reasons.

Worth noting that a second consumer strengthens the case for option 3 in that
document — moving state to `%ProgramData%\jdrgb\last`. Two resident readers
both wrong at logon is more annoying than one. Still not urgent.

## Non-goals

A daemon. A socket, named pipe, or HTTP surface. A `cdylib` for P/Invoke — it
would contradict `lib.rs`'s deliberate exclusion of the device layer to save
about five milliseconds per apply. Effects, animations, or anything that needs
jdrgb to stay running: this crate sets a color and exits, and a Stream Deck
module is not a reason to change that.

If the plugin ever wants the LEDs to follow album art or a temperature, it can
call `jdrgb RRGGBB` on a timer like anything else, and it should think hard
about the HID write rate first.

## Appendix A — verified findings

Checked on this machine on 2026-09-06 while writing this plan:

- **`src/gpu.rs` locks the I2C bus; `src/hid.rs` locks nothing.** A grep for
  `CreateMutex` across `main.rs`, `hid.rs` and `gpu.rs` matches only `gpu.rs`.
  This is the basis of Phase 1.
- **State lives at `%LOCALAPPDATA%\jdrgb\last`**, from `state_path()`
  (`src/palette.rs:324`).
- **`palette.rs` is the shared half** — `Last`, `record`, `record_pattern`,
  `load_last`, `state_path` (`:338`, `:502`, `:516`, `:533`, `:324`) — and
  `lib.rs` exports only `palette`.
- **`--all` records per leg before reporting failure** (`src/main.rs:383`),
  which is what makes trap 2 real.
- **The tray's child timeout is 90 s**, deliberately above the CLI's own 60 s
  `--wait` ceiling (`src/tray/worker.rs`). A C# port should keep that
  relationship rather than picking a fresh number.
- *Unverified:* whether the Stream Deck app's plugin host imposes anything on
  child-process spawning — working directory, environment, or a job object that
  would kill a child on plugin restart. Check before Phase 1 of the consumer's
  work; it is the only thing that could invalidate the spawn-the-CLI shape.
