# Plan: the LEDs as a Home Assistant light

Status: **implemented on the `home-assistant` branch; acceptance on the real
install pending.** Written 2026-10-29 and revised the same night through eight
rounds of external review — the last returning READY — and feedback and live
checks from the house's own HA setup (Appendix B). Phases 1–5 are built and
tested without hardware; where the build departs from or adds to this plan,
*Implementation notes* below says so.

The consumer is this house's Home Assistant, and through it a Hue dimmer switch
and Apple Home (via HA's HomeKit Bridge). The goal is that the PC's LEDs show
up in both as an ordinary light, and can take part in the house's scenes.

## Goal

A light entity in Home Assistant — on/off, a color wheel, white temperature,
brightness, and the named presets — that reports what the LEDs are showing, so
a Hue dimmer, a phone, a scene or HomeKit can drive it like any other bulb.

**Without** `jdrgb.exe` growing a daemon or a network surface, without a second
implementation of the device layer, without the bridge putting more GPU I2C
traffic on the bus than a person using the tray would, and without pretending
the hardware can do continuous color it has never been calibrated for.

## Decision summary

| Question | Decision |
|---|---|
| Transport | **MQTT with HA discovery.** The PC connects *out* to the house's existing broker; nothing listens on the PC |
| Broker auth | **None on the PC.** Anonymous on the broker's LAN listener, whose shared anonymous ACL the bridge's topics join — the pattern the house's other devices use. `user`/`password` stay optional for other installs |
| What runs on the PC | `jdrgb-ha.exe`, a third binary in this crate, one per machine, started at logon for one configured user |
| Does it touch hardware? | **No.** It spawns `jdrgb.exe`, as the tray does |
| MQTT client | `rumqttc`'s synchronous client, default features off (no TLS); `serde_json` for payloads |
| Light model | Level 1: on/off + presets as effects. Level 2: color wheel and white temperature *snapped to presets*, plus brightness |
| Brightness | New `--level N` on `jdrgb.exe`: scale after per-device calibration, record the wire value |
| Source of truth | The state file. The bridge *recognises* (preset, level) from it |
| Flash | Strip solid and `off` applies commit, exactly as a tray click does. Strip frames and GPU applies don't; the bridge never runs `--gpu save` |
| GPU traffic | Each command goes to the strip at once; the card then *follows the strip*, replaying the same command once the light has settled, at most once a minute. Between callers, the CLI's default 1.5 s gap is unchanged |
| Duplicate commands | Reach the CLI, like tray clicks — with one narrow exception for the bridge repeating itself |
| One entity or two? | One. Presets resolve per device, so one name looks the same on both |
| Boot task | Mutually exclusive with `-Ha`, enforced whichever is installed first |
| HomeKit | No code. Added by name to HA's HomeKit Bridge include list |
| Matter | Out of scope for this house: no Matter server, and HomeKit already covers Apple Home |

## Why the calibration is not an obstacle

A proper light speaks in *appearance* — "this hue, this saturation" — never in
wire values. jdrgb already keeps those apart: `SWATCH` (`src/palette.rs:161`)
is what a preset looks like, and `PRESETS` / `GPU_PRESETS` are what each device
must be sent to look like that. So the bridge translates in both directions:

- **Command in:** requested appearance → nearest preset → `jdrgb.exe <name>`.
  The *name* goes to the CLI, so each device gets its own calibration. The
  bridge never forwards HA's RGB as hex: a hex is literal and skips
  `GPU_PRESETS` entirely (Stream Deck plan, trap 1).
- **State out:** recorded wire values → which preset and level produced them →
  that preset's `SWATCH` appearance.

What is missing is coverage, not structure. The tables are 24 points on the
full-saturation ring, two whites and a pastel pink, and there is no data off
that ring (README, *Interpolated presets*). So the light *snaps*: HomeKit's
wheel lands on the nearest preset and the thumb moves to where the LEDs are.

Continuous color is a measurement project — saturation has no data, low
brightness distorts ratios, the blue region's `SWATCH` entries are untuned —
and is a non-goal.

## Why MQTT, not HTTP or SSH

- **SSH** works with no code, but it is a general remote shell: safe only if
  password auth is off and the key is restricted to jdrgb.
- **HTTP** (a listener on the PC) is least-privilege by construction, but opens
  an inbound port, needs a firewall rule, and gives HA state only by polling.
- **MQTT** inverts the connection: the PC dials out to the existing Mosquitto
  broker, so **no port is opened on the PC**. Discovery builds the entity from
  one retained message, state is pushed both ways, and the broker's last will
  marks the light *unavailable* when the PC sleeps or the bridge dies.

The house already runs the broker and other devices already use it, so the cost
is one ACL entry (*This install*, below). The bridge connects anonymously, so no
secret lives on the PC. The threat model is the one already accepted for an
unauthenticated LAN protocol: anyone on the LAN can change the PC's LEDs. The
ACL bounds what anonymous clients can do as a group — the house's anonymous
devices share one allowlist — rather than isolating this client. An install that
prefers a broker user sets `user`/`password` in `ha.conf`; TLS stays possible
later.

MQTT discovery is used rather than hand-written YAML in HA's `mqtt.yaml`, which
is how some of the house's devices are defined. Discovery lets the bridge define
its own entity and, with `--unpublish`, delete it cleanly.

`docs/streamdeck-plan.md` lists "a socket, named pipe, or HTTP surface" as a
non-goal. That still holds for `jdrgb.exe`. Remote control needs *something* on
the PC to receive commands; of `sshd`, a listener, or an outbound client, the
outbound client can do the least.

## This install

The plan was first written against a generic HA install. This house differs in
ways that matter for Phase 3:

- **No add-ons.** HA (2026.10.4) runs as a Docker container in a Lima VM on a
  Mac mini, not on HAOS, so there is no Supervisor. Mosquitto runs as its own
  container beside it and already serves other devices. Phase 3 needs no broker
  setup beyond the ACL entry below.
- **Devices connect anonymously** to the broker's LAN listener on port 1884. The
  house rule is that no secret lives on a device, and the bridge follows it.
  Anonymous clients there share **one** allowlist: Mosquitto applies the ACL
  file's leading `topic` lines to every anonymous client together, and nothing in
  them names a client. So the bridge's entry adds its topics to what any anonymous
  LAN client may do, and the bridge may also use the topics other anonymous
  devices were given. The ACL bounds anonymous LAN clients as a group; it doesn't
  isolate one from another. That is the boundary the house already accepts for
  its devices. The bridge's entry, with `<name>` the PC's lowercased computer name
  (the `<id>` of Phase 3 is `jdrgb_<name>`):

  ```
  topic readwrite jdrgb/jdrgb_<name>/#
  topic write     homeassistant/light/jdrgb_<name>/#
  topic read      homeassistant/status
  ```

  `readwrite`, not `write`, on the first line: the bridge *subscribes* to its
  own command topic, `jdrgb/jdrgb_<name>/set`, as well as publishing state and
  availability there. The will is published to that subtree too. The broker's
  owner adds the entry; the broker is theirs to touch.

  **An ACL mistake is invisible in the protocol on this broker.** Tested live:
  Mosquitto 2.0.22 *grants* a subscription its ACL denies — SUBACK `0`, not
  `0x80` — and enforces the read ACL at delivery instead. And in MQTT 3.1.1 a
  broker can't refuse a publish, so a denied one is dropped silently. A missing
  `read` gives a bridge that is online but deaf; a missing `write`, one that
  connects and is never seen. So the bridge proves its own access with an echo
  before it goes `online` (Phase 3, *Connecting*), and acceptance is checked from
  the HA side.
- **The listener's address** goes in `ha.conf`. Port 1884 is on the HA VM, which
  has its own pinned LAN address — not the Mac mini's, whose addresses all refuse
  1884. The broker's owner supplied it, and a TCP connect from the PC succeeded on
  2026-10-29. It is a private LAN address and stays out of this document.
- **HomeKit takes an explicit list.** The HomeKit Bridge exposes only the
  entities named in its include list, so the light won't appear in Apple Home by
  itself (Phase 3, acceptance).
- **No Matter server**, and HomeKit already covers Apple Home: Matter is out of
  scope here.
- **Scenes snapshot and restore** with `scene.create` (*Scenes*, below).

## Why a third binary, and why `rumqttc`

Same reasoning as the tray: a resident process that spawns the CLI keeps the
device layer in one place, and a wedged controller can't take the bridge with
it. It is not folded into the tray, which never touches the network and should
keep it that way; the bridge has no UI and runs whether or not the tray is
installed.

A hand-written MQTT subset was the first draft; review found it the largest
over-build in the plan. `rumqttc` handles framing, keepalive and reconnect,
maintained. It does **not** validate SUBACK return codes or keep its own
blocking calls off the polling thread — both are the bridge's job, specified in
Phase 3. Its cost is `tokio` underneath the sync API, linked into
`jdrgb-ha.exe` only: Phase 3 checks that `jdrgb.exe` and `jdrgb-tray.exe`
release sizes don't change and records `jdrgb-ha.exe`'s in the README. The
hidapi lesson was about *enumerating devices*, not about network dependencies.

## Work

### Phase 1 — make concurrent callers safe (prerequisite)

Today one caller at a time is the norm, because the tray queues its own clicks.
The bridge is a second resident caller that has never heard of the tray. Both
halves of this phase are bugs that exist today; the bridge makes them likely.

**1a. Serialise the strip.** As `docs/streamdeck-plan.md`, Phase 1 specifies —
a `Global\jdrgb-hid` mutex — with these tightenings:

- **Scope.** Taken before the *first* device operation and held until the
  transaction's state is recorded. Discovery is not read-only: `read_config`
  sends a request report (`request`, `src/main.rs:556`), so `open_controller`
  is inside the lock. The natural shape is a lock owned by `Device`, released on
  drop. `tune` and `preview` hold it for the session, as they already do for the
  GPU bus.
- **Fail closed.** `gpu.rs` proceeds unserialised if its mutex can't be created
  (`src/gpu.rs:280`). The HID lock does not: failure to create *or open* it is a
  transient error, so `--wait` retries and the tray reports it.
- **Busy is transient**, after the same 10 s wait as `BusLock::acquire`.
- **Lock order.** A process that holds both takes the HID lock before the GPU
  bus lock. The state lock (1b) is innermost and held only for one read or one
  write.

Cross-account serialisation (the SYSTEM boot task against a user) is still not
guaranteed, for the DACL reason in `src/gpu.rs:254-262`. Rather than fix that
for v1, the boot task and the bridge are never installed together (Phase 3).

**1b. Make the state file and its decisions transactional.**

*The file.* `update` (`src/palette.rs:597`) is an unlocked read-modify-write
ending in a truncating `std::fs::write`, so two processes can lose each other's
update and a reader can see a half-written or empty file.

- **Lock:** an exclusive `LockFileEx` on a sidecar `%LOCALAPPDATA%\jdrgb\state.lock`
  around each read-edit-write. Per-file, so naturally scoped to its user. After
  a short bounded wait the record is skipped with a message on stderr — it is
  best-effort today and stays so.
- **Atomic replace:** write `last.tmp`, then `std::fs::rename` over `last`.
  Readers see the old file or the new one. Rust's `File::open` shares delete
  access by default, so a Rust reader doesn't block the rename; a C# reader (the
  Stream Deck plan) must open with `FileShare.Delete`, and that plan gets a line
  saying so.
