# jdrgb

A tiny, single-purpose CLI that sets the LEDs on an **ASUS ProArt X870E-CREATOR**
motherboard and an **ASUS TUF RTX 5090** to a static color and exits. No driver,
no daemon, no admin rights to run, nothing resident in memory. The release binary
is ~240 KB and starts in about 5 ms.

There is also an optional [tray menu](#the-tray-menu-optional) — a separate
binary, so the CLI above stays exactly what it says it is.

Built because all I want is "warm white, always" instead of the firmware's
default pulsing rainbow — and OpenRGB, while it works, is far more than that
needs (and Armoury Crate, which is the only vendor way to touch the GPU, kept
crashing the machine).

## Hardware

This targets exactly one setup and makes no attempt to be general:

- **Motherboard:** ASUS ProArt X870E-CREATOR WIFI
- **Controller:** ASUS AURA LED Controller, USB `0B05:19AF` (HID interface 2)
- **Strip:** Phanteks NEON Digital-RGB M5 550mm (38 addressable LEDs)
- **GPU:** ASUS TUF GeForce RTX 5090 (`10DE:2B85`, subsystem `1043:89EF`)
- **GPU controller:** ENE `AUMA0-E6K5-1107` at I2C `0x67`, 4 LEDs, reached via
  NVAPI — userspace, no kernel driver, no admin

## Install / build

Requires the Rust toolchain (MSVC). Build the optimized binary:

```powershell
cargo build --release
# -> target\release\jdrgb.exe
```

## Usage

```
jdrgb                 default color, coolwhite (#FFB0D0)
jdrgb NAME            a named preset, e.g. jdrgb red   (see: jdrgb presets)
jdrgb RRGGBB          a hex color, e.g. jdrgb ffcf9e
jdrgb off             turn the LEDs off
jdrgb on [color]      bring back what `off` turned off (color = if nothing kept)
jdrgb presets         list the named color presets
jdrgb load [file]     per-LED colors from a config file (default leds.conf)
jdrgb template [file] write a starter config, one line per LED
jdrgb state [file]    write what's set now as a config (no file = stdout)
jdrgb restore [color] put back what --stash saved (color = if none saved)
jdrgb rainbow [n]     per-LED rainbow across n LEDs (default 38, white end-caps)
jdrgb tune [color]    dial in a color live (preset/hex, or that device's last)
jdrgb preview         slideshow all presets (+/- speed, n/N next/prev, s stop, q quit)
jdrgb probe           show firmware + config table (diagnostics)
jdrgb --gpu save      commit the GPU's current color to its flash
jdrgb --version
jdrgb --help

  --wait              retry ~60s until the controller is ready (used at boot)
  --gpu               act on the GPU LEDs instead of the motherboard
  --all               act on both
  --stash             before setting a color, save what's showing for `restore`
  --level N           dim a solid color to N percent (1-100), after calibration
```

### Targets

Without `--gpu` or `--all`, everything targets the motherboard strip exactly as
it always has. `rainbow` and `template` are motherboard-only — the GPU zone is
4 LEDs, so a 38-LED per-LED pattern there is meaningless, and passing `--gpu`
with them is an error rather than a partial apply.

`load` is the exception: a config file can carry a `gpu:` line, so one file
describes the whole machine. `jdrgb load theme.conf --all` paints both.

```powershell
jdrgb warmwhite         # motherboard strip (unchanged behavior)
jdrgb --gpu warmwhite   # GPU only
jdrgb --all warmwhite   # both
```

### Color presets

A color can be a case-insensitive keyword or an `RRGGBB` hex string. `jdrgb
presets` prints them with swatches. They're only starting points — these LEDs
render colors quite differently from nominal RGB, so tune any that look off.
Notably `white` (`#FFFFFF`) reads greenish here, so the default is `coolwhite`
(`#FFB0D0`), a by-eye-tuned clean white; `warmwhite` (`#FA9536`) is the original
warm tone.

```
coolwhite  warmwhite  white  black  red  burntorange  orange  amber  yellow
chartreuse  lime  green  seagreen  teal  turquoise  cyan  sky  azure  cobalt
sapphire  blue  indigo  purple  violet  magenta  cerise  hotpink  rose  pink
```

#### `black` vs `off`

They look the same but are different controller states, confirmed by reading the
mode register back with `jdrgb --gpu probe`:

| command | mode register |
|---|---|
| `jdrgb off` | `0` — the controller's own off mode |
| `jdrgb black` | `1` — static, displaying `#000000` |

There's no measurable power difference: the LEDs draw nothing either way and the
controller stays powered regardless. Prefer `off` when you mean "dark" — it's the
honest expression of intent and what gets committed if you `save`. `black` exists
so a theme can leave one device dark while the other is lit, e.g.
`jdrgb --gpu black` alongside `jdrgb warmwhite`.

#### Per-device calibration

The GPU renders colors quite differently from the strip — the same nominal value
can look nothing alike on the two.

One pattern holds across every entry: **this card's blue is far stronger than the
strip's**, so every calibration lowers blue relative to the other channels.
Nothing else is consistent — green against red moves both ways, and only slightly.

| preset | strip | GPU |
|---|---|---|
| `coolwhite` | `#FFB0D0` | `#D29432` |
| `warmwhite` | `#FA9536` | `#FF560A` |
| `burntorange` | `#FF1D00` | `#FF1000` | *interp* |
| `orange` | `#FF3A00` | `#FF2000` |
| `amber` | `#FF8700` | `#FF5400` |
| `yellow` | `#FFD000` | `#FF8C00` |
| `chartreuse` | `#D4FF00` | `#FFC000` |
| `lime` | `#80FF00` | `#DEFF00` |
| `seagreen` | `#00FF51` | `#00FF15` |
| `teal` | `#00FF80` | `#00FF2F` |
| `turquoise` | `#00FFBF` | `#00FF49` | *interp* |
| `cyan` | `#00FFFF` | `#00FF62` |
| `sky` | `#00BFFF` | `#00FF8C` | *interp* |
| `azure` | `#0080FF` | `#00FFB6` |
| `cobalt` | `#0040FF` | `#00C0FF` |
| `sapphire` | `#0020FF` | `#0060FF` | *interp* |
| `indigo` | `#2700FF` | `#BB00FF` |
| `purple` | `#4000FF` | `#FF007F` |
| `violet` | `#6E00FF` | `#FF0062` |
| `magenta` | `#FF00FF` | `#FF0026` |
| `cerise` | `#FF00BF` | `#FF001B` | *interp* |
| `hotpink` | `#FF0080` | `#FF0011` |
| `rose` | `#FF0040` | `#FF0008` | *interp* |
| `pink` | `#D52A66` | `#E71F18` |

That one effect explains corrections that look opposite. Where blue is the minor
channel it gets cut outright (`magenta` 255 → 38). Where blue is already dominant
and pinned at max, the same reduction has to be expressed by raising the others
instead (`azure` green 128 → 255, `indigo` red 39 → 187). Same rule, different
arithmetic.

The whites diverge most because they carry all three channels, so the blue excess
has nowhere to hide. Nominal `#FFFFFF` reading as sky blue is the same effect at
its most obvious — which is why `white` is left uncalibrated as a reference.

Pure primaries (`red`, `green`, `blue`) need no entry — one channel at max, with
no ratio to correct.

So a **preset name resolves per device**, while an **explicit hex is always
literal**. That split is what keeps `tune` honest: the hex it prints reproduces
exactly what you were looking at, on whichever device you were looking at.

```powershell
jdrgb --all warmwhite   # strip #FA9536, GPU #FF560A — same look, different values
jdrgb --gpu FA9536      # literal: no substitution
```

The override table (`GPU_PRESETS` in `src/main.rs`) is deliberately sparse. Only
presets actually dialled in by eye on the GPU belong there; everything else falls
back to the shared value, so the table never claims a calibration nobody has
looked at. `jdrgb presets` marks the calibrated ones with a `gpu` column.

#### Interpolated presets

Six entries are marked *interp*. They were not dialled in by eye — they were
derived from the ones that were.

Each calibrated pair is a record of two values you judged to *look alike*, so the
set as a whole is a sampled `strip hue -> GPU hue` function. It turns out to be
well-behaved: monotonic across all 18 originally-tuned pairs, and purely a matter
of hue, since every saturated value in both tables sits at full saturation. That
makes it something you can interpolate through rather than guess at.

They fill the widest gaps, taking the largest spacing from 30° down to 20° across
24 saturated hues. Each sits where the slope agrees between neighbouring samples.
The volatile region is `blue -> indigo -> purple`, where it runs 3.0 → 7.8 → 0.6
over about 25° — interpolating there would be guesswork, but it's already the
densest part of the wheel and needed nothing.

The method only works on the full-saturation ring, which is where all the data
lives. A darker or muted color — a proper crimson, say — sits off that ring
entirely and would have to be tuned by eye on both devices.

Confirm one in `preview`, then drop its `interp` marker; re-tune it if it's off.

##### What the gap numbers assume

`sapphire` is there because `cobalt -> blue` looked visibly wide while measuring
as an ordinary 15° step — the smallest class of gap left. The measurement was
wrong, in a way worth recording.

Gap analysis reads each preset's appearance from `SWATCH`, falling back to its
wire value where no entry exists. That fallback is an assumption, not data:
**untuned means unmeasured, not undistorted.** The tuned neighbours give it away.
`indigo` sits at wire 249 to appear at 255 and `purple` at wire 255 to appear at
270 — an appearance/wire slope of 1.6–2.5. The strip expands hue through that
region, and the expansion doesn't begin abruptly at `blue`. At a slope near 1.6,
`cobalt -> blue` is perceptually ~24°, the widest gap remaining.

`cyan`, `azure` and `cobalt` are all untuned too, so the same assumption is
load-bearing across that whole stretch, and `sky` and `turquoise` may be slightly
misplaced for it. The fix isn't more presets — it's `jdrgb tune azure` (and
`cobalt`, `blue`), recording what they actually look like in `SWATCH`. That would
hand the interpolator real data exactly where the strip is least linear, instead
of a straight line it assumed.

#### The third table: what a preset looks like

Tuned values are wrong on a *monitor* for the same reason they're right on the
hardware. `coolwhite` is `#FFB0D0` because that renders as clean white on the
strip — but drawn on screen it's pink. So a terminal swatch painted from the
device value shows what jdrgb is **sending**, when the useful thing is what
you'll **see**.

`SWATCH` in `src/main.rs` is the third view: nominal sRGB, what the name means to
your eye. `preview` uses it, so in this line —

```
[▓▓▓▓▓]  coolwhite  #FFB0D0   (1/23)   dwell 4.0s
```

— the block is a clean white while the hex stays `#FFB0D0`. They disagree on
purpose: appearance on the left, wire value on the right.

Unlike the other two tables it takes no device argument. Both calibrations exist
to produce the *same* color, so appearance is the one property that doesn't vary
by target. It's display-only and never reaches hardware, so a wrong value here is
cosmetic — tune it against your monitor the way the device tables were tuned
against the LEDs.

`tune` keeps showing its literal value, since a hand-dialled hex has no intended
appearance to look up — the number *is* the intent. Mapping arbitrary values to
screen colors would need a real per-device profile, which is a lot of work for a
swatch.

To add one, tune it and paste the hex it prints:

```powershell
jdrgb --gpu tune amber      # dial by eye, q to quit
```

A test asserts every name in `GPU_PRESETS` matches a real preset, so a typo fails
the build instead of silently doing nothing.

### GPU zones

The card has four LEDs and they *are* individually addressable — driving
red/green/blue/white produces four distinguishable regions across the shroud.

They're not isolated zones, though. The TUF logo renders a smooth green-to-blue
*gradient* rather than two flat halves, and both left tick marks take one color
while both right ticks take another. That looks like four point sources behind one
continuous light guide, each feature picking up whichever lamp is nearest.

The bleed is severe: pure `#FF0000` renders pink and pure `#00FF00` renders teal,
neither having any blue in it. Saturated per-LED patterns come out muddy.

Soft gradients between neighbouring hues are what it's actually good at, since the
blending works in your favour:

```
gpu: warmwhite amber warmwhite amber
```

### Per-LED config file

For dialing in individual LEDs, use a plain-text config: one `RRGGBB` hex color
per line, top line = LED 0. `#` starts a comment; blank lines are ignored. Any
LEDs past the end of the file are turned off.

```
# leds.conf
FF0000   # LED 0
FA9536   # LED 1
...
0000FF   # LED 37
```

Generate a starter file pre-filled with the preset (`jdrgb template leds.conf`),
edit it, and preview with `jdrgb load leds.conf` — re-run after each edit until
it's dialed in.

Two directives set a whole device at once instead:

```
mb:  warmwhite     # the whole strip, one color (not per-LED lines — use one or the other)
gpu: warmwhite     # the card; several colors drive its LEDs individually
gpu: off           # either directive takes `off`, the controller's own off mode
```

A device the file doesn't mention is left alone, so a config can describe one
device, the other, or both. `off` is not `black` — see above.

### Saving and restoring what's showing

`jdrgb state [file]` writes the current setting out as a config file, in exactly
the grammar `load` reads. With no argument it prints, so `>` works too.

```powershell
jdrgb state before.conf        # snapshot whatever is showing
jdrgb red --all                # ...borrow the lights...
jdrgb load before.conf --all   # put them back
```

This is what makes jdrgb usable from something else's event hook — a UPS going
to battery, a game launcher, a recording script — without that thing having to
know what your normal lighting is.

#### `--stash` and `restore`

The same thing with the bookkeeping done for you:

```powershell
jdrgb amber --all --stash     # set amber, remembering what was showing
jdrgb red   --all --stash     # ...again; the first snapshot is kept
jdrgb restore --all           # put the original back
```

Two rules carry it, and both were got wrong by hand first:

- **A second `--stash` never overwrites the first.** A UPS outage fires
  `on_battery` and then `on_pending`; if the second snapshot won, mains would
  restore the room to amber instead of normal.
- **An apply *without* `--stash` drops the snapshot.** Pick a color from the
  tray mid-outage and that's your new normal — restoring over it afterwards
  would stomp on a deliberate choice.

Every `--stash` also stamps the stash with a fresh `# stash-id:` comment —
including one that keeps an existing snapshot, which keeps its colors and gets
only the new id. `restore` checks that id again before each device it puts back,
while holding that device: if the stash was removed, replaced or re-stamped since
it was read, someone has borrowed the lights again (or chosen a color) in the
meantime, so it stops and leaves the stash for them. The id is what makes that
check possible at all — the same lighting stashed twice is the same bytes.

`restore` with nothing stashed is a silent success, so a hook that fires twice,
or on a machine that never stashed, leaves the lights alone rather than failing.
`jdrgb restore coolwhite` gives it something to fall back on instead.

A stash from a *previous boot* is ignored and removed. This is not an edge case:
an outage long enough to matter ends with the machine shut down, so `on_mains`
never runs and the snapshot is still on disk at next boot. The check is the
stash's age against `GetTickCount64` — if it's older than the machine has been
up, it predates the boot. (Measured on this machine: that tick count and the
boot time Windows reports agree to within 1 second across 31 hours of uptime.)

Why this isn't left to a shell one-liner: the obvious cmd spelling of the first
rule is wrong.

```cmd
if not exist "%LOCALAPPDATA%\jdrgb\stash.conf" jdrgb state "..." & jdrgb red --all
```

`&` does not end the `if` body, so **both** commands are skipped when the file
exists — the color never gets set. It reads correctly and isn't.

Everything round-trips: a solid, a per-LED pattern, `off`, and a machine where
the two devices are in different states. That's the reason `mb:` exists. Written
as 38 identical bare lines, a solid strip would go out over *direct* mode — a
different controller state, recorded as `multi` — so the tray would read back
"Multi" where it used to read "Warm White".

Two things it can't do:

- **It reports what jdrgb recorded, not what the hardware holds.** The strip's
  controller offers no color readback, so the record is the only source. (The
  card does have one — `jdrgb --gpu probe` reads its live color over I2C — but
  mixing the two would make one file disagree with itself about where the truth
  lives.) A color set by Armoury Crate or another tool is not seen.
- **A boot-task apply is invisible to your session.** That task runs as SYSTEM
  and records to SYSTEM's profile, so on a machine set only at boot, `state`
  has nothing to report until you set a color yourself. It says so and exits
  non-zero rather than writing a file that would restore you to darkness.

#### `off` and `on`

`jdrgb off` remembers what each device was showing, and `jdrgb on` puts it back
— a solid, a per-LED pattern, or the two devices showing different things.
Unlike the stash, this survives a reboot, so the lights can be turned off at
night and back on to the same thing the next morning.

```powershell
jdrgb off --all    # dark, remembering what was lit
jdrgb on --all     # back to it
```

It is built to sit behind a switch, so pressing either one twice is harmless.
A second `off` keeps the color the first one saved, rather than saving "off".
`on` only touches a device recorded as off, so with the lights already lit it
sends nothing to the hardware and costs the strip no flash write.

A device that is off with nothing saved — turned off by a jdrgb from before
`on` existed — comes back in the default color, or `jdrgb on warmwhite` names
one. A device never recorded at all is left alone, because nothing says it is
off.

`on` leaves the stash alone, so pressing it mid-outage can't drop what a UPS
hook is waiting to restore. The two cooperate in the other direction too. Turn
the lights off, let a UPS hook paint them amber with `--stash`, and when mains
`restore`s them to off, `on` still brings back the color they were turned off
from, not the amber.

### Brightness: `--level`

```powershell
jdrgb warmwhite --all --level 40    # warm white at 40%, on both
```

Each device resolves the preset with its own calibration *first*, and then every
channel is scaled — `(c × N + 50) / 100`. The order matters: dimming the shared
value and calibrating afterwards would dim the strip's warm white on the card,
which is a different color there. The dimmed value is what goes on the wire and
what gets recorded, so `state`, `load`, `--stash`/`restore` and `off`/`on` all
carry it through unchanged.

It is a plain multiplier, which keeps a color's ratios until the 8-bit channels
run out of room: `coolwhite` at 2% is `(5,4,4)`, and presets start rounding to
the same bytes. The CLI takes the whole 1–100 range anyway; `MIN_LEVEL` in
`src/palette.rs` (10 for now — the lowest level at which no two presets share a
value on either device) is the floor anything driving a slider should map onto.
Whether 10% still looks like the right color is a question for the eye.

`--level` applies to a solid color only. `off` has nothing to dim, and `on`,
`load` and `restore` put back recorded values that already carry their level —
so with those it's an error rather than a flag that silently does nothing.

### Tuning a color

`jdrgb tune [color]` steps a color live in HSL — `h`/`s`/`l` nudge each channel
down, `H`/`S`/`L` up (hold a key to ramp). It shows the current HSL, RGB, and hex
in a compact status line (cyan keys, yellow labels, bold-white values, plus a
live swatch), and `q` quits keeping the color and printing its hex. Live steps
skip the flash-save; the final pick is committed. With no argument it starts from
the last solid color set, or the default if that device was left multi-colored by
`rainbow`/`load`.

The last-set color is remembered per device in a small state file under
`%LOCALAPPDATA%\jdrgb`, with a separate slot for the strip and the GPU:

```
mb=FFB0D0
gpu=D29432
```

A device left multi-colored records `multi` plus the frame itself, so `state`
can reproduce it — `multi` alone says only "no single color", which is the right
answer for the tray's swatch and useless for a snapshot:

```
mb=multi
mb.pattern=FA9536,FA9536,...,FF0011
```

A frame is only ever written alongside `multi`, and any other write to that slot
clears it, so a frame can't outlive the state that produced it.

A device that went from lit to off also keeps what it was showing, for `on`, in
the same two-key shape:

```
mb=off
mb.lit=FA9536
gpu=off
gpu.lit=multi
gpu.lit.pattern=FF560A,FF5400,FF560A,FF5400
```

That record is written only on the way into off, and nothing else clears it.
`on` only reads it for a device that is off, so a stale one on a lit device is
never used.

The file also carries `rev=N`, a count of writes. Every write bumps it, even one
that changes no value, so anything watching the file can tell that *someone
wrote* when they wrote the same values again — which the values alone can't
show.

Writes go through a lock on a sidecar file, `state.lock`, so two processes
recording at once can't lose each other's update, and each one replaces the file
by writing `last.tmp` and renaming it over `last`, so a reader sees the old file
or the new one and never a half-written one. A reader in another language has to
open `last` with delete sharing (`FileShare.Delete` in .NET) or it blocks the
rename. If a record can't be written, the command says so on stderr: the lights
changed, but nothing reading the file will know.

The split matters because the same look is a *different* RGB on each (see the
calibration table above). A shared slot would hand `jdrgb --gpu tune` the strip's
value, which on the card doesn't render as anything like what was on screen. So
`--gpu tune` resumes the GPU's last color and `jdrgb tune` the strip's, and each
falls back to its own calibrated `coolwhite` when nothing has been recorded.
Under `--all`, `preview` writes each device the value that device actually got.

To change the built-in default, edit `DEFAULT_COLOR` in `src/main.rs`.

Solid colors are set via the controller's **static effect** mode, which the
hardware latches *and saves* — the color holds with nothing running. The per-LED
`rainbow` uses **direct** mode, streaming one frame that the controller also
latches; it's a static frame, not an animation, so it likewise holds after the
program exits.

#### Why live stepping doesn't crawl

Setting a color takes seven packets: a protocol-select, then a channel+mode
select and a color for each of the three headers. Sending all seven on every
repaint made the strip trail the GPU by about **half a second** — badly enough
that holding a key in `tune` didn't ramp, it crawled.

The host writes were never the cost; they measure ~4ms each. The delay was in the
controller, which appears to restart the effect whenever the mode is re-selected.
So `tune` and `preview` send the full sequence once, then **colors only** — three
packets instead of seven. The lag drops to a few milliseconds and a held key
ramps properly. All three headers still follow, so a color write lands without
the channel being re-selected first.

Committing paints always re-select, so what reaches the controller's flash is
built by the same full sequence as a one-shot `jdrgb COLOR`.

### GPU persistence

**The GPU's flash save works.** This was the open question for most of the build —
the widely repeated claim is that ASUS GPUs revert to their firmware rainbow on
power loss, and `jdrgb --gpu save` was written expecting to find out it didn't
stick.

Tested properly: set the card to green *without* saving, so flash held warm white
while the live registers held green. Then a full shutdown with the PSU switched
off for 30 seconds. It came back warm white — instantly, before POST completed,
with no boot task installed and no vendor software present. The green was gone, so
power really was lost; the warm white came from flash.

That also shows setting and saving are cleanly independent: preview as much as you
like, and only commit when you mean it.

So Armoury Crate reasserting the color at every boot was never necessary. One
`save` covers it, including during POST and in the BIOS, where nothing is running
to set anything.

```powershell
jdrgb --gpu warmwhite     # set what you want
jdrgb --gpu save          # commit it to the controller's flash
```

`save` is deliberately a separate, manual command and is never part of the boot
task: flash has finite write cycles, so committing on every startup would be a bad
trade for something you change once a year. Re-run it whenever you change the
color, or flash keeps serving the old one at POST.

Verify what's actually stored with `jdrgb --gpu probe`, which reads the color back
off the chip rather than reporting what jdrgb last sent.

The motherboard behaves the same way — jdrgb commits to the Aura controller's
flash on every solid set. **Which means neither device needs the boot task below**
for ordinary use. It's there as insurance for the cases that clear the
controllers: a BIOS update, a CMOS reset, or reinstalling vendor software.

## Run at boot (optional — see GPU persistence above)

`install.ps1` installs nothing by default. Run it bare and it prints its usage:
`-Tray` gets you the menu, `-Boot` gets you the boot task, and you can ask for
both. Nothing is elevated until you've named one, so reading the usage never
raises a UAC prompt.

**Why opt-in.** A boot task that reasserts a color is the wrong default once the
color is already latched in flash. Worse, it's invisible: it runs as `SYSTEM`, so
an unelevated `Get-ScheduledTask` won't even list it, and the only symptom is
that the strip quietly reverts on every startup — while the GPU, which the task
wasn't pointed at, keeps whatever you last set. That is a genuinely confusing
afternoon, and it's avoided by not installing the thing unasked.

`-Boot` registers a Scheduled Task that runs as the `SYSTEM` account — so the
color is set during boot, before anyone logs in. It copies the binary to
`C:\Program Files\jdrgb` and uses `--wait` (retry up to ~60s) to tolerate the
USB controller not being enumerated yet.

Triggers, for reliability: **at startup** (the pre-login goal), **at logon** (a
belt-and-suspenders re-apply that survives a late controller reset), and **on
resume from sleep**.

```powershell
# from an elevated PowerShell (the script self-elevates if needed)
.\install.ps1 -Boot                        # boots to the default (coolwhite)
.\install.ps1 -Boot -Color warmwhite       # boot to a preset name (or an RRGGBB hex)
.\install.ps1 -Boot -Config leds.conf      # boot to a saved per-LED pattern
.\install.ps1 -Boot -All -Color warmwhite  # motherboard strip + GPU
.\install.ps1 -Boot -Gpu -Color warmwhite  # GPU only
```

`-Color`, `-Config`, `-Gpu`, `-All` and `-NoWake` all configure the boot task, so
each one requires `-Boot`. Passing one without it is an error rather than a
silent no-op, which would otherwise look like the color had been set up when
nothing was listening.

`-Config` copies your file to `C:\Program Files\jdrgb\leds.conf` and points the
boot task at it (`jdrgb load … --wait`). It combines with `-Gpu`/`-All`, since a
config's `gpu:` line can carry the card's colors — if the file has no `gpu:`
line, jdrgb says so rather than failing silently.

It also adds a resume-from-sleep trigger so the color reasserts after waking.

**Running without `-Boot` removes an existing boot task.** That's how you back
out of one: `.\install.ps1 -Tray` leaves you the menu and nothing writing flash
behind your back.

Remove everything with:

```powershell
.\uninstall.ps1
```

## The tray menu (optional)

`jdrgb-tray.exe` is a notification-area menu for picking a preset without
opening a terminal. The icon *is* the current color — a filled dot, or a hollow
ring when the LEDs are off.

```powershell
.\install.ps1 -Tray                               # the tray at logon, no boot task
.\install.ps1 -Tray -Boot -All -Color warmwhite   # both, if you want the insurance
```

**`-Tray` on its own is usually the one you want.** The color is already held in
the controllers' flash (see [GPU persistence](#gpu-persistence)), so it survives
a cold boot with nothing running — which makes the boot task insurance rather
than a requirement, and it isn't free: it fires at startup, at logon *and* on
resume, writing flash each time to reassert a color that was already there. Add
`-Boot` if you want cover for a BIOS update or a CMOS reset. Otherwise the tray
is the only thing that actually needs to persist, because unlike a color, a menu
can't be latched into hardware.

The menu carries a shortlist of colors inline and the full palette under
**All Colors**. Edit `FAVOURITES` in `src/tray/main.rs` to change the
shortlist; a test checks every name against the preset table.

It is a **second binary, and a second task**. Two binaries because a PE has
exactly one subsystem: `jdrgb.exe` has to stay a console app, and a tray app
must not be one. They share the color tables through a small library, so the
menu draws from the same `SWATCH` values as `jdrgb presets` — a preset tuned by
eye shows up in both without a second edit.

Two tasks because the boot task runs as `SYSTEM` in session 0, which has no
interactive desktop and therefore no notification area. The tray gets its own
per-user logon task, unelevated. Ask for both and they coexist — the boot task
still fires before anyone logs in.

The tray never touches the hardware. Every click spawns `jdrgb.exe`, which
means a wedged controller can't take the menu down with it, and any error you'd
have seen in a terminal arrives as a balloon instead.

It defaults to **both devices**, where the CLI defaults to the strip. Every
existing `jdrgb COLOR` invocation has to keep meaning what it always meant; a
menu has no such history, and "set the lights" means all of them. Change it
under `Target`.

### What it can't know

The boot task runs as `SYSTEM`, so it records its state under `SYSTEM`'s
`%LOCALAPPDATA%`, not yours. The tray reads *your* copy. So immediately after a
boot the menu says "not set from here" rather than naming the color you're
looking at — it knows what you set from the tray or from a terminal, and
nothing else. The first pick from the menu syncs it up.

## Home Assistant (optional)

`jdrgb-ha.exe` makes the LEDs a light in Home Assistant — on/off, the presets as
effects, a color wheel, white temperature and brightness — so a Hue dimmer, a
scene, or Apple Home through HA's HomeKit Bridge can drive them like any other
bulb. The design, and every decision behind it, is in
`docs/home-assistant-plan.md`.

It is a third binary and a third per-user logon task, like the tray, and like
the tray it never touches hardware: it spawns `jdrgb.exe`. It connects *out* to
an MQTT broker, so nothing listens on the PC. One bridge per machine.

```powershell
.\install.ps1 -Ha          # or -Tray -Ha for both
```

`-Ha` and `-Boot` can't be installed together (see *Being a good neighbour on
the bus*); `-Ha` removes an existing boot task.

