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

mod gpu;

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

/// The default as a preset name, so each device gets its own tuned value if one
/// is listed in GPU_PRESETS.
const DEFAULT_PRESET: &str = "coolwhite";

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
    ("chartreuse", (0xD4, 0xFF, 0x00)),
    ("lime", (0x80, 0xFF, 0x00)),
    ("green", (0x00, 0xFF, 0x00)),
    ("seagreen", (0x00, 0xFF, 0x51)),
    ("teal", (0x00, 0xFF, 0x80)),
    ("cyan", (0x00, 0xFF, 0xFF)),
    ("azure", (0x00, 0x80, 0xFF)),  // bright sky blue
    ("cobalt", (0x00, 0x40, 0xFF)), // mid blue, bridges azure->blue in hue + brightness
    ("blue", (0x00, 0x00, 0xFF)),
    ("indigo", (0x40, 0x00, 0xFF)),
    ("purple", (0x80, 0x00, 0xFF)),
    ("violet", (0xBF, 0x00, 0xFF)),
    ("magenta", (0xFF, 0x00, 0xFF)),
    ("hotpink", (0xFF, 0x00, 0x80)), // intense synthwave/Barbie pink
    ("pink", (0xD5, 0x2A, 0x66)),    // softer, "pretty in pink"
];

/// Per-preset overrides for the GPU, which renders colors quite differently from
/// the strip — the same nominal value can look nothing alike on the two.
///
/// Deliberately sparse: only presets actually dialled in by eye on the GPU belong
/// here. Anything absent falls back to the shared value in PRESETS above, so this
/// table never claims a calibration that hasn't been eyeballed. Add entries with
/// `jdrgb --gpu tune NAME` and paste the hex it prints.
const GPU_PRESETS: &[(&str, (u8, u8, u8))] = &[
    ("warmwhite", (0xFF, 0x85, 0x12)), // matches the strip's FA9536 by eye
];

/// A color on its way to a device.
///
/// Preset names stay unresolved until the moment of writing, because the right
/// RGB depends on which controller it lands on. An explicit hex is always taken
/// literally — if you type a value, you get that value, which is what makes
/// `tune`'s output directly reusable.
#[derive(Clone, Copy)]
enum Paint {
    Literal((u8, u8, u8)),
    Preset(&'static str),
}

impl Paint {
    fn rgb(self, gpu: bool) -> (u8, u8, u8) {
        match self {
            Paint::Literal(rgb) => rgb,
            Paint::Preset(name) => {
                if gpu {
                    if let Some(&(_, rgb)) = GPU_PRESETS.iter().find(|(n, _)| *n == name) {
                        return rgb;
                    }
                }
                lookup_preset(name).unwrap_or(DEFAULT_COLOR)
            }
        }
    }
}

fn lookup_preset(name: &str) -> Option<(u8, u8, u8)> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|&(_, rgb)| rgb)
}

/// Resolve a preset name (case-insensitive) or an `RRGGBB` hex string.
fn parse_paint(s: &str) -> Option<Paint> {
    let lower = s.to_ascii_lowercase();
    if let Some(&(name, _)) = PRESETS.iter().find(|(n, _)| *n == lower) {
        return Some(Paint::Preset(name));
    }
    parse_hex(s).map(Paint::Literal)
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

enum Command {
    Solid(u8, Paint), // effect mode + color (MODE_STATIC or MODE_OFF)
    Rainbow(usize),          // per-LED demo across N LEDs
    Load(String),            // per-LED colors from a config file
    Template(String),        // write a starter config file
    Tune(Paint),             // interactively dial in a color
    Preview,                 // cycle through all presets
    Presets,                 // list keyword presets
    Probe,                   // diagnostics
    Save,                    // commit the GPU's current color to its flash
    Help,
    Version,
}

/// Which LED controller(s) a command acts on. Defaults to the motherboard so
/// every existing invocation behaves exactly as it always has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Mb,
    Gpu,
    All,
}