- **Record inside the device lock.** `apply_gpu_colors` (`src/main.rs:456`)
  drops the `Gpu` handle — and its bus lock — on return, and the GPU's
  `record(true, …)` happens afterwards (`src/main.rs:403`). Two processes can
  finish their writes in one order and record them in the other. Each device's
  record moves inside its lock scope.

*The decisions.* Locking each write is not enough where a command *decides*
from the record and then acts:

- **`on`** reads each device's slot and lit record while holding that device's
  lock, then applies and records before releasing it. Today `turn_on` reads the
  whole state before opening anything (`src/main.rs:1269`).
- **`restore`** records its legs in a *keep-lit* mode: a process-wide switch in
  `palette.rs` under which `record` skips the lit transition. This replaces
  `keeping_lit` (`src/palette.rs:630`), which snapshots both lit records before
  the apply and writes the snapshot back afterwards — overwriting a lit record
  another process wrote in between, including on a device whose lock the
  restore had already released. Process-wide is safe because `restore` is the
  whole of that process's work.
- **The stash.** `stash_now`'s check-then-write runs under the state lock, and
  every `--stash` stamps the stash with a fresh id in a comment line —
  `# stash-id: <ms since epoch>-<pid>` — so the config grammar is unchanged.
  *Every* `--stash`, including one that keeps an existing snapshot
  (`src/main.rs:1161`, the first-snapshot-wins rule): it keeps the snapshot's
  colors and refreshes only the id. Contents can't identify a borrow — the same
  lighting stashed twice is the same bytes, and a reused snapshot is the same
  bytes by definition — so without the refresh, a restore that paused mid-way
  could finish over a *newer* borrow and delete its record. `restore` reads the
  stash and keeps its id. Before each device leg, holding that device's lock, it
  checks the stash still carries that id; if it was removed, replaced or
  re-stamped, someone has decided since, so it stops there, leaves the stash
  intact, and reports what landed. After the last leg it deletes the stash under
  the state lock only if the id still matches. Today's sequence — read, apply
  both, unconditional `remove_file` (`src/main.rs:1196-1205`) — has neither check.
- **A revision counter.** Every `update` increments a `rev=` key under the state
  lock, whether or not any value changed. It lets a reader tell that *someone
  wrote*, even when they wrote identical values — which the bridge needs (Phase
  3, the follow). A counter rather than the modification time, because file
  timestamps come from a clock that can tick as coarsely as ~15 ms, so two
  writes could share one. The parser already skips unknown keys, so older
  readers ignore it.

*Tests*, no hardware: `update` takes its path internally so a test can point it
at a temp directory; threads hammering `record` on alternating devices lose
nothing; a reader looping during writes never parses an empty or partial file;
`record` in keep-lit mode leaves lit records alone. Cross-process interleavings
of device work are guaranteed by lock scope rather than tested: the device layer
has no seam to fake, and adding one for tests is out of proportion to the risk.
The lock scopes are what review should check.

### Phase 2 — `--level N`

`jdrgb warmwhite --all --level 40`. Each device resolves the preset with its
own calibration *first*, then every channel becomes `(c × N + 50) / 100` in
integer arithmetic. Order matters: scaling the shared value and then
calibrating would dim the strip's value on the GPU. The *scaled* value is what
goes on the wire and what is recorded, so the state file keeps meaning "what
was sent", and `state`, `load`, `--stash`/`restore` and `off`/`on` carry a
dimmed color through unchanged.

`--level` takes 1–100. With anything other than a solid color — `off`, `on`,
`load`, `rainbow`, `tune`, `preview`, `restore` — it is an error, not a silent
no-op.

Below some level the 8-bit channels lose their ratios (`coolwhite` at 2% is
`(5,4,4)`) and presets start rounding to the same bytes. `palette.rs` gains a
`MIN_LEVEL`; the CLI accepts the full range, and the bridge maps HA's brightness
onto `MIN_LEVEL..=100`. Its value must pass two tests: it looks right on both
devices, and an exhaustive test finds no two presets that round to the same
wire value at any level from `MIN_LEVEL` up. (Today the lowest collision-free
floor is 10: Cerise and Hot Pink both become `(23,0,2)` on the GPU at 9.)

*Settled on the real install:* `MIN_LEVEL` is **3**. The collision test now
applies to the strip, which is the device the bridge reads, resolves against
and reports, unless the strip is dark and the card alone is lit. The strip is
collision-free from 3; at 2, Cobalt and Sapphire both become `(0,1,5)`. The
card keeps its own bound of 10. Below it, the presets that merge are reds and
pinks that are the same bytes at that level, so the worst case is a card lit
alone being labelled as its twin. By eye, warm white still looked right on
both devices at 1.

Linear scaling is the starting point because the LEDs are PWM-driven. Whether
the slider wants a perceptual curve is decided by eye in Phase 4, in the
bridge's mapping — never in `--level`, which stays a plain multiplier.

### Phase 3 — the bridge, level 1: on/off and presets

`src/ha/main.rs` → `jdrgb-ha.exe`, `windows_subsystem = "windows"`, no window.

**Ownership: one bridge per machine.** A `Global\jdrgb-ha` mutex: an instance
that finds it held, *or is denied access to it*, exits. The MQTT client id is
`jdrgb-ha-<name>`, so a second bridge anywhere kicks the first off the
broker rather than silently sharing its topics. `install.ps1 -Ha` installs for
one user, carrying that user's SID through elevation as `-Tray` does
(`install.ps1:140`, `:292`). Multi-user arbitration is a non-goal.

**Config.** `%LOCALAPPDATA%\jdrgb\ha.conf`, `key=value` like the state file:

```
host=<the broker's LAN listener>
port=1884
```

`host` is required. `port` defaults to 1883, the MQTT standard; this house uses
1884. `name` is optional: the device's name in HA, which also names the entity.
It defaults to the computer's name plus "LEDs" — a PC named `DESK` becomes
`Desk LEDs`, entity `light.desk_leds` — because a house can have more than one
PC, and a fixed default would collide. `user` and `password` are optional and
absent here: without them the bridge connects anonymously. Missing `host`, or an
unreadable file, is logged and the bridge exits. The installer resolves *that
user's* profile path from the SID (not the elevated admin's), checks the file
exists, and prints what to put in it if not.

**Logging.** `%LOCALAPPDATA%\jdrgb\ha.log`, truncated to its last half when it
passes a small size. There is nowhere else for an error to go.

**Spawning.** The HWND-free half of `src/tray/worker.rs` — `run_with`,
`cli_path`, `CREATE_NO_WINDOW`, the 90 s `CHILD_TIMEOUT` — moves to
`src/lib.rs` as `jdrgb::spawn`, used by both resident binaries. The tray keeps
its `PostMessageW` wrapper. `lib.rs`'s "only colors and state" comment widens to
"colors, state, and how to run the CLI"; the device layer stays out. The bridge
never passes `--wait`: a light command should fail fast, not retry for a minute.

The shared result keeps a successful child's stderr too. Today `run_with`
returns an empty message for any zero exit (`src/tray/worker.rs:120`), which
would hide the one warning that matters most here: a hardware apply that
succeeded while its record was skipped (1b), the reason the bridge would then
report stale state. The bridge logs such stderr as a warning; the tray can
ignore it, as now. Tested with a harmless child that writes to stderr and exits
zero.

