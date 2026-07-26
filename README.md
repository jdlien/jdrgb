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
jdrgb tune [color]    dial in a color live (preset/hex, or the last-set color)
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
it always has. `rainbow`, `load`, and `template` are motherboard-only — the GPU
zone is 4 LEDs, so a 38-LED per-LED pattern there is meaningless, and passing
`--gpu` with them is an error rather than a partial apply.

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
coolwhite  warmwhite  white  red  orange  amber  yellow  chartreuse  lime
green  seagreen  teal  cyan  azure  cobalt  blue  indigo  purple  violet  magenta  hotpink  pink
```

#### Per-device calibration

The GPU renders colors quite differently from the strip — the same nominal value
can look nothing alike on the two.

This card overdrives blue badly: nominal `#FFFFFF` reads as sky blue, close to
`azure`. Both calibrated whites land at roughly a quarter of the strip's blue, so
expect to pull blue a long way down when tuning a new one.

| preset | strip | GPU |
|---|---|---|
| `coolwhite` | `#FFB0D0` | `#C79E38` |
| `warmwhite` | `#FA9536` | `#FF8512` |

So a **preset name resolves per device**, while an **explicit hex is always
literal**. That split is what keeps `tune` honest: the hex it prints reproduces
exactly what you were looking at, on whichever device you were looking at.

```powershell
jdrgb --all warmwhite   # strip #FA9536, GPU #FF8512 — same look, different values
jdrgb --gpu FA9536      # literal: no substitution
```

The override table (`GPU_PRESETS` in `src/main.rs`) is deliberately sparse. Only
presets actually dialled in by eye on the GPU belong there; everything else falls
back to the shared value, so the table never claims a calibration nobody has
looked at. `jdrgb presets` marks the calibrated ones with a `gpu` column.

To add one, tune it and paste the hex it prints:

```powershell
jdrgb --gpu tune amber      # dial by eye, q to quit
```

A test asserts every name in `GPU_PRESETS` matches a real preset, so a typo fails
the build instead of silently doing nothing.

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

`jdrgb tune [color]` steps a color live on the strip in HSL — `h`/`s`/`l` nudge
each channel down, `H`/`S`/`L` up (hold a key to ramp). It shows the current HSL,
RGB, and hex in a compact status line (cyan keys, yellow labels, bold-white
values, plus a live swatch), and `q` quits keeping the color and printing its
hex. Live steps skip the flash-save; the final pick is committed. With no
argument it starts from the last solid color set (remembered in a small state
file under `%LOCALAPPDATA%\jdrgb`), or the default if the strip was left
multi-colored by `rainbow`/`load`.

To change the built-in default, edit `DEFAULT_COLOR` in `src/main.rs`.

Solid colors are set via the controller's **static effect** mode, which the
hardware latches *and saves* — the color holds with nothing running. The per-LED
`rainbow` uses **direct** mode, streaming one frame that the controller also
latches; it's a static frame, not an animation, so it likewise holds after the
program exits.

### GPU persistence

The GPU is different. Its ENE controller applies a color to volatile registers,
so it reverts to the firmware's rainbow when the card loses power — which is why
Armoury Crate had to reassert it at every boot. The boot task below does the same
job without the bloat.

The controller *does* have a save-to-flash command, exposed as `jdrgb --gpu save`.
It's deliberately a separate, manual command and is never part of the boot task:
flash has finite write cycles, so committing on every startup would be a bad
trade. Run it once by hand, then test with a full power-off at the PSU — a warm
reboot may not drop power to the slot.

## Run at boot (no login required)

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
boot task at it (`jdrgb load … --wait`). It's motherboard-only, so combining it
with `-Gpu`/`-All` is rejected rather than installing a task that would fail on
every boot.

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