### Configuring it

`%LOCALAPPDATA%\jdrgb\ha.conf`:

```
host=<the broker's LAN address>
port=1884
```

| Key | |
|---|---|
| `host` | required: the broker |
| `port` | default 1883 |
| `name` | the device's name in HA, which names the entity. Default: the computer's name plus "LEDs" — `light.desk_leds` on a PC called DESK |
| `user`, `password` | only if the broker wants a login. Without them the bridge connects anonymously, so no secret lives on the PC |
| `mode` | `wheel` (default), or `presets` for on/off and effects only — the fallback if a client fights the snapped color wheel |

The broker needs an ACL entry for the bridge, with `<name>` the computer's
name lowercased (hyphens kept; anything but letters, digits and hyphens
becomes `_`):

```
topic readwrite jdrgb/jdrgb_<name>/#
topic write     homeassistant/light/jdrgb_<name>/#
topic read      homeassistant/status
```

You don't have to derive it: `install.ps1 -Ha` prints the exact lines, and the
bridge logs them in `ha.log` every time it starts.

`readwrite` on the first line, because the bridge subscribes to its own command
topic there. On an anonymous listener those lines join an allowlist every
anonymous client shares; the ACL bounds what anonymous LAN clients can do as a
group, it doesn't isolate one from another.