**Children die with the bridge.** Windows doesn't end a process's children when
the parent is killed, and `run_with`'s timeout only protects a child while the
parent is alive to call `kill()` (`src/tray/worker.rs:99-102`). A bridge killed
at logoff or by uninstall would leave a `jdrgb.exe` mid-write with nobody
watching it. So at startup the bridge puts itself in a job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (one more `windows-sys` feature,
`Win32_System_JobObjects`); children are associated with the job and end with
the bridge. A child ended mid-write is the case the tray's timeout already
accepts.

The job handle is created non-inheritable, so a child holding a copy can't keep
the job alive, and it is **never closed** — it lives exactly as long as the
process, because closing the last handle ends every process in the job,
including the one that closed it. If job setup fails, the resident logs it and
exits rather than run with unowned children. `--unpublish` spawns no child and
skips the job entirely, so nothing can end it before it reports its exit code.
Tested with a harmless sleeping child, never with hardware: killing the bridge
ends the child; `--unpublish` exits 0.

**Threads.** Two. Three facts about `rumqttc` 0.25.1 shape them: `publish` and
`subscribe` are blocking sends on a bounded channel that only the connection's
polling drains, so a blocking call on the polling thread can deadlock it;
`recv_timeout` builds a fresh poll future each call and drops it on timeout,
and since the event loop only keeps a connection once TCP *and* CONNACK have
completed, a short timeout aborts every slow handshake; and the plain blocking
`recv` / `iter` is never cancelled, its handshake bounded by `rumqttc`'s own
connection timeout.

- *Network* runs the blocking `connection.iter()` and owns the connection's
  lifecycle — subscribing, availability, discovery — using only the
  non-blocking `try_*` calls, so it can never wait on the queue it drains. It
  parses incoming commands and hands them to the worker, and sets a shared
  *ready* flag (below). Every event wakes it, and keepalive pings guarantee one
  at least every 30 s; that is when it retries a lifecycle step the queue
  refused and checks its own timers, such as the subscribe retry.
- *Worker* owns the pending intent, the follow, pacing and children. It wakes on
  a message or a 1 s tick. **Every tick** reads the state file (a few hundred
  bytes, no spawn) *and* the stash's identity, and validates a pending follow
  against both, whatever `rev` says: a hook can re-stamp the stash and then fail
  its hardware write, since `stash_now` runs before the apply
  (`src/main.rs:164`), which changes the stash and not `rev`. An unchanged `rev`
  only spares recomputing the projection. Whenever its projected state changes,
  the worker updates the shared **latest-state slot** and publishes it (below);
  if the queue refuses, the slot keeps it for the next tick. A 90-second child
  blocks only this thread.

The network thread never computes state itself. On readiness and on HA's birth
message it publishes whatever the latest-state slot holds, so reconnecting can't
undo the pending-follow rule (*State out*) by publishing a projection of the raw
file. The worker updates the follow *before* recomputing the projection, and the
projection *before* the slot.

**One lock orders the two publishers.** The slot is a mutex, and each thread
holds it across both choosing the payload *and* calling `try_publish` — the
worker while it writes a new projection and publishes it, the network thread
while it reads the slot and publishes it. Otherwise the network thread could read
an old `OFF`, the worker write and publish a newer `ON`, and the network thread
then enqueue its stale `OFF` last, leaving the wrong state retained until the
next change. `try_publish` never blocks, so the critical section is short. A
deterministic test drives that interleaving and asserts that an older projection
can never be enqueued after a newer one.

State is the only thing the worker publishes. It is retained and idempotent, so
one that slips onto a new connection early does no harm, and the lifecycle
messages whose order matters never leave the network thread.

Keepalive is 30 s, driven by the polling, so a slow child can't drop the
connection. After a connection error the network thread clears *ready* and
waits 1 s before polling again — `rumqttc` reconnects on the next poll with no
delay of its own — and logs a repeated identical error once rather than every
second.

**Connecting.** Everything here is scoped to one connection; a connection error
discards its *ready* flag, its awaited SUBACK and its lifecycle stage. The
request queue holds 32 entries. `try_*` doesn't retry a request the full queue
refuses — it returns an error — so the steps below are a **stage machine**: each
advances only once its request was accepted, and a refused one keeps its stage
and is retried on the next network event. *Ready* is set only once every startup
message has been accepted. A test with a one-entry queue and forced refusals
checks that nothing is skipped. On `ConnAck` the network thread:

1. publishes a retained **`offline`** to the availability topic. Withholding
   `online` isn't enough on its own: a retained `online` from an earlier
   connection would otherwise stand, and a live connection never fires its will;
2. subscribes to the command topic, an echo topic `jdrgb/<id>/echo`, and
   `homeassistant/status` in one SUBSCRIBE, noting its packet id from the
   `Outgoing::Subscribe` event;
3. on the matching SUBACK, checks the return codes — `rumqttc` doesn't
   (`handle_incoming_suback` is `Ok(None)`). A `0x80` is logged as a broker ACL
   problem, availability stays `offline`, and the subscribe is retried every
   5 minutes on the same connection. That covers brokers that report a denial;
   this house's doesn't (*This install*), which is what the next step is for;
4. publishes a fresh random nonce, not retained, to `jdrgb/<id>/echo`, and waits
   for it to come back on its own subscription — MQTT 3.1.1 delivers a client's
   publishes to its own matching subscriptions. Its return proves `read` and
   `write` on `/echo` itself, whatever the SUBACK said, and with the expected
   single wildcard grant on `jdrgb/<id>/#` that stands for the subtree. It can't
   rule out a narrower deny on `/set` alone — Mosquitto allows topic-specific
   denials under a broader grant — which is what the HA-side acceptance checks
   are for. An echo that hasn't returned 10 s after it was sent is handled on the
   network thread's next event (it wakes on events, so normally within the 30 s
   keepalive): logged as "the broker's ACL appears to deny `jdrgb/<id>/#`", the
   bridge stays *not ready*, and the echo is retried every 5 minutes. *Not
   ready* is the bridge's own state; whether the broker shows `offline` depends
   on step 1's publish having been allowed. If `write` was revoked, a stale
   retained `online` from an earlier connection can stand, and the broker's
   owner clears it. The echo topic sits inside the existing `readwrite` line, so
   the ACL needs nothing extra. A lifecycle test covers an echo that never
   returns;
5. once the echo is back: discovery, `online`, the latest-state slot, and *ready*
   set. Discovery's own topic isn't covered by the echo; whether HA sees the
   entity is the acceptance check.

HA's birth message (`homeassistant/status` = `online`) republishes discovery and
the latest-state slot the same way, but only once *ready*. **No step of connecting ever writes
hardware.**

Retained messages on the command topic are ignored and logged: a command
someone left retained would otherwise replay on every reconnect. HA's own MQTT
light doesn't retain commands by default.

**Exit.** Any exit other than `--unpublish` — killed at logoff, crashed —
closes the socket, and the broker publishes the will at once. A sleeping PC's
will fires after 1.5 × keepalive, ~45 s. After that HA doesn't send commands at
all: a call to an unavailable light is skipped, with a logged warning, and the
calling script carries on (tested live on 2026.10.4). Only inside that ~45 s,
while HA still believes the light is available, can a command reach the broker,
sit on the dead connection, and apply on wake. Both are acceptable for a light.

**Topics.** `<id>` is `jdrgb_` plus the lowercased computer name.

| Topic | Use |
|---|---|
| `homeassistant/light/<id>/leds/config` | discovery, retained |
| `jdrgb/<id>/set` | commands from HA, subscribed at QoS 0 |
| `jdrgb/<id>/echo` | the bridge's own access check (*Connecting*), never retained |
| `jdrgb/<id>/state` | state to HA, retained |
| `jdrgb/<id>/availability` | `online` / `offline` (the will), retained |
| `homeassistant/status` | HA's birth message |

**Discovery**, level 1 (effect list abbreviated):

```json
{
  "name": null,
  "unique_id": "jdrgb_desk_leds",
  "schema": "json",
  "command_topic": "jdrgb/jdrgb_desk/set",
  "state_topic": "jdrgb/jdrgb_desk/state",
  "availability_topic": "jdrgb/jdrgb_desk/availability",
  "supported_color_modes": ["onoff"],
  "effect": true,
  "effect_list": ["Cool White", "Warm White", "White", "Black", "Red", "…"],
  "transition": false,
  "flash": false,
  "device": { "identifiers": ["jdrgb_desk"], "name": "Desk LEDs", "manufacturer": "jdrgb" }
}
```

`name: null` names the entity after the device. `state_topic` is what makes HA
wait for confirmed state instead of going optimistic. `transition` and `flash`
default to **true** in HA's MQTT light; left alone, HA's UI offers fades and
blinking. HA sends `transition` only when a caller passes one, and it would then
be the bridge's to implement — a fade is a stream of writes the GPU must not
see.

**Effect names.** `effect_list` is every preset's `display_name`, in table
order, *including* `Black` — a device showing black is on, and has to be
reportable. The reverse map is lowercase-and-strip-spaces, which
`a_display_name_round_trips_only_through_lowercase` (`src/palette.rs`) already
pins. An effect name not in the list is logged and dropped.

**Commands into a pending intent.** HA's JSON light sends only the fields that
changed:

```json
{"state": "OFF"}
{"state": "ON"}
{"state": "ON", "effect": "Warm White"}
```

The worker keeps **one pending intent** that remembers which fields were
present: `state`, plus optional `effect`, and in level 2 `color`, `color_temp`
and `brightness`.

- `OFF` replaces the pending intent outright.
- `ON` merges in: present fields overwrite the same fields, and a look field
  (`effect`, `color`, `color_temp`) replaces any other look field, so the most
  recent wins. Within one message, `effect` > `color` > `color_temp`.
