# jdrgb

A tiny, single-purpose CLI that sets the LEDs on an **ASUS ProArt X870E-CREATOR**
motherboard and an **ASUS TUF RTX 5090** to a static color and exits. No driver,
no daemon, no admin rights to run, nothing resident in memory. The release binary
is ~160 KB and runs in tens of milliseconds.

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
jdrgb presets         list the named color presets
jdrgb load [file]     per-LED colors from a config file (default leds.conf)
jdrgb template [file] write a starter config, one line per LED
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
coolwhite  warmwhite  white  black  red  vermilion  orange  amber  yellow
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
| `vermilion` | `#FF1D00` | `#FF1000` | *interp* |
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

`install.ps1` registers a Scheduled Task that runs as the `SYSTEM` account — so
the color is set during boot, before anyone logs in. It copies the binary to
`C:\Program Files\jdrgb` and uses `--wait` (retry up to ~60s) to tolerate the
USB controller not being enumerated yet.

Triggers, for reliability: **at startup** (the pre-login goal), **at logon** (a
belt-and-suspenders re-apply that survives a late controller reset), and **on
resume from sleep**.

```powershell
# from an elevated PowerShell (the script self-elevates if needed)
.\install.ps1                      # boots to the default (coolwhite)
.\install.ps1 -Color warmwhite     # boot to a preset name (or an RRGGBB hex)
.\install.ps1 -Config leds.conf    # boot to a saved per-LED pattern
.\install.ps1 -All -Color warmwhite  # motherboard strip + GPU
.\install.ps1 -Gpu -Color warmwhite  # GPU only
```

`-Config` copies your file to `C:\Program Files\jdrgb\leds.conf` and points the
boot task at it (`jdrgb load … --wait`). It combines with `-Gpu`/`-All`, since a
config's `gpu:` line can carry the card's colors — if the file has no `gpu:`
line, jdrgb says so rather than failing silently.

It also adds a resume-from-sleep trigger so the color reasserts after waking.

Remove everything with:

```powershell
.\uninstall.ps1
```

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

### Lineage & credits

The motherboard wire protocol was ported from **OpenRGB**'s
`AuraMainboardController` and cross-checked against **liquidctl**'s `aura_led.py`.
The GPU path was ported from OpenRGB's `ENESMBusController` and
`i2c_smbus_nvapi`. All are GPL-2.0-or-later; huge thanks to those projects for the
reverse-engineering that made this possible. Because it derives from that source,
this utility is likewise **GPL-2.0-or-later**.
