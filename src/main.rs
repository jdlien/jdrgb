//! jdrgb — a tiny, single-purpose controller for the ASUS Aura USB LED
//! controller (USB 0B05:19AF) on the ProArt X870E-CREATOR.
//!
//! It speaks the ASUS Aura USB protocol directly over USB-HID. No driver, no
//! daemon, no admin rights: set the LEDs, then exit. Nothing stays resident.
//!
//!   * Solid colors use "effect" (static) mode — the controller latches and
//!     saves the color, so it holds with nothing running.
//!   * Per-LED patterns use "direct" mode — one frame is streamed and latched,
//!     so a *static* pattern also holds after we exit (only animation would need
//!     a resident process re-streaming frames).
//!
//! Protocol ported from OpenRGB's AuraMainboardController (GPL-2.0-or-later) and
//! cross-checked against liquidctl's aura_led.py. See README for lineage.

use std::io::{Read, Write};
use std::process::ExitCode;

use hidapi::{DeviceInfo, HidApi, HidDevice};

// ---- Device -----------------------------------------------------------------
const VENDOR_ID: u16 = 0x0B05; // ASUSTek
const PRODUCT_ID: u16 = 0x19AF; // AURA LED Controller on this board
const STRIP_LEDS: usize = 38; // LEDs on the Phanteks NEON M5 550mm strip

// ---- Protocol ---------------------------------------------------------------
const CMD: u8 = 0xEC; // every Aura packet starts with this (byte 0, no report-id)
const REPORT_LEN: usize = 65; // full HID report length

const REQ_FIRMWARE: u8 = 0x82;
const REQ_CONFIG: u8 = 0xB0;

const CTRL_EFFECT: u8 = 0x35; // select channel + effect mode
const CTRL_EFFECT_COLOR: u8 = 0x36; // effect color
const CTRL_COMMIT: u8 = 0x3F; // latch / save
const CTRL_DIRECT: u8 = 0x40; // per-LED frame

const MODE_OFF: u8 = 0x00;
const MODE_STATIC: u8 = 0x01;
const MODE_DIRECT: u8 = 0xFF;

const LEDS_PER_PACKET: usize = 20; // 20 * 3 bytes = 60, fits one report

/// Default: `coolwhite`, hand-tuned by eye so the strip renders a clean white.
/// (Nominal #FFFFFF reads greenish on this strip; this tuned value looks pink on
/// a screen but renders as a proper cool white, so it's the default.)
const DEFAULT_COLOR: (u8, u8, u8) = (0xFF, 0xB0, 0xD0);

/// Case-insensitive keyword colors. Sensible starting points only — LEDs render
/// colors quite differently from nominal RGB, so tune any that look off by eye.
const PRESETS: &[(&str, (u8, u8, u8))] = &[
    ("coolwhite", (0xFF, 0xB0, 0xD0)), // hand-tuned clean white; the default
    ("warmwhite", (0xFA, 0x95, 0x36)), // the original warm white
    ("white", (0xFF, 0xFF, 0xFF)),     // nominal white (reads greenish here)
    ("red", (0xFF, 0x00, 0x00)),
    ("orange", (0xFF, 0x3A, 0x00)),
    ("amber", (0xFF, 0x87, 0x00)),
    ("yellow", (0xFF, 0xD0, 0x00)),
    ("lime", (0x80, 0xFF, 0x00)),
    ("green", (0x00, 0xFF, 0x00)),
    ("teal", (0x00, 0xFF, 0x80)),
    ("cyan", (0x00, 0xFF, 0xFF)),
    ("blue", (0x00, 0x00, 0xFF)),
    ("azure", (0x00, 0x80, 0xFF)),
    ("purple", (0x80, 0x00, 0xFF)),
    ("magenta", (0xFF, 0x00, 0xFF)),
    ("pink", (0xFF, 0x00, 0x80)),
];

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

enum Command {
    Solid(u8, (u8, u8, u8)), // effect mode + color (MODE_STATIC or MODE_OFF)
    Rainbow(usize),          // per-LED demo across N LEDs
    Load(String),            // per-LED colors from a config file
    Template(String),        // write a starter config file
    Tune((u8, u8, u8)),      // interactively dial in a color
    Presets,                 // list keyword presets
    Probe,                   // diagnostics
    Help,
    Version,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wait = args.iter().any(|a| a == "--wait");
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--wait"))
        .map(String::as_str)
        .collect();