impl Target {
    fn mb(self) -> bool {
        self != Target::Gpu
    }
    fn gpu(self) -> bool {
        self != Target::Mb
    }
}

/// Commands that only make sense on the motherboard's addressable strip.
/// The GPU zone is a handful of LEDs, so a per-LED config or a 38-LED rainbow
/// would be meaningless there — better to say so than to half-apply it.
fn mb_only(command: &Command) -> Option<&'static str> {
    match command {
        Command::Rainbow(_) => Some("rainbow"),
        Command::Load(_) => Some("load"),
        Command::Template(_) => Some("template"),
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wait = args.iter().any(|a| a == "--wait");

    let target = match (args.iter().any(|a| a == "--gpu"), args.iter().any(|a| a == "--all")) {
        (true, true) => {
            eprintln!("jdrgb: --gpu and --all are mutually exclusive\n");
            print_help();
            return ExitCode::FAILURE;
        }
        (true, false) => Target::Gpu,
        (false, true) => Target::All,
        (false, false) => Target::Mb,
    };

    // Filter the known flags by name — a blanket "--" filter would swallow
    // --help and --version, which parse() handles as positional words.
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "--wait" | "--gpu" | "--all"))
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

    if let (Some(name), false) = (mb_only(&command), target == Target::Mb) {
        eprintln!("jdrgb: `{name}` is motherboard-only — drop --gpu/--all");
        return ExitCode::FAILURE;
    }

    let result = match command {
        Command::Help => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            println!("jdrgb {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Preview => preview(target),
        Command::Presets => list_presets(),
        Command::Probe => probe(target),
        Command::Save => save_gpu_flash(target),
        Command::Solid(mode, color) => set_solid_targets(target, wait, mode, color),
        Command::Rainbow(n) => with_retry(wait, || set_rainbow(n)).map(|()| {
            println!("jdrgb: rainbow across {n} LEDs (white end-caps)");
        }),
        Command::Load(path) => {
            with_retry(wait, || set_from_config(&path)).map(|()| println!("jdrgb: loaded {path}"))
        }
        Command::Template(path) => write_template(&path).map(|()| {
            println!("jdrgb: wrote {path} ({STRIP_LEDS} LEDs) — edit it, then `jdrgb load {path}`");
        }),
        // Tuning needs one concrete starting RGB even with --all, so a preset
        // resolves against whichever device is the focus: the GPU only when
        // it's the sole target.
        Command::Tune(start) => tune(target, start.rgb(target == Target::Gpu)),
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
        "" => Ok(Command::Solid(MODE_STATIC, Paint::Preset(DEFAULT_PRESET))),
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        "probe" => Ok(Command::Probe),
        "save" => Ok(Command::Save),
        "preview" => Ok(Command::Preview),
        "presets" | "colors" => Ok(Command::Presets),
        "off" => Ok(Command::Solid(MODE_OFF, Paint::Literal((0, 0, 0)))),
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
                Some(s) => parse_paint(s).ok_or_else(|| format!("'{s}' is not a color or preset"))?,
                // No arg: start from the last solid color we set, else the
                // default (also the fallback when the strip is multi-colored).
                None => Paint::Literal(load_last().unwrap_or(DEFAULT_COLOR)),
            };
            Ok(Command::Tune(start))
        }
        other => parse_paint(other)
            .map(|p| Command::Solid(MODE_STATIC, p))
            .ok_or_else(|| format!("'{other}' is not a color, preset, or command (try `jdrgb --help`)")),
    }
}

/// An error that knows whether retrying it could ever help.
trait Retryable {
    fn is_permanent(&self) -> bool;
    fn into_message(self) -> String;
}

/// Motherboard failures are all readiness problems — the controller may simply
/// not be enumerated yet at boot — so they're always worth retrying.
impl Retryable for String {
    fn is_permanent(&self) -> bool {
        false
    }
    fn into_message(self) -> String {
        self
    }
}