Some brokers — Mosquitto 2.0 among them — grant a subscription their ACL denies
and quietly deliver nothing, and no MQTT 3.1.1 broker can refuse a publish. So
before it goes online the bridge publishes a random value to its own
`jdrgb/jdrgb_<name>/echo` and waits for it to come back. If it doesn't, the
light stays unavailable and `ha.log` says the ACL looks wrong.

Its log is `%LOCALAPPDATA%\jdrgb\ha.log`. There is nowhere else for an error to
go.

### What Home Assistant sees

- **Effects** are the 29 presets by name. Picking one sends the *name* to
  `jdrgb.exe`, so each device gets its own calibration.
- **The color wheel and the white-temperature slider snap to presets.** The
  calibration tables are points, not a continuous function, so the wheel lands
  on the nearest preset and the light reports where it really is. White
  temperature has two stops, Warm White at 2700 K and Cool White at 6500 K,
  split near 3815 K.
- **Brightness** is `--level`, mapped onto `MIN_LEVEL` (10%, provisional) to
  100%.
- A hand-tuned hex or a per-LED pattern shows as on, with no effect, and its
  color reported as an approximation.

### The card follows the strip

Every command goes to the strip at once. The card replays the same command once
the light has been quiet for 5 seconds, and the bridge never writes the card
more than once a minute — two GPU writes seconds apart once glitched the
display. So the dimmer's off turns the strip off at once and the card a few
seconds later, and twenty presses of brightness-up step the strip twenty times
and the card once. While the card is catching up, HA is shown the strip's state.