- `ON` after a pending `OFF` replaces it. `ON` with no fields merged into a
  pending intent that already has a look leaves that look in place.
- An intent that ends up with no look and no brightness is a **plain `ON`**, and
  means `jdrgb on`. Merging never turns it into "reapply the cached preset",
  which would lose `on`'s per-device lit records and its care for the stash.

**Resolving a command.** After a 250 ms settle with no new message, the worker
first validates any pending follow (below), then resolves the pending intent
against one **base**, chosen by the same rule as what is reported (*State out*):

- while a follow is pending, **the strip** — including its lit record when it is
  off. The card is converging on the strip, so its record is stale by design;
- otherwise, the device the state table takes the look from — the strip, unless
  the strip is off or absent and the card is lit.

Otherwise a command sent while the card lags would resolve against the card's
old look: Red at 40%, then `OFF`, then `ON` with a brightness, would bring back
the card's lagging Blue rather than the Red the strip was turned off from, which
is what plain `on` restores (`src/main.rs:1253`). The result is a single
command:

| Intent | Command |
|---|---|
| `OFF` | `off` |
| plain `ON` | `on` |
| `ON` + effect P | `P` |

Level 2 adds `--level L` (Phase 4).

**The strip leg runs at once:** `jdrgb <command>`, attempted once whatever its
exit status. After each child the worker re-reads the state file and publishes
it (Stream Deck plan, trap 2: a partial failure is a real state).

**The card follows the strip.** When the strip leg finishes, the worker holds
one *follow*: the same command for the card — `jdrgb off --gpu`,
`jdrgb on --gpu`, `jdrgb P --gpu` — and two observations taken right then,
after the strip leg's child has exited: the state file's `rev`, and the stash's
identity (absent, or its `stash-id`; both 1b). A revision rather than the
recorded values, because values can't show a write that repeated them: with both
devices Blue, HA picks Red and the card's Red follow waits; the user then picks
Blue for the card from the tray. The records look exactly as the follow expects,
and only the revision shows that someone chose since. The card gets the *command*, not a copied value, so the CLI resolves
it against the card itself: a preset gets the card's calibration, and
`on --gpu` acts only if the card is recorded off, using the card's own lit
record.

A newer strip leg **replaces** the follow. There is never more than one, it
always mirrors the latest command, and nothing carries over from the one it
replaced.

One exception: a plain `ON` that **left the strip unchanged** — the strip was
already lit, so `jdrgb on` did nothing and recorded nothing — doesn't replace a
pending follow whose observations still match. Replacing it with `on --gpu` would strand the
card on its old color, since `on` acts only on a device recorded off. If the
observations no longer match, someone else has acted, and the plain `ON`
replaces the follow as usual. A plain `ON` that turned the strip back on always
replaces it.

The follow runs once the light has been quiet for 5 s and the bridge's previous
GPU leg was at least 60 s ago (pacing, below). The worker re-checks both
observations **on every tick**, not just before spawning, and once more
immediately before spawning. **Any difference means someone else acted since** —
a tray click, a terminal, a UPS hook — so the follow is dropped at once, logged,
and the ordinary projection published at once (*State out*), rather than a
minute later when pacing would have let it run. Because the observations are
taken *after* the strip leg's child, the strip leg's own write and any stash it
cleared don't count as differences.

Two windows are accepted: a write by someone else that lands *during* the
bridge's own child is attributed to the bridge, and the time between the final
check and the child holding the bus is the spawn plus up to 10 s the CLI may
wait for the bus lock.

Nothing ever corrects the hardware toward an old intent. A message that arrives
while a child runs starts the next pending intent; it doesn't cancel the child.

Worked cases, each a pure resolution test:

| Sequence | Result |
|---|---|
| Red, then (level 2) brightness 40% before the follow runs | strip Red, then Red at 40%; the follow becomes `red --gpu --level 40`. The second command resolved against the strip, already Red |
| Red; a UPS hook paints amber with `--stash`; nothing more | follow dropped on the next tick: `rev` and the stash both changed |
| Both blue; Red; then the tray sets the card Blue | follow dropped on the next tick: the values match what it observed, but `rev` moved |
| `OFF`; then the tray sets the card Green before the follow runs | follow dropped on the next tick, and HA sees `ON` with the card's Green at once instead of the strip-only `OFF` |
| Red; UPS amber `--stash`; then plain `ON` | strip `on`, a no-op; the old follow's observations no longer match, so it becomes `on --gpu`, also a no-op; the stash survives for the UPS restore |
| Both blue; Red; then plain `ON` before the follow runs | strip `on` changes nothing and the follow's observations still match, so `red --gpu` stays and runs; the card ends Red |
| Warm White picked during a UPS borrow | the strip leg clears the stash — the new normal, as from the tray; the follow observes no stash both times and runs |
| `OFF` then plain `ON`, card lit | strip off, then back on; follow `on --gpu` is a no-op, so the card is never touched |
| `OFF` then plain `ON`, card already recorded off | strip back on; follow `on --gpu` restores the card's own lit record |
| (level 2) Both Blue; Red at 40%; `OFF`; `ON` + brightness 60%, all before the follow runs | base is the strip's lit record, Red: strip Red at 60%, follow `red --gpu --level 60` — never the card's lagging Blue |
| (level 2) Both Blue at 100%; Red at 40%; then effect Green before the follow runs | Green at the strip's 40%, not the card's 100% |

**Pacing.** This is where the plan meets `docs/ups-wedge-incident.md`: two GPU
writes *seconds* apart visibly glitched the monitor, writes *minutes* apart did
not, and each GPU invocation spends ~83 I2C transactions — `prepare()`'s
signature reads included, which happen before `space_out_writes` paces anything.

- **One child at a time**, one pending intent, one follow.
- **The strip leg goes at once.** It costs USB HID writes and, for a solid or
  `off`, one flash commit — a tray click's worth.
- **The follow waits** until the light has been quiet for 5 s *and* the bridge's
  own previous GPU leg was at least 60 s ago. As a courtesy it also waits while
  the CLI's machine-wide marker (`%ProgramData%\jdrgb\gpu-last-write`,
  `src/gpu.rs:211`) shows a GPU write by anyone in the last 60 s; a missing or
  malformed marker is ignored. That courtesy is best-effort: another caller can
  write between the check and the child taking the bus. Between callers the
  floor is the CLI's own gap — 1500 ms by default, itself best-effort (skipped
  when the marker is missing or unreadable, or `JDRGB_GPU_GAP_MS=0`) — which this
  plan doesn't change.
- **A narrow repeat filter on the strip.** A strip leg identical to the bridge's
  own previous one is skipped if that ran under 10 s ago, the record still
  matches it, and no stash exists — for a HomeKit wheel dragged inside one
  preset's region. A skipped strip leg still produces a follow. Everything else
  reaches the CLI, like a tray click: a record matching a command is not proof
  the hardware does (trap 4), and the same color picked during a UPS borrow must
  make it the new normal, as from the tray.
- **A runaway guard on the strip.** A token bucket of 30 strip legs, refilling
  one a minute, counted on *attempt*, whatever the outcome. An empty bucket drops
  the strip leg, logs it and publishes the truth. Two things still happen:
  - an `off` strip leg gets through at most once every 5 minutes, so the light
    can always be turned off. That allowance counts attempts too, never records:
    a record can fail to land (1b keeps recording best-effort) while the write
    succeeds, and an exemption keyed on "strip not recorded off" would then let
    every repeated `OFF` commit flash;
  - the **follow is still created**, with its observations taken at the moment
    the strip leg was dropped. The card has its own pacing, at most once a minute, so
    an `OFF` reaches a lit card even when the strip's bucket is empty or the strip
    was already off.

  The bucket lives in memory and starts full on every launch. It guards against
  a looping automation; it is not a wear budget, since the controller's flash
  endurance is unknown.

In practice: the dimmer's off button turns the strip off at once and the card
off five seconds later. Twenty presses of brightness-up step the strip twenty
times and the card once.

**State out.** One entity over two devices, projected from the state file:

| Strip | GPU | Reported |
|---|---|---|
| absent | absent | `"state": null` (unknown) |
| off or absent | off or absent (not both absent) | `OFF` |
| lit | anything | `ON`, look from the strip |
| off or absent | lit | `ON`, look from the GPU |

A *look* is:

- a solid that recognises as a preset → `effect` = its display name;
- a solid that doesn't (a hand-tuned hex), or a per-LED frame →
  `"effect": null`. **Null, not omitted**: HA's state handler only touches
  `effect` when the key is present (`schema_json.py`, `with suppress(KeyError)`),
  so omitting it leaves the last preset showing.

When both are lit with different looks the strip's is reported, documented as
approximate.

**While a follow is pending, the strip alone is reported** — its on/off state as
well as its look. The difference is the bridge's own pacing and the card is
converging on the strip, so the strip is what the user asked for. Without this,
the dimmer's toggle would break: after an `OFF` the card stays lit for at least
5 s, the table would say `ON`, and a second press — `light.toggle` decides from
the reported state — would send another `OFF` instead of turning the light back
on.

The projection is for display only; it never decides whether a leg is done.

Level 1 state messages:

```json
{"state": "OFF"}
{"state": "ON", "color_mode": "onoff", "effect": "Warm White"}
{"state": "ON", "color_mode": "onoff", "effect": null}
{"state": null}
```

