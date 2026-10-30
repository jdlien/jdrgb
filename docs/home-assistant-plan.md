# Plan: the LEDs as a Home Assistant light

Status: **planned, not started.** Written 2026-10-30 and revised three times the
same day after external review (Appendix B). The `off`/`on` pair this builds on has
landed; everything below is still to do.

The consumer is a Home Assistant install on the LAN, and through it a Hue
dimmer switch, Apple Home (via HA's HomeKit Bridge) and, optionally, Matter.
The goal is that the PC's LEDs show up in all of them as an ordinary light.

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
| Transport | **MQTT with HA discovery.** The PC connects *out* to the broker; nothing listens on the PC |
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
| HomeKit / Matter | No code. HA's HomeKit Bridge; a community Matter bridge |

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
- **MQTT** inverts the connection: the PC dials out to the broker HA already
  runs (the Mosquitto add-on), so **no port is opened on the PC**. Discovery
  builds the entity from one retained message, state is pushed both ways, and
  the broker's last will marks the light *unavailable* when the PC sleeps or
  the bridge dies.

The cost is the Mosquitto add-on and a broker password on the PC, sent in the
clear on the LAN. Use a dedicated broker user for the bridge; restricting it by
ACL to the bridge's topics is recommended, and makes a leaked password worth
exactly "can change the PC's LED color". TLS stays possible later.

`docs/streamdeck-plan.md` lists "a socket, named pipe, or HTTP surface" as a
non-goal. That still holds for `jdrgb.exe`. Remote control needs *something* on
the PC to receive commands; of `sshd`, a listener, or an outbound client, the
outbound client can do the least.

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
- **The stash.** `stash_now`'s check-then-write runs under the state lock.
  `restore` reads the stash and keeps its contents. Before each device leg,
  holding that device's lock, it checks the stash file still has those
  contents; if it was removed or replaced, someone has decided since, so it
  stops there and reports what landed. After the last leg it deletes the stash
  under the state lock only if the contents still match, so it can't delete a
  newer stash created mid-restore. Today's sequence — read, apply both,
  unconditional `remove_file` (`src/main.rs:1196-1205`) — has neither check.

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

Linear scaling is the starting point because the LEDs are PWM-driven. Whether
the slider wants a perceptual curve is decided by eye in Phase 4, in the
bridge's mapping — never in `--level`, which stays a plain multiplier.

### Phase 3 — the bridge, level 1: on/off and presets

`src/ha/main.rs` → `jdrgb-ha.exe`, `windows_subsystem = "windows"`, no window.

**Ownership: one bridge per machine.** A `Global\jdrgb-ha` mutex: an instance
that finds it held, *or is denied access to it*, exits. The MQTT client id is
`jdrgb-ha-<computername>`, so a second bridge anywhere kicks the first off the
broker rather than silently sharing its topics. `install.ps1 -Ha` installs for
one user, carrying that user's SID through elevation as `-Tray` does
(`install.ps1:140`, `:292`). Multi-user arbitration is a non-goal.

**Config.** `%LOCALAPPDATA%\jdrgb\ha.conf`, `key=value` like the state file:
`host`, `port` (default 1883), `user`, `password`, and an optional `name` for
the device in HA (default `PC LEDs`). Plain text in the user's own profile.
Missing or unreadable → logged, exit. The installer resolves *that user's*
profile path from the SID (not the elevated admin's), checks the file exists,
and prints what to put in it if not.

**Logging.** `%LOCALAPPDATA%\jdrgb\ha.log`, truncated to its last half when it
passes a small size. There is nowhere else for an error to go.

**Spawning.** The HWND-free half of `src/tray/worker.rs` — `run_with`,
`cli_path`, `CREATE_NO_WINDOW`, the 90 s `CHILD_TIMEOUT` — moves to
`src/lib.rs` as `jdrgb::spawn`, used by both resident binaries. The tray keeps
its `PostMessageW` wrapper. `lib.rs`'s "only colors and state" comment widens to
"colors, state, and how to run the CLI"; the device layer stays out. The bridge
never passes `--wait`: a light command should fail fast, not retry for a minute.

**Children die with the bridge.** Windows doesn't end a process's children when
the parent is killed, and `run_with`'s timeout only protects a child while the
parent is alive to call `kill()` (`src/tray/worker.rs:99-102`). A bridge killed
at logoff or by uninstall would leave a `jdrgb.exe` mid-write with nobody
watching it. So at startup the bridge puts itself in a job object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (one more `windows-sys` feature,
`Win32_System_JobObjects`); children inherit the job and end with the bridge.
A child ended mid-write is the case the tray's timeout already accepts. Tested
with a harmless sleeping child, never with hardware.

**Threads.** Two. `rumqttc`'s `Client::publish` and `subscribe` are blocking
sends on a bounded channel that only the connection's own polling drains
(rumqttc 0.25.1), so a blocking call on the polling thread can deadlock it —
and splitting connection lifecycle across threads lets a step meant for one
connection land on the next. So one thread owns the connection outright:

- *Network* owns the connection and **every** `Client` call. It loops on
  `Connection::recv_timeout(100 ms)`, handles the event, then moves the
  worker's requests into a small outbox and flushes it with the non-blocking
  `try_publish` / `try_subscribe_many`. Anything the full request queue refuses
  stays in the outbox for the next pass; state in the outbox is latest-wins.
  Incoming commands are parsed here and handed to the worker.
- *Worker* owns the pending intent, the follow, pacing and children, and never
  touches the `Client`. It wakes on a message or a 1 s tick; on a tick it
  `stat`s the state file and asks the network thread to republish if the
  modification time changed — no spawn. A 90-second child blocks only this
  thread.

Keepalive is 30 s, driven by the polling, so a slow child can't drop the
connection. After a connection error the network thread waits 1 s before
polling again — `rumqttc` reconnects on the next poll with no delay of its own —
and logs a repeated identical error once rather than every second.

**Connecting.** Everything here is scoped to one connection. A connection error
discards that connection's "ready" flag, its queued discovery and availability,
and its awaited SUBACK; only the latest state survives in the outbox. On
`ConnAck` the network thread subscribes to the command topic and
`homeassistant/status` in one SUBSCRIBE, noting its packet id from the
`Outgoing::Subscribe` event. **Availability goes `online` only when that
SUBACK grants the command subscription** — `rumqttc` doesn't inspect return
codes (`handle_incoming_suback` is `Ok(None)`), so the bridge does: a `0x80` for
the command topic is logged as a broker ACL problem, availability stays
`offline`, and the subscribe is retried every 5 minutes on the same connection.
Once granted: discovery, `online`, then current state. **No step of connecting
ever writes hardware.** HA's birth message (`homeassistant/status` = `online`)
republishes discovery and state the same way.

Retained messages on the command topic are ignored and logged: a command
someone left retained would otherwise replay on every reconnect. HA's own MQTT
light doesn't retain commands by default.

**Exit.** Any exit other than `--unpublish` — killed at logoff, crashed —
closes the socket, and the broker publishes the will at once. A sleeping PC's
will fires after 1.5 × keepalive, ~45 s; commands HA sends inside that window
are buffered and may apply on wake, and after it they are gone. Both are
acceptable for a light.

**Topics.** `<id>` is `jdrgb_` plus the lowercased computer name.

| Topic | Use |
|---|---|
| `homeassistant/light/<id>/leds/config` | discovery, retained |
| `jdrgb/<id>/set` | commands from HA, subscribed at QoS 0 |
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
  "device": { "identifiers": ["jdrgb_desk"], "name": "PC LEDs", "manufacturer": "jdrgb" }
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
takes the pending intent and resolves it against one **base**: the device the
state table below takes the look from — the strip, unless the strip is off or
absent and the card is lit. The result is a single command:

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
`jdrgb on --gpu`, `jdrgb P --gpu` — and three observations taken right then:
the strip's record, the card's record, and the stash's identity (absent, or its
contents). The card gets the *command*, not a copied value, so the CLI resolves
it against the card itself: a preset gets the card's calibration, and
`on --gpu` acts only if the card is recorded off, using the card's own lit
record.

A newer strip leg **replaces** the follow. There is never more than one, it
always mirrors the latest command, and nothing carries over from the one it
replaced.

The follow runs once the light has been quiet for 5 s and the bridge's previous
GPU leg was at least 60 s ago (pacing, below). Immediately before spawning, the
worker takes the three observations again. **Any difference means someone else
acted since** — a tray click, a terminal, a UPS hook — so the follow is dropped,
logged, and the truth published. Because the observations are taken *after* the
strip leg, a stash the strip leg itself cleared doesn't count as a difference.
The window between that check and the child holding the bus is the spawn plus
up to 10 s the CLI may wait for the bus lock; that window is accepted.

Nothing ever corrects the hardware toward an old intent. A message that arrives
while a child runs starts the next pending intent; it doesn't cancel the child.

Worked cases, each a pure resolution test:

| Sequence | Result |
|---|---|
| Red, then (level 2) brightness 40% before the follow runs | strip Red, then Red at 40%; the follow becomes `red --gpu --level 40`. The second command resolved against the strip, already Red |
| Red; a UPS hook paints amber with `--stash`; nothing more | follow dropped: the strip's record is no longer what the strip leg left |
| Red; UPS amber `--stash`; then plain `ON` | strip `on`, a no-op; the follow becomes `on --gpu`, also a no-op; the stash survives for the UPS restore |
| Warm White picked during a UPS borrow | the strip leg clears the stash — the new normal, as from the tray; the follow observes no stash both times and runs |
| `OFF` then plain `ON`, card lit | strip off, then back on; follow `on --gpu` is a no-op, so the card is never touched |
| `OFF` then plain `ON`, card already recorded off | strip back on; follow `on --gpu` restores the card's own lit record |

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
  one a minute. An empty bucket drops a command — no strip leg, no follow — logs
  it and publishes the truth, except an `off` for a strip not already recorded
  off, which always goes through, so the light can always be turned off. The
  bucket lives in memory and starts full on every launch. It guards against a
  looping automation; it is not a wear budget, since the controller's flash
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
approximate — and while a follow is pending the difference is the bridge's own
pacing, so the strip's look is simply right. The projection is for display
only; it never decides whether a leg is done.

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

**Remove.** `uninstall.ps1`, in order:

1. Disables and stops the `jdrgb-ha` task and ends any `jdrgb-ha` process — its
   children end with it, through the job object — then waits (bounded) until
   `Global\jdrgb-ha` is free and no `jdrgb.exe` from the install directory is
   running.
2. Runs `jdrgb-ha.exe --unpublish --config <that user's ha.conf>`. It takes the
   singleton itself, so a resident can't start mid-cleanup; connects with the
   client id `<id>-cleanup`; publishes **nothing** of its normal startup;
   publishes empty retained payloads to the config, state and availability
   topics at QoS 1 and waits for their three PUBACKs; queues DISCONNECT, polls
   until the `Outgoing::Disconnect` event confirms it was sent, then drops the
   connection — the client closes, per MQTT, rather than waiting for the broker
   to. One 15 s deadline covers the whole operation; missing it, or any failed
   step, exits non-zero.
3. Removes the task and binary, reporting a failed unpublish with the manual
   fallback (delete the device under HA's MQTT integration).

`ha.conf` is left in place and its path printed: it holds a password the user may
want to keep or delete deliberately. Acceptance check, in HA: after uninstall
the entity is gone and stays gone across an HA restart.

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
  produces the same wire value, and from `MIN_LEVEL` up it is the same preset;
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

## Open questions, to settle by eye or by test

1. **`MIN_LEVEL`, and linear vs perceptual brightness.** Tune on both devices at
   once with `--level`, the same way the presets were tuned; the collision test
   sets its lower bound.
2. **The bridge's 60 s GPU interval and 5 s settle.** Guesses between "seconds
   apart glitched" and "minutes apart didn't", like the CLI's own 1500 ms. Lower
   them only on evidence.
3. **The HomeKit wheel with a snapped echo.** Verify Apple Home settles on the
   reported color rather than fighting it.

## Appendix A — verified findings

Checked on this machine on 2026-10-30:

- **Discovery writes to the device:** `open_controller` calls `read_config`,
  which sends a request report (`src/main.rs:523`, `request` at `:556`).
- **The state file write is unlocked and truncating:** `update`,
  `src/palette.rs:597-604`. **The GPU records after its lock is released:**
  `apply_gpu_colors` (`src/main.rs:456`) returns before `record(true, …)` at
  `src/main.rs:403`. **`turn_on` reads state before opening any device**
  (`src/main.rs:1269`). **`keeping_lit` writes back a whole-state snapshot**
  (`src/palette.rs:630`).
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
  variants exist; `Connection::recv_timeout` exists, which is what lets one
  thread both poll and publish; `handle_incoming_suback` ignores the SUBACK's
  return codes. Default features can be turned off for a TLS-free build.
  *From review, not independently checked:* the event loop reconnects on the next
  poll with no delay of its own. The plan's 1 s wait is harmless either way.
- **`restore` deletes the stash unconditionally** once its legs land
  (`src/main.rs:1196-1205`). **The tray's child timeout depends on the parent
  living** to call `kill()` (`src/tray/worker.rs:99-102`), and Windows does not
  end a killed process's children.
- **Hue:** HA's Hue docs state one event per second per device and describe
  `hue_event` and device triggers.
- **Matter:** HA's own Matter integration is a controller only. The original
  Home Assistant Matter Hub ended maintenance in January 2026 and was archived on
  2026-03-11; a community fork and Matterbridge's HA plugin remain.

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