Nothing ever overrides a later decision: if the tray, a terminal or a UPS hook
changes the lights in between, the card's pending follow is dropped and HA is
shown what is really there. A runaway automation is capped at 30 strip changes,
refilling one a minute; an `off` still gets through once every 5 minutes.

### The Hue dimmer

Keep the dimmer paired to the Hue Bridge, so the room lights keep their native
response, and add an automation on its `hue_event` — check the exact event data
under Developer Tools → Events, listening for `hue_event`:

```yaml
automation:
  - alias: Dimmer drives the PC LEDs
    triggers:
      - trigger: event
        event_type: hue_event
        event_data: { device_id: <the dimmer's device id>, type: short_release }
    actions:
      - choose:
          - conditions: "{{ trigger.event.data.subtype == 1 }}"   # on/off
            sequence: [{ action: light.toggle, target: { entity_id: light.desk_leds } }]
          - conditions: "{{ trigger.event.data.subtype == 2 }}"   # brighter
            sequence: [{ action: light.turn_on, target: { entity_id: light.desk_leds }, data: { brightness_step_pct: 10 } }]
          - conditions: "{{ trigger.event.data.subtype == 3 }}"   # dimmer
            sequence: [{ action: light.turn_on, target: { entity_id: light.desk_leds }, data: { brightness_step_pct: -10 } }]
```