**Install.** `install.ps1 -Ha` copies `jdrgb-ha.exe`, registers a per-user logon
task `jdrgb-ha` modelled on the tray's (`install.ps1:297-313`), and starts it.
Exclusion with the boot task holds in both directions:

- `-Ha -Boot` together is an error.
- `-Ha` with a boot task installed removes it, as running without `-Boot`
  already does — and if the task is running, stops it and waits (bounded) until
  it isn't, before starting the bridge.
- `-Boot` with a `jdrgb-ha` task installed is an error naming the way out.

**Acceptance, in this house**, once the broker's ACL entry exists:

1. `Test-NetConnection <host> -Port 1884` succeeds from the PC (done 2026-10-29).
2. `ha.log` shows the echo coming back and availability going `online`.
3. The entity appears in HA with no YAML, named after the device:
   `light.<computer>_leds` by default. That is the check on discovery's `write`.
4. Its state follows the tray: pick a color from the tray, and HA shows it within
   about a second — the check on state's `write`.
5. Picking a named preset in HA changes the strip, and HA then shows that preset
   as confirmed state — the check on the command path, HA to bridge, which the
   echo exercises but doesn't prove for HA's own publishes.
6. Unplugging the network, or sleeping the PC, turns it `unavailable` within
   about 45 s; it comes back on its own.
7. **HomeKit:** the HA owner adds the light to the HomeKit Bridge's include list,
   and it appears in Apple Home.

**Remove.** `uninstall.ps1`, in order:

1. Disables and stops the `jdrgb-ha` task and ends any `jdrgb-ha` process — its
   children end with it, through the job object — then waits (bounded) until
   `Global\jdrgb-ha` is free and no `jdrgb.exe` from the install directory is
   running.
2. Runs `jdrgb-ha.exe --unpublish --config <that user's ha.conf>`. It takes the
   singleton itself, so a resident can't start mid-cleanup; connects with the
   client id `jdrgb-ha-<name>-cleanup`; publishes **nothing** of its normal startup;
   publishes empty retained payloads to the config, state and availability
   topics at QoS 1 and waits for their three PUBACKs; queues DISCONNECT, polls
   until the `Outgoing::Disconnect` event confirms it was sent, then drops the
   connection — the client closes, per MQTT, rather than waiting for the broker
   to. One 15 s deadline covers the whole operation; missing it, or any failed
   step, exits non-zero.
3. Removes the task and binary, reporting a failed unpublish with the manual
   fallback (delete the device under HA's MQTT integration).

`ha.conf` is left in place and its path printed, since it may hold a password on
an install that uses one. Acceptance check, in HA: after uninstall the entity is
gone and stays gone across an HA restart.

### Phase 4 — level 2: color wheel, white temperature, brightness

**Discovery** changes to `supported_color_modes: ["hs", "color_temp"]`,
`color_temp_kelvin: true`, `min_kelvin: 2700`, `max_kelvin: 6500`, effects
kept. `onoff` can't be combined with other modes, so it goes. HA treats `hs`
and `color_temp` lights as brightness-capable whatever the `brightness` flag
says (`brightness_supported`), so HA will send `brightness`.

**HS → preset.** HA's `color` is `{h: 0–360, s: 0–100}`. Each candidate's
`SWATCH` appearance is converted with **HSV** — not the HSL that `tune` uses
(`rgb_to_hsl`, `src/main.rs:1528`): `coolwhite`'s swatch is ~3.9% saturated in
HSV and 100% in HSL. Request and candidates go onto the unit disc as
`(s·cos h, s·sin h)`, and the nearest wins by Euclidean distance, ties to table
order. Candidates: every preset except `black` (not a color) and `white` (an
uncalibrated reference that reads green). The disc, not hue alone, is what makes
the whites and `pink` reachable from the wheel.

**Kelvin → preset.** Two stops: `warmwhite` = 2700 K (its `SWATCH` comment
already says so) and `coolwhite` = 6500 K. Nearest in mireds; the midpoint is
~3815 K, ties to `warmwhite`. Labels chosen to match how they look, not
measurements. More whites would make the slider less binary; each is a preset
tuned by eye on both devices.

**Brightness ↔ level.** HA `b` in 1–255 → `level = MIN_LEVEL + round((b−1)·(100−MIN_LEVEL)/254)`.
Reported back: `b = 1 + round((level−MIN_LEVEL)·254/(100−MIN_LEVEL))`, and `1`
for any level below `MIN_LEVEL` (a hand-set `--level 2` is still recognised,
just reported at the bottom of the slider).

**Recognising (preset, level)** — a pure function in `palette.rs`, so the tray
can later show "Warm White · 40%" from the same code. For one device's recorded
wire value: every preset, every level 1–100, keep those that `--level` would
have turned into exactly this value; highest level wins, then table order.
`black` recognises at level 100. Two devices are "the same look" when they
recognise as the same (preset, level).

**Resolving brightness**, against the base as in Phase 3:

- `ON` + brightness, base lit → the base's recognised preset at the new level.
  If the base doesn't recognise: a hex snaps to the nearest preset first; a
  frame ignores the brightness, logged, and publishes the truth. Re-scaling a
  hex directly would compound rounding and never recognise again, so the slider
  would jump back to 100% each time.
- `ON` + brightness, base off → the lit record's preset (`mb.lit`/`gpu.lit`)
  at the new level. An unrecognisable lit record falls back to plain `on`,
  ignoring the brightness.
- `ON` + look, no brightness → the look at the base's recognised level — the
  lit record's if the base is off — or 100 if neither recognises.

The strip leg becomes `jdrgb P --level L` and its follow `jdrgb P --gpu --level L`.
The repeat filter compares wire values, never brightness numbers, so rounding
can't cause a spawn.

**State out** adds `color_mode`, `brightness`, and the preset's appearance:
`color_temp` for `coolwhite` and `warmwhite`, always — deterministic, because
the file can't say which control chose them — and `color: {h, s}` for everything
else. An unrecognised hex reports its wire value as HSV at brightness 255; a
frame reports its first LED's wire value the same way. Both are approximate by
construction: a hand-tuned hex has no recorded appearance (README, *The third
table*).

```json
{"state": "ON", "color_mode": "color_temp", "color_temp": 2700, "brightness": 255, "effect": "Warm White"}
{"state": "ON", "color_mode": "hs", "color": {"h": 30.0, "s": 100.0}, "brightness": 102, "effect": "Orange"}
{"state": "ON", "color_mode": "hs", "color": {"h": 211.8, "s": 40.0}, "brightness": 255, "effect": null}
```

Tests, all pure:

- exhaustive (device, preset, level) → wire → recognise: the recognised pair
  produces the same wire value, and from the device's floor up it is the same
  preset (`MIN_LEVEL` on the strip, 10 on the card);
- every *eligible* candidate's own appearance snaps to itself (`black` and
  `white` excluded, since they are not candidates);
- kelvin snapping at and either side of the midpoint;
- brightness → level → brightness across 1–255 stays within the mapping's actual
  quantisation bound — half a level's width plus half a step, computed from
  `MIN_LEVEL` — rather than a fixed "one step", which doesn't hold for every
  floor (at `MIN_LEVEL` 20, `9 → 23 → 11`).

### Phase 5 — the Hue dimmer (documentation, not code)

The dimmer stays paired to the Hue Bridge, so the room lights keep their
bridge-native response. HA's Hue integration fires `hue_event` and offers device
triggers for each button; one automation maps them onto the light:

- on/off → `light.toggle` (or `turn_on` / `turn_off` on the four-button model)
- brightness up/down → `light.turn_on` with `brightness_step_pct`
- long press → cycle `effect` through a shortlist

HA's Hue documentation states the Hue API limits each device to one event per
second. That suits this light, and the GPU pacing makes faster pointless.

Acceptance check for the toggle: press off, then on again within five seconds.
The strip goes off and comes back, and the card is never touched — the second
press saw `OFF`, because the pending follow made the strip's state the
reported one.

Acceptance check for dimming: five brightness-up presses about a second apart
step the strip five times. HA doesn't send `brightness_step_pct` as a step: it
computes an absolute brightness from the last *reported* state, so a press that
arrives before the previous strip leg's state is published is computed from the
stale value, and two presses collapse into one. At one Hue event per second and
a 250 ms settle it should hold; if presses collapse, the fix is publishing state
sooner, not changing HA.

### Scenes (documentation and acceptance, not code)

The house's Studio scenes — `script.studio_off`, and Relax / Bright / Synthwave
Dreams driven by `input_select.studio_mode` — snapshot and restore with
`scene.create`. A known quirk: a restore skips any entity whose last-reported
state already matches. The light needs nothing special, but these behaviours are
worth pinning down:

- **A scene sees the true requested state at once.** The strip changes within
  about a second, and while a follow is pending the bridge reports the strip
  (Phase 3, *State out*), so a snapshot or restore never waits on the card. The
  card visibly arrives 5–60 s after the strip in a scene change. That is the
  pacing working, not a bug, and the user-facing docs say so.
- **A restore lands on the same preset.** Tested live on the level-1 payload: a
  snapshot taken at Warm White restores as `{"state":"ON","effect":"Warm White"}`
  — effect only, whether over Red or over `OFF` — and sends nothing over a
  matching Warm White (the known skip). So a snapshot comes back as exactly that
  preset. At level 2, check whether `color` or `brightness` ride along; within one
  command `effect` wins (Phase 3), so the result is the same either way. A
  hand-tuned hex reports `effect: null` and comes back snapped.
