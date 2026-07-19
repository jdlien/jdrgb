# jdrgb

A tiny, single-purpose CLI that sets the LEDs on an **ASUS ProArt X870E-CREATOR**
motherboard to a static color and exits. No driver, no daemon, no admin rights
to run, nothing resident in memory. The release binary is ~160 KB and runs in
tens of milliseconds.

Built because all I want is "warm white, always" instead of the firmware's
default pulsing rainbow — and OpenRGB, while it works, is far more than that
needs.

## Hardware

This targets exactly one setup and makes no attempt to be general:

- **Motherboard:** ASUS ProArt X870E-CREATOR WIFI
- **Controller:** ASUS AURA LED Controller, USB `0B05:19AF` (HID interface 2)
- **Strip:** Phanteks NEON Digital-RGB M5 550mm (38 addressable LEDs)

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
jdrgb --version
jdrgb --help

  --wait              retry ~60s until the controller is ready (used at boot)
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
```

`-Config` copies your file to `C:\Program Files\jdrgb\leds.conf` and points the
boot task at it (`jdrgb load … --wait`).

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

### Lineage & credits

The wire protocol was ported from **OpenRGB**'s `AuraMainboardController` and
cross-checked against **liquidctl**'s `aura_led.py`. Both are GPL-2.0-or-later;
huge thanks to those projects for the reverse-engineering that made this
possible. Because it derives from that source, this utility is likewise
**GPL-2.0-or-later**.