HA's Hue integration delivers at most one event per second per remote, which
suits a light whose card is paced anyway.

### Scenes

A `scene.create` snapshot restores as `{"state":"ON","effect":"<preset>"}`, so a
restored scene comes back as exactly the preset it was. Scripts that set the
light directly should name an effect rather than a color — the wheel would snap
it anyway. A scene that includes the light while the PC sleeps simply skips it.

### Removing it

`.\uninstall.ps1` stops the bridge, removes the light from Home Assistant
(`jdrgb-ha.exe --unpublish`), and then the task and binary. It leaves `ha.conf`
where it is, since on an install that uses one it holds a password.

## How it works

The ASUS Aura USB controller speaks a simple HID protocol: 65-byte reports whose
first byte is `0xEC`. jdrgb reads the config table (`0xEC 0xB0`) to learn the
addressable-header count, then either:

- **solid:** for each header, select static mode (`0xEC 0x35 …`), send the color
  (`0xEC 0x36 …`), and commit/save (`0xEC 0x3F 0x55`); or
- **per-LED:** switch the header into direct mode, then stream color data in
  packets of up to 20 LEDs (`0xEC 0x40 …`), flagging the final packet to latch.

The GPU is an entirely separate path. `nvapi64.dll` is loaded at runtime and
`nvapi_QueryInterface` resolves `NvAPI_I2CWriteEx`/`NvAPI_I2CReadEx`, which
tunnel SMBus transactions to the card's I2C port 1. The ENE chip there exposes
16-bit registers over 8-bit SMBus: select via command `0x00`, then read via
`0x81` or write via `0x01`/`0x03`. Setting a color writes the effect bank at
`0x8160` (**R, B, G** order, not RGB), sets mode `0x8021` to static, applies via
`0x80A0`, then clears the direct flag at `0x8020` — a stale direct flag silently
overrides the effect mode.