impl Retryable for gpu::Error {
    fn is_permanent(&self) -> bool {
        self.permanent
    }
    fn into_message(self) -> String {
        self.msg
    }
}

/// Run `f`, retrying for ~60s when `wait` is set (a controller may not be
/// enumerated yet at boot). Exits the instant it succeeds. Permanent failures —
/// wrong card, bad ENE signature, missing NVAPI interface — abort immediately
/// rather than burning a minute on something that can't come right.
fn with_retry<T, E: Retryable>(wait: bool, mut f: impl FnMut() -> Result<T, E>) -> Result<T, String> {
    let tries = if wait { 120 } else { 1 };
    let mut last = String::new();
    for attempt in 0..tries {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if e.is_permanent() {
                    return Err(e.into_message());
                }
                last = e.into_message();
                if attempt + 1 < tries {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    }
    Err(last)
}

fn transient(msg: impl Into<String>) -> gpu::Error {
    gpu::Error { msg: msg.into(), permanent: false }
}

// ---------------------------------------------------------------------------
// Multi-target dispatch
// ---------------------------------------------------------------------------

/// Confirm every requested target is present and usable. Strictly read-only —
/// this is phase 1 of the two-phase apply below.
fn check_targets(target: Target) -> Result<(), gpu::Error> {
    if target.mb() {
        let api = HidApi::new().map_err(|e| transient(format!("hidapi init failed: {e}")))?;
        let dev = open(&api).map_err(transient)?;
        let cfg = read_config(&dev).ok_or_else(|| transient("could not read config table"))?;
        if header_count(&cfg) == 0 {
            return Err(transient("config table reported no addressable headers"));
        }
    }
    if target.gpu() {
        gpu::detect()?.prepare()?;
    }
    Ok(())
}

/// Set a solid color on every requested target.
///
/// The two phases matter: discovery is retried until everything is ready, then
/// each target is written exactly once. Retrying the *write* instead would
/// re-issue the motherboard's flash commit on every attempt while waiting for
/// the GPU to come up — hammering flash that has finite write cycles.
fn set_solid_targets(target: Target, wait: bool, mode: u8, paint: Paint) -> Result<(), String> {
    with_retry(wait, || check_targets(target))?;

    // Label each line only when there's more than one device to tell apart, so
    // plain `jdrgb warmwhite` prints exactly what it always has.
    let labelled = target == Target::All;

    if target.mb() {
        let color = paint.rgb(false);
        set_solid(mode, color)?;
        report(labelled.then_some("mb"), mode, color);
    }
    if target.gpu() {
        let color = paint.rgb(true);
        let mut card = gpu::detect().map_err(|e| e.msg)?;
        card.prepare().map_err(|e| e.msg)?;
        card.apply_solid(mode, color).map_err(|e| e.msg)?;
        report(labelled.then_some("gpu"), mode, color);
    }
    Ok(())
}

fn report(label: Option<&str>, mode: u8, (r, g, b): (u8, u8, u8)) {
    let prefix = match label {
        Some(l) => format!("jdrgb: {l}: "),
        None => "jdrgb: ".to_string(),
    };
    if mode == MODE_OFF {
        println!("{prefix}LEDs off");
    } else {
        println!("{prefix}set solid #{r:02X}{g:02X}{b:02X}");
    }
}

/// Commit the GPU controller's current color to its non-volatile flash.
///
/// Never called automatically and never part of the boot task: flash wears out,
/// and this is the one operation here that isn't trivially reversible.
fn save_gpu_flash(target: Target) -> Result<(), String> {
    if !target.gpu() {
        return Err("`save` applies to the GPU — use `jdrgb --gpu save`".into());
    }
    let mut card = gpu::detect().map_err(|e| e.msg)?;
    card.prepare().map_err(|e| e.msg)?;

    println!("jdrgb: committing the GPU's current color to controller flash.");
    println!("       This writes non-volatile memory — do it once, not on a schedule.");
    card.save_to_flash().map_err(|e| e.msg)?;
    println!("jdrgb: {}: saved.", card.label());
    println!("       To test it: full power-off at the PSU (a warm reboot may not drop slot power).");
    Ok(())
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

/// Everything a live command (tune, preview) paints to, opened once up front.
struct Live<'a> {
    mb: Option<(HidDevice, u8)>, // device + addressable header count
    gpu: Option<gpu::Gpu>,
    _api: &'a HidApi,
}

impl Live<'_> {
    fn open(api: &HidApi, target: Target) -> Result<Live<'_>, String> {
        let mb = if target.mb() {
            let dev = open(api)?;
            let cfg = read_config(&dev).ok_or("could not read config table")?;
            let headers = header_count(&cfg);
            if headers == 0 {
                return Err("config table reported no addressable headers".into());
            }
            Some((dev, headers))
        } else {
            None
        };
        let gpu = if target.gpu() {
            let mut card = gpu::detect().map_err(|e| e.msg)?;
            card.prepare().map_err(|e| e.msg)?;
            Some(card)
        } else {
            None
        };
        Ok(Live { mb, gpu, _api: api })
    }

    /// Paint everywhere, resolving the color separately per device so a preset
    /// lands as whatever looks right on each. `commit` only means anything on
    /// the motherboard; the GPU's equivalent is the separate `save` command, so
    /// live stepping never touches its flash.
    fn paint(&self, paint: Paint, commit: bool) -> Result<(), String> {
        if let Some((dev, headers)) = &self.mb {
            apply_solid(dev, *headers, MODE_STATIC, paint.rgb(false), commit)?;
        }
        if let Some(card) = &self.gpu {
            card.apply_solid(MODE_STATIC, paint.rgb(true)).map_err(|e| e.msg)?;
        }
        Ok(())
    }
}