    let command = match parse(&positional) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("jdrgb: {e}\n");
            print_help();
            return ExitCode::FAILURE;
        }
    };

    let result = match command {
        Command::Help => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("jdrgb {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Presets => list_presets(),
        Command::Probe => probe(),
        Command::Solid(mode, color) => with_retry(wait, || set_solid(mode, color)).map(|()| {
            if mode == MODE_OFF {
                println!("jdrgb: LEDs off");
            } else {
                let (r, g, b) = color;
                println!("jdrgb: set solid #{r:02X}{g:02X}{b:02X}");
            }
        }),
        Command::Rainbow(n) => with_retry(wait, || set_rainbow(n)).map(|()| {
            println!("jdrgb: rainbow across {n} LEDs (white end-caps)");
        }),
        Command::Load(path) => {
            with_retry(wait, || set_from_config(&path)).map(|()| println!("jdrgb: loaded {path}"))
        }
        Command::Template(path) => write_template(&path).map(|()| {
            println!("jdrgb: wrote {path} ({STRIP_LEDS} LEDs) — edit it, then `jdrgb load {path}`");
        }),
        Command::Tune(start) => tune(start),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jdrgb: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[&str]) -> Result<Command, String> {
    match args.first().copied().unwrap_or("") {
        "" => Ok(Command::Solid(MODE_STATIC, DEFAULT_COLOR)),
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        "probe" => Ok(Command::Probe),
        "presets" | "colors" => Ok(Command::Presets),
        "off" => Ok(Command::Solid(MODE_OFF, (0, 0, 0))),
        "rainbow" => {
            let n = match args.get(1) {
                Some(s) => s.parse().map_err(|_| format!("invalid LED count '{s}'"))?,
                None => STRIP_LEDS,
            };
            Ok(Command::Rainbow(n))
        }
        "load" => Ok(Command::Load(args.get(1).copied().unwrap_or("leds.conf").to_string())),
        "template" => Ok(Command::Template(args.get(1).copied().unwrap_or("leds.conf").to_string())),
        "tune" => {
            let start = match args.get(1) {
                Some(s) => parse_color(s).ok_or_else(|| format!("'{s}' is not a color or preset"))?,
                // No arg: start from the last solid color we set, else the
                // default (also the fallback when the strip is multi-colored).
                None => load_last().unwrap_or(DEFAULT_COLOR),
            };
            Ok(Command::Tune(start))
        }
        other => parse_color(other)
            .map(|c| Command::Solid(MODE_STATIC, c))
            .ok_or_else(|| format!("'{other}' is not a color, preset, or command (try `jdrgb --help`)")),
    }
}

/// Run `f`, retrying for ~60s when `wait` is set (the controller may not be
/// enumerated yet at boot). Exits the instant it succeeds; otherwise fail fast.
fn with_retry(wait: bool, mut f: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let tries = if wait { 120 } else { 1 };
    let mut last = String::new();
    for attempt in 0..tries {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e;
                if attempt + 1 < tries {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// Device discovery
// ---------------------------------------------------------------------------

/// Open the Aura control interface. The correct HID interface is the one that
/// answers the config request (reply byte 1 == 0x30).
fn open(api: &HidApi) -> Result<HidDevice, String> {
    let candidates: Vec<&DeviceInfo> = api
        .device_list()
        .filter(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
        .collect();

    if candidates.is_empty() {
        return Err(format!("no ASUS Aura controller found (USB {VENDOR_ID:04X}:{PRODUCT_ID:04X})"));
    }

    let mut last = String::from("controller found but no HID interface answered");
    for info in candidates {
        match info.open_device(api) {
            Ok(dev) if read_config(&dev).is_some() => return Ok(dev),
            Ok(_) => {
                last = "opened the controller but it didn't respond \
                    (is Armoury Crate or another RGB app holding it?)"
                    .into()
            }
            Err(e) => last = format!("could not open HID interface: {e}"),
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// Low-level I/O
// ---------------------------------------------------------------------------

/// Write one logical Aura packet (payload[0] must be 0xEC) as a 65-byte report.
fn write(dev: &HidDevice, payload: &[u8]) -> Result<(), String> {
    let mut buf = [0u8; REPORT_LEN];
    buf[..payload.len()].copy_from_slice(payload);
    dev.write(&buf).map_err(|e| format!("HID write failed: {e}"))?;
    Ok(())
}

/// Send a request byte and read the 65-byte reply.
fn request(dev: &HidDevice, req: u8) -> Option<[u8; REPORT_LEN]> {
    write(dev, &[CMD, req]).ok()?;
    let mut buf = [0u8; REPORT_LEN];
    (dev.read_timeout(&mut buf, 500).ok()? >= 2).then_some(buf)
}

/// Read the 60-byte config table (reply id 0x30).
fn read_config(dev: &HidDevice) -> Option<[u8; 60]> {
    let reply = request(dev, REQ_CONFIG)?;
    if reply[1] != 0x30 {
        return None;
    }
    let mut cfg = [0u8; 60];
    cfg.copy_from_slice(&reply[4..64]);
    Some(cfg)
}

// ---------------------------------------------------------------------------
// Effect (solid color) — latched & saved, holds with nothing running
// ---------------------------------------------------------------------------

/// The board's addressable header count, from the config table.
fn header_count(cfg: &[u8; 60]) -> u8 {
    // config[0x1B] = onboard LED count (0 on this board), config[0x02] = headers.
    cfg[0x02]
}

fn set_solid(mode: u8, color: (u8, u8, u8)) -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let dev = open(&api)?;
    let cfg = read_config(&dev).ok_or("could not read config table")?;
    let headers = header_count(&cfg);
    if headers == 0 {
        return Err("config table reported no addressable headers".into());
    }
    apply_solid(&dev, headers, mode, color, true)?;
    save_state(Some(color));
    Ok(())
}

/// Set every header to one color. Each header is one effect "channel" of a
/// single LED-slot; the hardware fills the whole strip. With `commit`, the
/// controller saves it (survives with nothing running); without, it's a live
/// preview only — handy for rapid updates without hammering the flash.
fn apply_solid(dev: &HidDevice, headers: u8, mode: u8, (r, g, b): (u8, u8, u8), commit: bool) -> Result<(), String> {
    write(dev, &[CMD, 0x52, 0x53, 0x00, 0x01])?; // select Gen1 protocol
    for ch in 0..headers {
        write(dev, &[CMD, CTRL_EFFECT, ch, 0x00, 0x00, mode])?; // select channel + mode
        let mask = 1u16 << ch; // one LED-slot per header, at position `ch`
        write(dev, &[CMD, CTRL_EFFECT_COLOR, (mask >> 8) as u8, (mask & 0xFF) as u8, 0x00, r, g, b])?;
    }
    if commit {
        write(dev, &[CMD, CTRL_COMMIT, 0x55])?; // latch + save
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Direct (per-LED) — one latched frame, holds after exit
// ---------------------------------------------------------------------------

/// Stream a full per-LED frame to one header. The channel must be switched into
/// direct mode first, or the controller ignores the frame.
fn send_direct(dev: &HidDevice, channel: u8, colors: &[(u8, u8, u8)]) -> Result<(), String> {
    let led_count = colors.len().min(255);
    if led_count == 0 {
        return Ok(());
    }

    write(dev, &[CMD, CTRL_EFFECT, channel, 0x00, 0x00, MODE_DIRECT])?; // enter direct mode

    let mut offset = 0;
    loop {
        let n = (led_count - offset).min(LEDS_PER_PACKET);
        let apply = offset + n == led_count;

        let mut buf = [0u8; REPORT_LEN];
        buf[0] = CMD;
        buf[1] = CTRL_DIRECT;
        buf[2] = if apply { 0x80 } else { 0x00 } | channel; // 0x80 latches the frame
        buf[3] = offset as u8;
        buf[4] = n as u8;
        for i in 0..n {
            let (r, g, b) = colors[offset + i];
            buf[5 + i * 3] = r;
            buf[6 + i * 3] = g;
            buf[7 + i * 3] = b;
        }
        dev.write(&buf).map_err(|e| format!("HID write failed: {e}"))?;

        offset += n;
        if apply {
            return Ok(());
        }
    }
}

/// White end-caps with a rainbow interior — the per-LED showcase. Written to
/// every header so it lands regardless of which one the strip is on.
fn set_rainbow(count: usize) -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let dev = open(&api)?;
    let cfg = read_config(&dev).ok_or("could not read config table")?;
    let count = count.clamp(2, 255);

    let colors: Vec<(u8, u8, u8)> = (0..count)
        .map(|i| {
            if i == 0 || i == count - 1 {
                (255, 255, 255)
            } else {
                hsv(360.0 * (i - 1) as f32 / (count - 2) as f32)
            }
        })
        .collect();

    for ch in 0..header_count(&cfg) {
        send_direct(&dev, ch, &colors)?;
    }
    save_state(None); // strip is now multi-colored
    Ok(())
}

/// Load per-LED colors from a config file and paint them via direct mode.
/// The strip is padded to its full length with "off" so every LED is defined.
fn set_from_config(path: &str) -> Result<(), String> {
    let mut colors = read_led_config(path)?;
    if colors.len() < STRIP_LEDS {
        colors.resize(STRIP_LEDS, (0, 0, 0));
    }

    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let dev = open(&api)?;
    let cfg = read_config(&dev).ok_or("could not read config table")?;
    for ch in 0..header_count(&cfg) {
        send_direct(&dev, ch, &colors)?;
    }
    save_state(None); // strip is now multi-colored
    Ok(())
}

/// Parse a config file: one `RRGGBB` per line, line N = LED N. `#` starts a
/// comment; blank lines are skipped.
fn read_led_config(path: &str) -> Result<Vec<(u8, u8, u8)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut colors = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let color = parse_hex(line).ok_or_else(|| format!("{path}:{}: invalid color '{line}'", n + 1))?;
        colors.push(color);
    }
    if colors.is_empty() {
        return Err(format!("{path}: no colors found"));
    }
    Ok(colors)
}

/// Write a starter config with one line per LED, pre-filled with the preset.
fn write_template(path: &str) -> Result<(), String> {
    let (r, g, b) = DEFAULT_COLOR;
    let mut out = String::from(
        "# jdrgb per-LED config: one RRGGBB hex color per line, top = LED 0.\n\
         # '#' starts a comment; blank lines are ignored.\n\n",
    );
    for i in 0..STRIP_LEDS {
        out.push_str(&format!("{r:02X}{g:02X}{b:02X}   # LED {i}\n"));
    }
    std::fs::write(path, out).map_err(|e| format!("cannot write {path}: {e}"))
}

/// Fully-saturated, full-value HSV (hue in degrees) to RGB.
fn hsv(h: f32) -> (u8, u8, u8) {
    let x = 1.0 - ((h / 60.0) % 2.0 - 1.0).abs();
    let (r, g, b) = match (h as u32 / 60) % 6 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    ((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

// ---------------------------------------------------------------------------
// Interactive tuner
// ---------------------------------------------------------------------------

const HUE_STEP: f32 = 1.0; // degrees per keypress (hold a key to ramp)
const SL_STEP: f32 = 0.01; // saturation/lightness per keypress (1%)

/// Dial in a color live on the strip with single keypresses, in HSL.
fn tune(start: (u8, u8, u8)) -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let dev = open(&api)?;
    let cfg = read_config(&dev).ok_or("could not read config table")?;
    let headers = header_count(&cfg);
    if headers == 0 {
        return Err("config table reported no addressable headers".into());
    }

    let (mut h, mut s, mut l) = rgb_to_hsl(start);

    // Enable the console (and ANSI) before printing so the colored intro renders.
    let raw = RawMode::enable();
    let pal = Palette::new(raw.color);
    let (k, r) = (pal.key, pal.reset);

    println!("jdrgb tune - dial in a color, live on the strip.");
    println!("  {k}h/H{r} hue    {k}s/S{r} sat    {k}l/L{r} light    {k}q{r} quit");
    println!();

    let mut stdin = std::io::stdin();
    let mut key = [0u8; 1];

    let mut rgb = hsl_to_rgb(h, s, l);
    apply_solid(&dev, headers, MODE_STATIC, rgb, false)?; // live preview, no flash-save
    draw_status(h, s, l, rgb, &pal);

    loop {
        if stdin.read(&mut key).unwrap_or(0) == 0 {
            break; // EOF
        }
        match key[0] {
            b'q' | 3 => break, // q or Ctrl+C
            b'h' => h = (h - HUE_STEP).rem_euclid(360.0),
            b'H' => h = (h + HUE_STEP).rem_euclid(360.0),
            b's' => s = (s - SL_STEP).max(0.0),
            b'S' => s = (s + SL_STEP).min(1.0),
            b'l' => l = (l - SL_STEP).max(0.0),
            b'L' => l = (l + SL_STEP).min(1.0),
            _ => continue,
        }
        rgb = hsl_to_rgb(h, s, l);
        apply_solid(&dev, headers, MODE_STATIC, rgb, false)?;
        draw_status(h, s, l, rgb, &pal);
    }

    apply_solid(&dev, headers, MODE_STATIC, rgb, true)?; // commit the chosen color
    save_state(Some(rgb));
    let (cr, cg, cb) = rgb;
    println!();
    println!("jdrgb: kept {}#{cr:02X}{cg:02X}{cb:02X}{}", pal.value, pal.reset);
    Ok(())
}

/// Terminal color codes, or empty strings when output isn't a console.
/// Convention (à la well-behaved CLIs): cyan hotkeys, yellow labels,
/// bold-white values.
struct Palette {
    enabled: bool,
    reset: &'static str,
    value: &'static str,
    label: &'static str,
    key: &'static str,
}

impl Palette {
    fn new(color: bool) -> Self {
        if color {
            Palette {
                enabled: true,
                reset: "\x1b[0m",
                value: "\x1b[1;97m", // bold bright white
                label: "\x1b[33m",   // yellow
                key: "\x1b[36m",     // cyan
            }
        } else {
            Palette { enabled: false, reset: "", value: "", label: "", key: "" }
        }
    }
}

fn draw_status(h: f32, s: f32, l: f32, (r, g, b): (u8, u8, u8), pal: &Palette) {
    // Bracket-framed swatch: the frame keeps its extent visible even for a
    // near-black color on a dark terminal, so it never blends into nothing.
    let swatch = if pal.enabled {
        format!("[\x1b[48;2;{r};{g};{b}m     \x1b[0m]  ")
    } else {
        String::new()
    };
    let (lab, val, rst) = (pal.label, pal.value, pal.reset);
    print!(
        "\r  {swatch}{lab}H{rst} {val}{h:5.1}{rst}   {lab}S{rst} {val}{:3.0}%{rst}   \
         {lab}L{rst} {val}{:3.0}%{rst}   {lab}rgb{rst} {val}({r:3},{g:3},{b:3}){rst}   \
         {val}#{r:02X}{g:02X}{b:02X}{rst}    ",
        s * 100.0,
        l * 100.0,
    );
    let _ = std::io::stdout().flush();
}

/// RAII guard: put the console input into raw (unbuffered, no-echo) mode and
/// enable ANSI/truecolor output, restoring both on drop. `color` reports
/// whether ANSI output is available (false when piped / not a console).
struct RawMode {
    in_handle: windows_sys::Win32::Foundation::HANDLE,
    in_prev: u32,
    out_handle: windows_sys::Win32::Foundation::HANDLE,
    out_prev: u32,
    active: bool,
    color: bool,
}

impl RawMode {
    fn enable() -> Self {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE,
        };
        unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let (mut in_prev, mut out_prev) = (0u32, 0u32);

            let active = GetConsoleMode(in_handle, &mut in_prev) != 0;
            if active {
                SetConsoleMode(in_handle, in_prev & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT));
            }

            let color = GetConsoleMode(out_handle, &mut out_prev) != 0;
            if color {
                SetConsoleMode(out_handle, out_prev | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }

            RawMode { in_handle, in_prev, out_handle, out_prev, active, color }
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        unsafe {
            if self.active {
                SetConsoleMode(self.in_handle, self.in_prev);
            }
            if self.color {
                SetConsoleMode(self.out_handle, self.out_prev);
            }
        }
    }
}

/// RGB to HSL (h in degrees 0..360, s/l in 0..1).
fn rgb_to_hsl((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < f32::EPSILON {
        return (0.0, 0.0, l); // gray
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == rf {
        60.0 * ((gf - bf) / d).rem_euclid(6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / d + 2.0)
    } else {
        60.0 * ((rf - gf) / d + 4.0)
    };
    (h.rem_euclid(360.0), s, l)
}

/// HSL (h in degrees, s/l in 0..1) to RGB.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let hp = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn probe() -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let candidates: Vec<&DeviceInfo> = api
        .device_list()
        .filter(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
        .collect();
    if candidates.is_empty() {
        return Err(format!("no device {VENDOR_ID:04X}:{PRODUCT_ID:04X} found"));
    }

    for info in candidates {
        println!(
            "interface {} (usage_page={:#06x} usage={:#06x})",
            info.interface_number(),
            info.usage_page(),
            info.usage()
        );
        let dev = match info.open_device(&api) {
            Ok(d) => d,
            Err(e) => {
                println!("  could not open: {e}\n");
                continue;
            }
        };
        match request(&dev, REQ_FIRMWARE) {
            Some(r) => {
                let fw: String = r[2..17].iter().filter(|&&c| c.is_ascii_graphic()).map(|&c| c as char).collect();
                println!("  firmware: {fw}");
            }
            None => println!("  no firmware reply"),
        }
        match read_config(&dev) {
            Some(cfg) => println!(
                "  addressable headers: {}   onboard LEDs: {}",
                header_count(&cfg),
                cfg[0x1B]
            ),
            None => println!("  no config reply (not the control interface, or device busy)"),
        }
        println!();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
    ))
}

/// Resolve a color from a case-insensitive preset name or an RRGGBB hex string.
fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    let lower = s.to_ascii_lowercase();
    PRESETS
        .iter()
        .find(|(name, _)| *name == lower)
        .map(|&(_, rgb)| rgb)
        .or_else(|| parse_hex(s))
}

/// Path to the "last color" state file in the user's local app data.
fn state_path() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| std::path::PathBuf::from(p).join("jdrgb").join("last"))
}

/// Remember the last solid color set, or `None` to mark the strip multi-colored
/// (so `tune` falls back to the default). Best-effort; failures are ignored.
fn save_state(color: Option<(u8, u8, u8)>) {
    if let Some(path) = state_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let content = match color {
            Some((r, g, b)) => format!("{r:02X}{g:02X}{b:02X}"),
            None => "multi".to_string(),
        };
        let _ = std::fs::write(path, content);
    }
}

/// The last solid color we set, if the strip isn't multi-colored.
fn load_last() -> Option<(u8, u8, u8)> {
    parse_hex(std::fs::read_to_string(state_path()?).ok()?.trim())
}

/// Print the keyword presets, with a swatch when the terminal supports color.
fn list_presets() -> Result<(), String> {
    let color = enable_ansi_output();
    println!("Presets (case-insensitive) - tune any that render off by eye:");
    for &(name, (r, g, b)) in PRESETS {
        if color {
            println!("  [\x1b[48;2;{r};{g};{b}m    \x1b[0m]  \x1b[1;97m{name:<10}\x1b[0m \x1b[33m#{r:02X}{g:02X}{b:02X}\x1b[0m");
        } else {
            println!("  {name:<10} #{r:02X}{g:02X}{b:02X}");
        }
    }
    Ok(())
}

/// Enable ANSI/VT output on the console; returns whether color is available.
fn enable_ansi_output() -> bool {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            true
        } else {
            false
        }
    }
}

fn print_help() {
    let (r, g, b) = DEFAULT_COLOR;
    println!(
        "jdrgb {ver} — set the ASUS Aura LEDs, then exit.\n\
         \n\
         USAGE:\n\
         \x20 jdrgb                 default color, coolwhite (#{r:02X}{g:02X}{b:02X})\n\
         \x20 jdrgb NAME            a named preset, e.g. jdrgb red  (`jdrgb presets` lists them)\n\
         \x20 jdrgb RRGGBB          a hex color, e.g. jdrgb ffcf9e\n\
         \x20 jdrgb off             turn the LEDs off\n\
         \x20 jdrgb presets         list the named color presets\n\
         \x20 jdrgb load [file]     per-LED colors from a config file (default leds.conf)\n\
         \x20 jdrgb template [file] write a starter config, one line per LED\n\
         \x20 jdrgb rainbow [n]     per-LED rainbow across n LEDs (default {STRIP_LEDS})\n\
         \x20 jdrgb tune [color]    dial in a color live (from a preset/hex, or the last set)\n\
         \x20 jdrgb probe           show firmware + config (diagnostics)\n\
         \x20 jdrgb --version       print version\n\
         \x20 jdrgb --help          this message\n\
         \n\
         FLAGS:\n\
         \x20 --wait                retry ~60s until the controller is ready (use at boot)\n",
        ver = env!("CARGO_PKG_VERSION"),
    );
}