Two guards stand in front of every GPU write: an exact PCI vendor/device/
subsystem match, then the ENE signature (registers `0xA0`–`0xAF` must read
`0x00`–`0x0F`). The PCI gate comes first because an I2C read still puts a command
byte on the wire — the signature can't protect a device it has to touch to check.
The NVAPI struct layout is pinned with compile-time offset assertions, since a
wrong offset would silently send the wrong bytes to real hardware.

### Being a good neighbour on the bus

jdrgb once wedged a UPS on the same USB bus, because the HID library it used
enumerated every device on the machine and asked each one for its string
descriptors before filtering by VID/PID. `docs/ups-wedge-incident.md` is the full
account; it is worth reading before changing anything in `src/hid.rs`.

The rules that came out of it:

- **Only our device is ever opened.** `src/hid.rs` gets the HID interface list
  from the configuration manager, matches the VID and PID as text in the device
  path, and calls `CreateFileW` on nothing else. No other device is opened, read
  from, or sent anything.
- **GPU writes are spaced out and serialised.** The card's RGB controller sits on
  the GPU's own I2C block, which is display plumbing; two writes seconds apart
  have been observed to glitch the monitor. Separate invocations are held at
  least 1500ms apart, recorded machine-wide, and a named mutex keeps two
  processes off the bus at once — a command that finds the bus busy says so and
  fails rather than interleaving. Repaints *within* one `tune` or `preview`
  session use a 100ms floor instead, because a second and a half per keypress
  would make live tuning useless; that session holds the mutex until it exits.