/// Dial in a color live on the strip with single keypresses, in HSL.
fn tune(target: Target, start: (u8, u8, u8)) -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let live = Live::open(&api, target)?;

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
    // Tuning is always literal: you're dialling in a specific value, so no
    // per-device preset substitution happens here — what you see is what the
    // printed hex will reproduce.
    live.paint(Paint::Literal(rgb), false)?; // live preview, no flash-save
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
        live.paint(Paint::Literal(rgb), false)?;
        draw_status(h, s, l, rgb, &pal);
    }

    live.paint(Paint::Literal(rgb), true)?; // commit the chosen color
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
// Preview (slideshow of all presets)
// ---------------------------------------------------------------------------

fn preview(target: Target) -> Result<(), String> {
    let api = HidApi::new().map_err(|e| format!("hidapi init failed: {e}"))?;
    let live = Live::open(&api, target)?;

    let raw = RawMode::enable();
    let pal = Palette::new(raw.color);
    let (k, r) = (pal.key, pal.reset);
    println!("jdrgb preview - cycling {} presets, live on the strip.", PRESETS.len());
    println!("  {k}+{r} faster    {k}-{r} slower    {k}n{r}/{k}N{r} next/prev    {k}s{r} stop    {k}q{r} quit");
    println!();

    let total = PRESETS.len();
    let tick_ms = 50u64;
    let mut dwell_ms = 4000u64;
    let mut idx = 0usize;
    let mut elapsed = 0u64;
    let mut paused = false;

    // Live preview (no flash-save) while cycling.
    live.paint(Paint::Preset(PRESETS[idx].0), false)?;
    draw_preview(idx, total, dwell_ms, paused, &pal);

    let mut quit = false;
    while !quit {
        let mut refresh = false;
        while let Some(c) = poll_key(raw.in_handle) {
            match c {
                'q' | '\u{3}' => quit = true, // q or Ctrl+C
                's' | 'S' => {
                    paused = !paused;
                    refresh = true;
                }
                '+' | '=' => {
                    dwell_ms = dwell_ms.saturating_sub(500).max(500);
                    refresh = true;
                }
                '-' | '_' => {
                    dwell_ms = (dwell_ms + 500).min(20_000);
                    refresh = true;
                }
                'n' | ' ' => {
                    idx = (idx + 1) % total;
                    live.paint(Paint::Preset(PRESETS[idx].0), false)?;
                    elapsed = 0;
                    refresh = true;
                }
                'N' => {
                    idx = (idx + total - 1) % total;
                    live.paint(Paint::Preset(PRESETS[idx].0), false)?;
                    elapsed = 0;
                    refresh = true;
                }
                _ => {}
            }
        }
        if quit {
            break;
        }
        if refresh {
            draw_preview(idx, total, dwell_ms, paused, &pal);
        }

        std::thread::sleep(std::time::Duration::from_millis(tick_ms));
        if !paused {
            elapsed += tick_ms;
            if elapsed >= dwell_ms {
                idx = (idx + 1) % total;
                live.paint(Paint::Preset(PRESETS[idx].0), false)?;
                elapsed = 0;
                draw_preview(idx, total, dwell_ms, paused, &pal);
            }
        }
    }

    // Keep whatever's showing when you quit: commit and remember it.
    let (name, rgb) = PRESETS[idx];
    live.paint(Paint::Preset(name), true)?;
    save_state(Some(rgb));
    let (cr, cg, cb) = rgb;
    println!();
    println!("jdrgb: kept {name} {}#{cr:02X}{cg:02X}{cb:02X}{}", pal.value, pal.reset);
    Ok(())
}