- **Wiring.** The Studio's Off snapshots a fixed list of entities and its On
  restores that list with `scene.turn_on`: the light joins that list, and Off → On
  is then one strip leg and one follow. Relax, Bright and Dreams don't restore;
  they call `light.turn_on` directly, so each names an effect:
  - Relax is burnt orange (rgb 255,144,55) → effect `Burnt Orange`;
  - Bright is 4255 K, on the cool side of the 3815 K split → `Cool White`;
  - Dreams sets the Galaxy to hs 323/100. At level 1 there is no color wheel, and
    even at level 2 snapping would choose for it, so Dreams names the effect
    directly. **Starting value `Hot Pink`**, not a fixed choice: its strip value
    `FF0080` sits at hue ~330 against Cerise's ~315, a hair nearer 323, and
    `palette.rs` already calls it "intense synthwave/Barbie pink". JD adjusts by
    eye once it's live.
- **Test: Off → Relax inside 5 s.** Run `script.studio_off`, then Relax within
  five seconds. The strip goes off, then Burnt Orange; the card's `off` never
  runs. The follow collapses to a single card command — Relax's,
  `burntorange --gpu` — so the card is written once.
- **The PC asleep.** Tested live with a probe entity: a script step that targets
  an unavailable MQTT light is skipped — nothing is published — HA logs one
  warning ("Referenced entities … are missing or not currently available"), and
  the script continues and returns normally. A sleeping PC doesn't break the
  Studio scripts. Repeat once with the real light as an acceptance check.

## Traps

1. **Never forward HA's RGB as hex.** It would look right on the strip and
   wrong on the card.
2. **The card lags, on purpose.** At least five seconds behind the strip, and up
   to a minute after recent GPU activity. That is the display-safety rule.
3. **A busy bus fails after 10 s.** `BusLock::acquire` waits `LOCK_TIMEOUT_MS`
   (`src/gpu.rs:197`) and fails as transient. During a `tune` or `preview`
   session the bridge's GPU legs fail; they retire and the truth is published.
   No retry loop.
4. **The record is not the hardware.** A color set by Armoury Crate or OpenRGB is
   invisible, and after a power cycle the card shows its *flash* color — the last
   `jdrgb --gpu save` — while its record shows the last color set. HA reports the
   record. An explicit preset or `off` repairs it, since commands aren't
   suppressed against the record. A plain `on` doesn't: it acts only on a device
   recorded off (`on_state`, `src/main.rs:1249`). `jdrgb --gpu save` after
   choosing a normal color keeps the gap small.
5. **`--wait` is not 60 s.** The tray's comment calls it that; it is 120 attempts
   of 500 ms *plus* each attempt's own work, which for the GPU includes up to 10 s
   for the bus lock. The 90 s `CHILD_TIMEOUT` is the parent's own ceiling. The
   bridge doesn't pass `--wait`.

## Known issues, inherited

- **The state file is per-user** (`src/palette.rs:324`), and the bridge runs as
  its one configured user. It agrees with that user's tray and terminal.
- **No strip readback.** The strip's color can only be known by recording it.

## Non-goals

- Continuous color. Snapping is the honest model for these tables.
- Transitions, flashing, animated effects.
- **Deferred flash commits (`--no-save`) and a startup re-apply.** Both were in
  the first draft. Both need a maintenance path through the CLI that preserves
  the stash and lit records and acts only on an expected state — an ordinary
  apply clears the UPS stash (`src/main.rs:164`) — plus persisted commit
  bookkeeping. Committing like a tray click is simpler, and bounded by the pacing
  above.
- **Suppressing commands that match the record.** Considered and rejected: see
  the repeat filter, and trap 4.
- **A hard cross-caller GPU interval** enforced inside the CLI. The bridge paces
  its own GPU writes; the CLI's 1.5 s floor still governs between callers.
- TLS, Windows credential storage, a custom HA integration in Python.
- Two entities, multi-user arbitration, a listener of any kind on the PC.
- **Matter**, for this house: there is no Matter server, and HomeKit already
  covers Apple Home. An install that wants it would need a community bridge
  (Appendix A); nothing in the bridge would change.

## Open questions, to settle by eye or by test

1. **`MIN_LEVEL`, and linear vs perceptual brightness.** Tune on both devices at
   once with `--level`, the same way the presets were tuned; the collision test
   sets its lower bound. *`MIN_LEVEL` settled at 3 (Phase 2). Linear vs
   perceptual waits on the slider in use.*
2. **The bridge's 60 s GPU interval and 5 s settle.** Guesses between "seconds
   apart glitched" and "minutes apart didn't", like the CLI's own 1500 ms. Lower
   them only on evidence.
3. **The HomeKit wheel with a snapped echo.** Verify Apple Home settles on the
   reported color rather than fighting it. If it fights, `mode=presets` in
   `ha.conf` (below) is the fallback.

## Implementation notes

Built on the `home-assistant` branch, 2026-10-29, all without touching hardware:
every CLI run had `JDRGB_NO_HW=1`, and nothing was installed. Where the build
differs from, or adds to, the plan above:

- **Levels 1 and 2 shipped together.** The light is the full one — wheel, white
  temperature, brightness, effects — and `mode=presets` in `ha.conf` drops back
  to the level-1 payload (`onoff`, effects only). Added for open question 3: if
  Apple Home fights the snapped echo, that is a one-line way out without a
  rebuild.
- **`on` looks twice.** The plan has `on` decide under each device's lock; it
  also first checks the record unlocked, so that with nothing off it opens no
  device — opening the strip sends a config request, which a redundant press
  shouldn't cost. A device that looks off is re-decided under its lock.
- **The bridge skips spawning a no-op.** A plain `ON` with the strip already
  lit, and an `on --gpu` follow for a card that isn't off, are decided in the
  bridge rather than by spawning a `jdrgb on` that would open nothing. Same
  outcome, one process fewer.
- **A record that can't be written** is reported by `jdrgb.exe` at exit
  (`palette::record_failed`), not printed from `palette.rs`, which stays
  print-free. The bridge logs a successful child's stderr as a warning.
- **The state lock** is std's `File::try_lock` on `state.lock` — `LockFileEx`
  underneath — in a bounded retry loop, rather than raw `LockFileEx`.
- **Brightness 0** from HA is treated as 1, the bottom of the slider; HA sends
  `OFF` for off.
- **Sizes,** release: `jdrgb.exe` 260,608 bytes and `jdrgb-tray.exe` 249,856,
  both identical before and after `rumqttc` was added; `jdrgb-ha.exe` 462,848.
  After the implementation review's fixes: 261,120 (the stash-locking change in
  `main.rs`), 249,856 unchanged, and 464,384.
- **Topic ids keep hyphens.** The plan's `<name>` is "the lowercased computer
  name"; turning `-` into `_` put a hyphenated PC outside its own documented ACL.
  The installer prints the exact ACL lines and the bridge logs them at start.
- **A SUBACK timeout** (30 s, then subscribe again) and clearing `rumqttc`'s
  buffered events on every disconnect, neither in the plan: `MqttState::clean`
  leaves events queued for the dead connection, and a stale `Outgoing::Subscribe`
  could otherwise be taken for the new connection's and strand it.
- **Tests** (all hardware-free): the strip lock, the state file under threads
  and torn reads, stash identity, `--level` and the exhaustive `MIN_LEVEL`
  collision check, recognition across every device/preset/level, wheel and
  kelvin snapping, the model's worked cases on a fake clock, the lifecycle stage
  machine with forced queue refusals, the slot's publish ordering under two
  threads, the job object ending a child, and the real `rumqttc` client against
  a fake broker — a healthy one, one that grants but never delivers, one that
  refuses, one that drops the connection mid-subscribe, and `--unpublish`.
- **Under Task Scheduler**, the bridge was run once from a temporary per-user
  task (closed-port config, removed straight after) to confirm it can join its
  kill-on-close job inside the scheduler's own: it can.

**Implementation review** (Codex, gpt-6-astra, xhigh, on e2999b7) — `FIX FIRST`,
9 findings, every one checked against the code and adopted:

| # | Finding | Fix |
|---|---|---|
| 1 | Installers carried on past their shutdown deadlines | Timeouts abort; uninstall also waits for the bridge's singleton to be released |
| 2 | Stash removals went ahead when the state lock wasn't had | `clear_stash` / `remove_stash_if` in `palette.rs`, which refuse without it; contention test |
| 3 | `write_atomic` fell back to writing in place | It fails and leaves the original; test with a reader that denies delete |
| 4 | The installer replaced `jdrgb.exe` before stopping its callers | Boot task disabled and waited out, bridge ended, no `jdrgb.exe` left running — then copy; a failure puts back what it stopped |
| 5 | Stale `rumqttc` events after a reconnect could strand the subscription | Events cleared on disconnect; SUBACK timeout; reconnect-mid-subscribe test |
| 6 | Log trimming could slice inside a UTF-8 character and abort the bridge | Trimmed as bytes at a newline, serialised with appends; em-dash and bad-UTF-8 tests |
| 7 | The repeat filter could swallow a retry after a failed apply | Judged by the record's wire value, not the last command's arguments; tests for both |
| 8 | A brightness while off snapped a saved non-preset color | Plain `on`, as the plan says; test |
| 9 | Hyphenated computer names fell outside the documented ACL | Hyphens kept; exact ACL printed and logged; test |

**Implementation review, round 2** (on 0a2e2d9) — `FIX FIRST`; seven of the nine
fixes confirmed, two problems found in the other two, both adopted:

| # | Finding | Fix |
|---|---|---|
| 1 | The tray could still start a `jdrgb.exe` mid-install or mid-uninstall | Both installers stop it with the other callers, first; uninstall disables every task up front and re-enables them if it times out |
| 2 | The repeat filter judged against the record alone, so a partial failure could make a correction look like a repeat | A repeat must also match what the previous *successful* strip leg wrote; `on_done` takes the child's outcome; test |

Not adopted: mocked PowerShell tests for the installers — the repo has no
PowerShell test harness; both scripts are parse-checked, their pre-elevation
paths exercised, and the uninstaller's singleton probe checked against a live
bridge.

**Implementation review, round 3** (on f01688c, which adds only a guard for a
process that exits before it is killed) — `VERDICT: SHIP`, no findings.

**On the real install** (2026-10-30). Phases 1 and 2 were checked by hand on
both devices. `off --all` then `on --all` brought back the prior amber, and
warm white was stepped through 40, 10, 7, 5, 3 and 1. It looked right
throughout, so `MIN_LEVEL` is 3 (see Phase 2). The bridge is installed: it
connected, got its echo back and published. Home Assistant shows the light on,
as Amber, with the expected modes, effects and device, and neither HA nor
Mosquitto logged warnings or denials.

Still to do, in order: the rest of Phase 3 acceptance (*Acceptance, in this
house*), then the Hue dimmer and the scenes.

## Appendix A — verified findings

Checked on this machine on 2026-10-29:

- **Discovery writes to the device:** `open_controller` calls `read_config`,
  which sends a request report (`src/main.rs:523`, `request` at `:556`).
- **The state file write is unlocked and truncating:** `update`,
  `src/palette.rs:597-604`. **The GPU records after its lock is released:**
  `apply_gpu_colors` (`src/main.rs:456`) returns before `record(true, …)` at
  `src/main.rs:403`. **`turn_on` reads state before opening any device**
  (`src/main.rs:1269`). **`keeping_lit` writes both snapshotted lit records
  back over freshly read state** (`src/palette.rs:630`; `update` re-reads, and
  `keep_lit_from` replaces only `mb_lit` and `gpu_lit`). **`set_solid` commits
  before it records** (`src/main.rs:589-590`). **Every stash has the same fixed
  header** (`stash_now`, `src/main.rs`), so identical lighting stashes to
  identical bytes, and **a `--stash` with a stash present returns without
  touching it** (`src/main.rs:1161`). **`record` rewrites the state file even
  when nothing changed** (`update`, `src/palette.rs:604`), which is what makes a
  revision counter meaningful.
- **A bare apply clears the stash:** `src/main.rs:160-170`; `on` and `restore`
  are excluded by `is_apply`.
- **Only the strip's solid path commits flash:** `CTRL_COMMIT` is sent only from
  `apply_solid_stepped` (`src/main.rs:637`); `set_solid` always passes `true`
  (`src/main.rs:589`). Direct-mode frames never commit; the GPU commits only via
  `save_gpu_flash`.
- **GPU pacing:** `DEFAULT_WRITE_GAP` 1500 ms (`src/gpu.rs:182`);
  `space_out_writes` sleeps the remainder (`src/gpu.rs:670`) and runs after
  `prepare()`'s signature reads; the marker is `%ProgramData%\jdrgb\gpu-last-write`,
  written best-effort (`src/gpu.rs:211`, `:229`). The bus mutex waits 10 s, then
  fails as transient.
- **The tray already distinguishes Unknown and Mixed** (`src/tray/main.rs:160-190`),
  the precedent for the state table.
- **The installer carries the tray user's SID through elevation**
  (`install.ps1:140`, `:292`); **uninstall knows only the boot and tray tasks**
  (`uninstall.ps1:24`, `:40`).
- **The state parser skips unknown keys**, which is what let `*.pattern` and
  `*.lit` be added without touching the tray.
- **29 presets, 12 `SWATCH` overrides**, 24 GPU overrides (`src/palette.rs`).
  At level 9, Cerise (`FF001B`) and Hot Pink (`FF0011`) both scale to
  `(23,0,2)` on the GPU; at 10 they differ.
- **HA's MQTT JSON light** (docs and `schema_json.py` at 2026.10.0): `transition`
  and `flash` default to `true`; `transition` is sent only when a caller passes
  one; `onoff`/`brightness` must be the only color mode when used;
  `color_temp_kelvin`, `min_kelvin`, `max_kelvin` exist; `unique_id` is required
  for a `device` block; `effect` updates only when the key is present;
  `"state": null` means unknown; brightness is handled for any brightness-capable
  color mode.
- **`rumqttc` 0.25.1** (docs.rs source): `Client::publish`, `subscribe` and
  `disconnect` are blocking sends on the bounded request channel; `try_*`
  variants exist; `handle_incoming_suback` ignores the SUBACK's return codes.
  `Connection::recv_timeout` creates a new `eventloop.poll()` future per call
  and drops it on timeout, while `EventLoop::poll` assigns `self.network` only
  after TCP connect and CONNACK both complete — so a short `recv_timeout` aborts
  any slower handshake. `recv` and `iter` block on the poll with no timeout; the
  handshake is bounded by `network_options.connection_timeout()`. Default
  features can be turned off for a TLS-free build.
  *From review, not independently checked:* the event loop reconnects on the next
  poll with no delay of its own. The plan's 1 s wait is harmless either way.
- **`restore` deletes the stash unconditionally** once its legs land
  (`src/main.rs:1196-1205`). **The tray's child timeout depends on the parent
  living** to call `kill()` (`src/tray/worker.rs:99-102`), and Windows does not
  end a killed process's children.
- **Hue:** HA's Hue docs state one event per second per device and describe
  `hue_event` and device triggers.
- **Matter** (out of scope for this house): HA's own Matter integration is a
  controller only. The original Home Assistant Matter Hub ended maintenance in
  January 2026 and was archived on 2026-03-11; a community fork and
  Matterbridge's HA plugin remain.
- **The broker, from the PC** (2026-10-29, single TCP connects, no MQTT): the
  Mac mini's own addresses refused both 1884 and 1883; the HA VM's own LAN
  address, supplied by the broker's owner, accepted 1884.