- **The strip is serialised too.** A second named mutex, `Global\jdrgb-hid`, is
  taken before the controller is opened — reading its config table already sends
  a request — and held until the change is recorded, so two processes can't
  interleave their reports or record in the wrong order. Unlike the GPU's, it
  fails closed: if it can't be created, the command refuses rather than write
  unserialised. Busy is an error that `--wait` retries, after a 10 s wait.
  Between processes of one user this is guaranteed; between the SYSTEM boot task
  and a user it isn't (the mutex inherits its creator's DACL), which is why the
  Home Assistant bridge refuses to be installed alongside the boot task.

Two environment variables:

| Variable | Effect |
| --- | --- |
| `JDRGB_NO_HW=1` | Every hardware path refuses before opening anything. Parsing, config files, presets and `state` still work, so an automated run can exercise the CLI without a single USB or I2C transaction. **Set this when scripting or testing jdrgb.** |
| `JDRGB_GPU_GAP_MS=n` | Minimum milliseconds between GPU LED writes (default 1500). `0` disables the wait. |

If you are wiring jdrgb into something that runs at a delicate moment — a UPS
power hook, a shutdown script — prefer `--mb`, which never touches the graphics
card at all.

### Lineage & credits

The motherboard wire protocol was ported from **OpenRGB**'s
`AuraMainboardController` and cross-checked against **liquidctl**'s `aura_led.py`.
The GPU path was ported from OpenRGB's `ENESMBusController` and
`i2c_smbus_nvapi`. All are GPL-2.0-or-later; huge thanks to those projects for the
reverse-engineering that made this possible. Because it derives from that source,
this utility is likewise **GPL-2.0-or-later**.