fn draw_preview(idx: usize, total: usize, dwell_ms: u64, paused: bool, pal: &Palette) {
    let (name, (r, g, b)) = PRESETS[idx];
    let swatch = if pal.enabled {
        format!("[\x1b[48;2;{r};{g};{b}m     \x1b[0m]  ")
    } else {
        String::new()
    };
    let (lab, val, rst) = (pal.label, pal.value, pal.reset);
    let secs = dwell_ms as f64 / 1000.0;
    // Fixed width ("  [paused]" vs 10 spaces) so the line never leaves residue.
    let state = if paused {
        format!("  {}[paused]{rst}", pal.key)
    } else {
        "          ".to_string()
    };
    print!(
        "\r  {swatch}{val}{name:<10}{rst} {lab}#{r:02X}{g:02X}{b:02X}{rst}   {lab}({}/{}){rst}   {lab}dwell{rst} {val}{secs:.1}s{rst}{state}    ",
        idx + 1,
        total,
    );
    let _ = std::io::stdout().flush();
}

/// Non-blocking: return the next key-down character if one is queued, else None.
fn poll_key(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<char> {
    use windows_sys::Win32::System::Console::{
        GetNumberOfConsoleInputEvents, ReadConsoleInputW, INPUT_RECORD, KEY_EVENT,
    };
    unsafe {
        let mut pending = 0u32;
        if GetNumberOfConsoleInputEvents(handle, &mut pending) == 0 || pending == 0 {
            return None;
        }
        for _ in 0..pending {
            let mut rec: INPUT_RECORD = std::mem::zeroed();
            let mut read = 0u32;
            if ReadConsoleInputW(handle, &mut rec, 1, &mut read) == 0 || read == 0 {
                break;
            }
            if rec.EventType == KEY_EVENT as u16 {
                let ev = rec.Event.KeyEvent;
                if ev.bKeyDown != 0 && ev.uChar.UnicodeChar != 0 {
                    return char::from_u32(ev.uChar.UnicodeChar as u32);
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn probe(target: Target) -> Result<(), String> {
    if target.mb() {
        probe_mb()?;
    }
    if target.gpu() {
        probe_gpu()?;
    }
    Ok(())
}

/// Read-only GPU diagnostics: everything needed to confirm the NVAPI path works
/// and the ENE controller is really where we think it is, without writing a
/// single LED register.
fn probe_gpu() -> Result<(), String> {
    let mut card = gpu::detect().map_err(|e| e.msg)?;
    let ids = card.ids;

    println!("GPU {:04X}:{:04X} subsystem {:04X}:{:04X}", ids.vendor, ids.device, ids.sub_vendor, ids.sub_device);
    println!("  NVAPI:          ok (I2C reachable{})", if is_elevated() { ", elevated" } else { ", unelevated" });

    // Show what's actually at 0x67 rather than just pass/fail — if this card
    // doesn't match, the raw bytes are the whole diagnosis.
    let sig = card.signature().map_err(|e| e.msg)?;
    let hex: Vec<String> = sig.iter().map(|b| format!("{b:02X}")).collect();
    let ok = gpu::Gpu::signature_ok(&sig);
    println!("  A0..AF:         {}", hex.join(" "));
    println!("  ENE signature:  {}", if ok { "ok" } else { "MISMATCH (expected 00 01 02 .. 0F)" });

    if !ok {
        println!();
        println!("  No ENE controller is answering at 0x67 on I2C port 1.");
        return Ok(());
    }

    card.read_identity().map_err(|e| e.msg)?;
    println!("  ENE firmware:   {:?}", card.ene_name);
    println!("  LED count:      {} (config table offset 0x03)", card.raw_led_count);
    println!("  config table:");
    for (row, chunk) in card.config_table().chunks(16).enumerate() {
        let bytes: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        println!("    {:04X}  {}", 0x1C00 + row * 16, bytes.join(" "));
    }

    match card.validate() {
        Ok(n) => println!("  writable:       yes ({n} LEDs)"),
        Err(e) => {
            println!("  writable:       NO — {}", e.msg);
            println!("  If the firmware string above is what this card reports, set");
            println!("  EXPECTED_ENE_NAME in src/gpu.rs to it and rebuild.");
        }
    }
    println!();
    Ok(())
}

fn is_elevated() -> bool {
    // Best-effort: probe output is diagnostic, so a wrong answer here is
    // cosmetic. Writing to HKLM is the cheap proxy without extra imports.
    std::fs::metadata("C:\\Windows\\System32\\config\\SAM")
        .and_then(|_| std::fs::File::open("C:\\Windows\\System32\\config\\SAM"))
        .is_ok()
}

fn probe_mb() -> Result<(), String> {
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
/// Presets carrying a separate GPU calibration show it in a second column.
fn list_presets() -> Result<(), String> {
    let color = enable_ansi_output();
    println!("Presets (case-insensitive) - tune any that render off by eye:");
    for &(name, (r, g, b)) in PRESETS {
        // Only presets dialled in on the GPU get a second column; the rest fall
        // back to the strip's value, and saying so would be noise.
        let gpu = GPU_PRESETS.iter().find(|(n, _)| *n == name).map(|&(_, rgb)| rgb);
        if color {
            let extra = match gpu {
                Some((gr, gg, gb)) => format!(
                    "   [\x1b[48;2;{gr};{gg};{gb}m    \x1b[0m] \x1b[33m#{gr:02X}{gg:02X}{gb:02X}\x1b[0m \x1b[36mgpu\x1b[0m"
                ),
                None => String::new(),
            };
            println!("  [\x1b[48;2;{r};{g};{b}m    \x1b[0m]  \x1b[1;97m{name:<10}\x1b[0m \x1b[33m#{r:02X}{g:02X}{b:02X}\x1b[0m{extra}");
        } else {
            let extra = match gpu {
                Some((gr, gg, gb)) => format!("   #{gr:02X}{gg:02X}{gb:02X} gpu"),
                None => String::new(),
            };
            println!("  {name:<10} #{r:02X}{g:02X}{b:02X}{extra}");
        }
    }
    if !GPU_PRESETS.is_empty() {
        println!();
        println!("A `gpu` column means that preset is calibrated separately for the GPU.");
        println!("Others use the same value on both. Hex arguments are always literal.");
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
         \x20 jdrgb preview         slideshow all presets (+/- speed, n/N next/prev, s stop)\n\
         \x20 jdrgb probe           show firmware + config (diagnostics)\n\
         \x20 jdrgb --gpu save      commit the GPU's current color to its flash (see below)\n\
         \x20 jdrgb --version       print version\n\
         \x20 jdrgb --help          this message\n\
         \n\
         FLAGS:\n\
         \x20 --wait                retry ~60s until the controller is ready (use at boot)\n\
         \x20 --gpu                 act on the GPU LEDs instead of the motherboard\n\
         \x20 --all                 act on both\n\
         \n\
         TARGETS:\n\
         \x20 Without --gpu/--all, everything targets the motherboard strip, as always.\n\
         \x20 rainbow/load/template are motherboard-only (the GPU has a handful of LEDs).\n\
         \x20 `save` writes the GPU controller's non-volatile flash so the color holds\n\
         \x20 with nothing running — do it once by hand, never on a schedule.\n",
        ver = env!("CARGO_PKG_VERSION"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typo'd name in GPU_PRESETS would silently never match, and the preset
    /// would quietly keep using the strip's value forever.
    #[test]
    fn every_gpu_override_names_a_real_preset() {
        for &(name, _) in GPU_PRESETS {
            assert!(lookup_preset(name).is_some(), "GPU_PRESETS has no matching preset: {name}");
        }
    }

    #[test]
    fn calibrated_preset_differs_per_device() {
        let p = Paint::Preset("warmwhite");
        assert_eq!(p.rgb(false), (0xFA, 0x95, 0x36)); // strip
        assert_eq!(p.rgb(true), (0xFF, 0x85, 0x12)); // GPU, tuned by eye
    }

    #[test]
    fn uncalibrated_preset_is_shared() {
        // `red` has no GPU entry, so both devices get the same value.
        let p = Paint::Preset("red");
        assert_eq!(p.rgb(false), p.rgb(true));
        assert_eq!(p.rgb(true), (0xFF, 0x00, 0x00));
    }

    #[test]
    fn hex_is_never_remapped() {
        // Typing warmwhite's strip value explicitly must not become the GPU's.
        let p = Paint::Literal((0xFA, 0x95, 0x36));
        assert_eq!(p.rgb(false), (0xFA, 0x95, 0x36));
        assert_eq!(p.rgb(true), (0xFA, 0x95, 0x36));
    }

    #[test]
    fn parses_names_and_hex() {
        assert!(matches!(parse_paint("WarmWhite"), Some(Paint::Preset("warmwhite"))));
        assert!(matches!(parse_paint("ff8512"), Some(Paint::Literal((0xFF, 0x85, 0x12)))));
        assert!(matches!(parse_paint("#FF8512"), Some(Paint::Literal((0xFF, 0x85, 0x12)))));
        assert!(parse_paint("nonsense").is_none());
    }

    #[test]
    fn default_preset_resolves() {
        assert_eq!(Paint::Preset(DEFAULT_PRESET).rgb(false), DEFAULT_COLOR);
    }

    #[test]
    fn target_flags_select_devices() {
        assert!(Target::Mb.mb() && !Target::Mb.gpu());
        assert!(!Target::Gpu.mb() && Target::Gpu.gpu());
        assert!(Target::All.mb() && Target::All.gpu());
    }

    #[test]
    fn per_led_commands_are_motherboard_only() {
        assert!(mb_only(&Command::Rainbow(38)).is_some());
        assert!(mb_only(&Command::Load("x".into())).is_some());
        assert!(mb_only(&Command::Template("x".into())).is_some());
        assert!(mb_only(&Command::Solid(MODE_STATIC, Paint::Preset("red"))).is_none());
        assert!(mb_only(&Command::Probe).is_none());
    }
}