- **Live checks by the HA owner** (HA 2026.10.4, Mosquitto 2.0.22, a throwaway
  probe entity using this plan's level-1 payload, cleaned up afterwards):
  discovery renders with no warnings — `supported_color_modes` `[onoff]`,
  `supported_features` 4 (effect), 29 effects, entity named after the device;
  `"effect": null` clears the attribute and `"state": null` shows as unknown;
  HA sends `{"state":"OFF"}`, `{"state":"ON"}` and
  `{"state":"ON","effect":"Cerise"}`; discovery prefix and birth message are the
  defaults; HA's own client has full access on its own listener; **Mosquitto
  grants an ACL-denied subscription with SUBACK `0`** and enforces read at
  delivery; a script step on an unavailable light is skipped with a warning and
  the script continues; a `scene.create` restore sends effect only.
- **Second live pass, on the implementation** (e2999b7; same install, a throwaway
  probe using `payload::discovery()` exactly, cleaned up afterwards): the wheel
  payload renders with no warnings — `supported_color_modes` `[color_temp, hs]`,
  2700–6500 K, features 4; Warm White lands as `color_temp` 2700 and Orange as
  hs (30, 100). With `color_temp_kelvin` true HA sends `color_temp` in kelvin.
  A wheel-mode `scene.create` restore sends color and brightness alongside
  `effect` — `{"state":"ON","color":{"h":30.0,"s":100.0},"brightness":102,"effect":"Orange"}`
  — and `resolve` takes the effect and `level_for(brightness)`, so it lands on
  the exact preset and level; `level_for(brightness_for(L)) == L` for every level
  from `MIN_LEVEL`, so repeated off/on can't drift. `brightness_step_pct` arrives
  as an absolute brightness computed from the last reported state (Phase 5,
  dimming check). The broker's ACL is installed: an echo on the bridge's topic
  comes back, one on another machine's doesn't.

## Appendix B — review log

Reviews by Codex (gpt-6-astra, xhigh). Each finding was checked against the
source before being acted on.

**Round 1** — 16 findings.

| # | Finding | Disposition |
|---|---|---|
| 1 | HID lock scope must cover discovery; fail closed; boot task conflict | Adopted (1a) |
| 2 | State file lost updates, torn reads, record outside device lock | Adopted (1b) |
| 3 | Reconciler could loop and fight later external changes | Adopted: one attempt per leg, no corrective writes |
| 4 | Commit/re-apply would clear the UPS stash | Adopted the simpler option: `--no-save`, deferred commits and startup re-apply removed |
| 5 | Coalescing doesn't bound sustained GPU traffic | Adopted, stricter from the incident record (revised in round 2) |
| 6 | Startup snapshot lacked `--all` | Moot: startup re-apply removed |
| 7 | Commit-policy bounds inconsistent | Moot: commit policy removed |
| 8 | Hand-written MQTT under-specified | Adopted: `rumqttc` |
| 9 | Retained commands, will vs DISCONNECT, reconnect replays | Adopted, except suspend-time command discard: with clean sessions and QoS 0 the exposure is the ~45 s will window, documented |
| 10 | Discovery incomplete; omitted `effect` doesn't clear | Adopted, verified in `schema_json.py` |
| 11 | State projection invented ON | Adopted: state table; unknown is `null` |
| 12 | `{on, look}` lost plain-ON and field presence | Adopted |
| 13 | `Local\` singleton is per session | Adopted: `Global\`, explicit client id |
| 14 | Recognition range, HSV vs HSL | Adopted |
| 15 | Uninstall, retained discovery removal, broker credentials | Adopted (revised in round 2) |
| 16 | Wording corrections | Adopted |

**Round 2** — 13 findings.

| # | Finding | Disposition |
|---|---|---|
| 1 | "60 s after any GPU write" is unenforceable from outside the CLI | Adopted by narrowing the promise: the bridge paces its own legs; the marker check is a stated courtesy; the CLI's 1.5 s floor is the cross-caller guarantee. A CLI-side check was not added (non-goals) |
| 2 | Boot-task exclusion only enforced in one install order | Adopted: both directions, stop-and-wait for a running boot task |
| 3 | A delayed GPU leg can overwrite a later UPS/tray change and clear its stash | Adopted: waiting legs carry a precondition on the GPU record and stash, checked before spawning |
| 4 | `on`, `restore`/`keeping_lit` and the stash decide outside their transactions | Adopted (1b): decisions under the device lock; keep-lit record mode replaces `keeping_lit`'s snapshot; stash under the state lock. Interleaving tests across devices not adopted: no device seam, guaranteed by lock scope |
| 5 | Blocking `rumqttc` calls on the iterator thread can deadlock | Adopted, verified in rumqttc 0.25.1: three threads, network thread never calls `Client` |
| 6 | Partial commands have no defined base while the GPU lags | Adopted: per-device base = unfinished leg, else record; explicit cases |
| 7 | `--unpublish` can't run beside the resident | Adopted: stop, wait for the singleton, dedicated cleanup connection with PUBACK waits |
| 8 | `rumqttc` doesn't validate SUBACK | Adopted, verified: `online` only after the command subscription is granted |
| 9 | Duplicate suppression skips the stash-clearing side effect | Adopted by removing suppression against the record; only the narrow repeat filter remains, and never with a stash present |
| 10 | Suppression blocks repairing the post-power-loss mismatch | Adopted, same change |
| 11 | Runaway guard semantics | Adopted: token bucket, `off` always passes, in-memory by design |
| 12 | "Every bridge write commits" overstated | Adopted |
| 13 | Brightness inverse bound; `MIN_LEVEL` collisions; snap test scope | Adopted, collision example verified by hand |

**Round 3** — 9 findings. Four of the six majors (and round 2's #6 before them)
came from one mechanism — a waiting GPU leg with its own base and precondition —
so the mechanism was replaced rather than patched: **the card follows the
strip**, replaying the latest strip command, re-observed before it runs.

| # | Finding | Disposition |
|---|---|---|
| 1 | A newer partial command could revive an invalidated GPU target | Resolved by the follow model: nothing is inherited; the follow mirrors the latest strip command |
| 2 | `restore` reads, applies and deletes the stash without identity checks | Adopted (1b): identity checked before each leg; conditional delete |
| 3 | A generation's own strip leg clears the stash and drops its GPU leg | Resolved: observations are taken after the strip leg |
| 4 | Cancelling a waiting `off` leaves an already-off card off after `ON` | Resolved: the follow is `on --gpu`, which the CLI resolves against the card's real record |
| 5 | A killed bridge orphans its `jdrgb.exe` child | Adopted: kill-on-close job object; uninstall waits for children |
| 6 | Cross-thread lifecycle lets a stale `online` reach a new connection | Adopted Codex's simpler option: one network thread with `recv_timeout` and `try_*`; publisher thread removed; connection-scoped state |
| 7 | No reconnect delay | Adopted: 1 s, de-duplicated logging |
| 8 | `--unpublish` waited for the broker to close | Adopted: client closes after `Outgoing::Disconnect`; one overall deadline |
| 9 | Race window, 1.5 s gap and plain-`on` repair overstated | Adopted: wording corrected |

**Round 4** — 8 findings, each checked against the code or the pinned
`rumqttc` source first.

| # | Finding | Disposition |
|---|---|---|
| 1 | The always-allowed `off` keyed on the record allows unbounded commits if recording fails; an empty bucket could drop an `OFF` a lit card needs | Adopted: `off` allowance of one per 5 min counted on attempt; a dropped strip leg still creates the follow |
| 2 | Contents can't identify a stash: identical lighting, identical bytes | Adopted: `stash-id` comment, used by restore checks, conditional delete and follow observations |
| 3 | `recv_timeout(100 ms)` cancels every handshake slower than 100 ms | Adopted, verified in 0.25.1 source: network thread back on blocking `iter()`; worker publishes state only, non-blocking, while *ready* |
| 4 | A redundant plain `ON` replaces a colour follow with a no-op `on --gpu` | Adopted: a plain `ON` that changed nothing keeps a still-valid follow; new worked case |
| 5 | A stale retained `online` survives a refused subscription | Adopted: retained `offline` first on every connection |
| 6 | `light.toggle` reverses wrongly while the card's `off` is pending | Adopted by changing the projection rather than the automation: while a follow is pending, the strip alone is reported; two-press acceptance check |
| 7 | Job handle lifetime; `--unpublish` could be ended by its own job | Adopted: non-inheritable handle held for the process lifetime; setup failure exits; `--unpublish` skips the job |
| 8 | Appendix A overstated `keeping_lit` | Adopted: wording corrected (the body was already right) |

**Round 5** — 3 findings.

| # | Finding | Disposition |
|---|---|---|
| 1 | A reused stash keeps its id, so a paused restore can finish over a newer borrow | Adopted: every `--stash` re-stamps the id, keeping the first snapshot's colors |
| 2 | Value observations miss a write that repeats the same values; invalidation only before spawning delays the honest projection | Adopted: follow observes a `rev` counter (not mtime, for clock granularity) and the stash id, re-checked every tick |
| 3 | Reconnect and HA birth published a projection of the raw file, undoing the pending-follow rule | Adopted: the worker maintains a latest-state slot; the network thread publishes only from it |

**Round 6** — 3 findings.

| # | Finding | Disposition |
|---|---|---|
| 1 | Two publishers of the slot can enqueue an older projection after a newer one | Adopted: one mutex held across choosing the payload and `try_publish`; interleaving test |
| 2 | A lifecycle `try_*` refused by a full queue is lost, leaving the connection unready | Adopted, verified in 0.25.1: a stage machine that advances only on acceptance and retries on the next event; queue of 32; *ready* only after all startup messages are accepted |
| 3 | The tick was gated on `rev`, but a stash can change without one | Adopted: every tick checks the stash identity; `rev` only gates recomputing the projection |

**Round 8** — `VERDICT: READY`, with 3 minor findings, all adopted: the shared
spawn result keeps a successful child's stderr so a skipped record reaches
`ha.log`; the echo's guarantee is stated as `/echo` plus the expected wildcard
grant, with *not ready* distinguished from a broker-side `offline` that a revoked
`write` can't publish; and the echo timeout is handled on the next network
event, normally within the 30 s keepalive.

**Round 7** — 3 findings.

| # | Finding | Disposition |
|---|---|---|
| 1 | Command resolution ignored the pending-follow override, so a command while the card lagged could resolve against the card's stale look | Adopted: while a follow is pending the base is the strip, including its lit record; two worked cases |
| 2 | The anonymous ACL doesn't isolate one anonymous client from another | Adopted: wording now says anonymous clients share one allowlist; the threat model was already "anyone on the LAN" |
| 3 | SUBACK doesn't expose a missing read permission on Mosquitto | Already resolved by the live checks (below): the echo, and an acceptance step for HA's commands reaching the strip, now sharpened |

**The house's HA setup** — feedback from the HA owner, 2026-10-29.

| # | Feedback | Disposition |
|---|---|---|
| 1 | No add-ons: HA in Docker in a Lima VM, Mosquitto its own container | Adopted: *This install*; "the existing broker" |
| 2 | Anonymous on the 1884 LAN listener, ACL'd; no secret on the PC | Adopted, with one correction: `readwrite` on `jdrgb/jdrgb_<name>/#`, because the bridge subscribes to its command topic there. `user`/`password` kept optional. Listener address left as a placeholder |
| 3 | Keep MQTT discovery | Kept; the reason (`--unpublish`) is now stated |
| 4 | HomeKit Bridge takes an explicit include list | Adopted as a Phase 3 acceptance step |
| 5 | Matter out of scope for this house | Adopted |
| 6 | Scenes: true state at once, Off → Relax, asleep, Dreams effect | Adopted as *Scenes*. Off → Relax collapses to one card command, which is `on --gpu` only if Relax sends a plain ON |
| 7 | Dates were UTC | Fixed to 2026-10-29 |

**Live checks on the house's HA** — by the HA owner, 2026-10-29.

| Result | What changed |
|---|---|
| Mosquitto 2.0.22 grants ACL-denied subscriptions (SUBACK `0`) | The SUBACK check no longer claims to catch ACL mistakes; the bridge proves `read`/`write` with an echo before `online`; acceptance adds "an HA command changes the strip" |
| The 1884 listener is on the HA VM's own LAN address | Confirmed from the PC; kept out of this document |
| Discovery renders cleanly on 2026.10.4; entity named after the device | Default device name is now the computer's name plus "LEDs", so two PCs can't collide |
| HA skips calls to an unavailable light; scripts continue | *Exit* and *Scenes* corrected: buffering only inside the ~45 s before the will |
| A restore sends effect only | *Scenes*: restores are exact; Studio wiring named (Relax, Bright, Dreams) |
