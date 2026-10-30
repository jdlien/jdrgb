//! Colors, and the record of what was last set.
//!
//! Shared by `jdrgb.exe` and `jdrgb-tray.exe`. The tray needs the preset names
//! to build its menu, the `SWATCH` values to draw them (wire values would draw
//! `coolwhite` as pink), and the state file to know which one is current — so
//! this is everything about *what a color is*, with nothing about how to send
//! one. No device access, no printing.
//!
//! Deliberately excludes `Target` and the effect-mode constants: those describe
//! a command, not a color, and the tray builds its commands as CLI arguments.

use std::path::Path;

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// Default: `coolwhite`, hand-tuned by eye so the strip renders a clean white.
/// (Nominal #FFFFFF reads greenish on this strip; this tuned value looks pink on
/// a screen but renders as a proper cool white, so it's the default.)
pub const DEFAULT_COLOR: (u8, u8, u8) = (0xFF, 0xB0, 0xD0);

/// The default as a preset name, so each device gets its own tuned value if one
/// is listed in GPU_PRESETS.
pub const DEFAULT_PRESET: &str = "coolwhite";

/// Case-insensitive keyword colors. Sensible starting points only — LEDs render
/// colors quite differently from nominal RGB, so tune any that look off by eye.
pub const PRESETS: &[(&str, (u8, u8, u8))] = &[
    ("coolwhite", (0xFF, 0xB0, 0xD0)), // hand-tuned clean white; the default
    ("warmwhite", (0xFA, 0x95, 0x36)), // the original warm white
    // Nominal white, deliberately left untuned as a reference point: it reads
    // greenish on the strip and sky-blue on the GPU. Use coolwhite for a real white.
    ("white", (0xFF, 0xFF, 0xFF)),
    // Static black. Not the same as `off`, which selects the controller's own
    // off mode; this is "display black". Handy in a theme when you want one
    // device dark and the other lit.
    ("black", (0x00, 0x00, 0x00)),
    ("red", (0xFF, 0x00, 0x00)),
    // Interpolated to fill a gap, not dialled in — see the note on GPU_PRESETS.
    //
    // Named for what it renders as, not for where it sits on the wheel. By value
    // it is within 2° of dictionary scarlet (#FF2400) and it was called
    // `vermilion` for a while on the same reasoning. Neither name survived
    // contact with the strip: asking a HomeKit bulb for "vermillion" and then
    // for "burnt orange" and holding each against this, it is plainly the
    // second. The wheel says one thing and the LEDs say another; the LEDs win,
    // because the name exists to help you recognise the colour before it lands.
    ("burntorange", (0xFF, 0x1D, 0x00)),
    ("orange", (0xFF, 0x3A, 0x00)),
    ("amber", (0xFF, 0x87, 0x00)),
    ("yellow", (0xFF, 0xD0, 0x00)),
    ("chartreuse", (0xD4, 0xFF, 0x00)),
    ("lime", (0x80, 0xFF, 0x00)),
    ("green", (0x00, 0xFF, 0x00)),
    ("seagreen", (0x00, 0xFF, 0x51)),
    ("teal", (0x00, 0xFF, 0x80)),
    ("turquoise", (0x00, 0xFF, 0xBF)), // interpolated
    ("cyan", (0x00, 0xFF, 0xFF)),
    ("sky", (0x00, 0xBF, 0xFF)),    // interpolated
    ("azure", (0x00, 0x80, 0xFF)),  // bright sky blue
    ("cobalt", (0x00, 0x40, 0xFF)), // mid blue, bridges azure->blue in hue + brightness
    // Interpolated. By wire value cobalt->blue is an ordinary 15° step, but it
    // reads much wider, because the strip starts expanding hue here — the tuned
    // indigo/purple values imply an appearance/wire slope near 1.6-2.5 just
    // above blue, which would make this gap perceptually ~24°.
    ("sapphire", (0x00, 0x20, 0xFF)),
    ("blue", (0x00, 0x00, 0xFF)),
    ("indigo", (0x27, 0x00, 0xFF)),
    ("purple", (0x40, 0x00, 0xFF)),
    ("violet", (0x6E, 0x00, 0xFF)),
    ("magenta", (0xFF, 0x00, 0xFF)),
    ("cerise", (0xFF, 0x00, 0xBF)),  // interpolated
    ("hotpink", (0xFF, 0x00, 0x80)), // intense synthwave/Barbie pink
    ("rose", (0xFF, 0x00, 0x40)),    // interpolated
    ("pink", (0xD5, 0x2A, 0x66)),    // softer, "pretty in pink"
];

/// Per-preset overrides for the GPU, which renders colors quite differently from
/// the strip — the same nominal value can look nothing alike on the two.
///
/// With the full wheel dialled in, one pattern holds across every entry: this
/// card's blue is far stronger than the strip's, so **every** calibration lowers
/// blue relative to the other channels. Nothing else is consistent — green
/// against red moves both ways and only slightly.
///
/// That single effect explains what look like opposite corrections. Where blue is
/// the minor channel it gets cut outright (`magenta` 255 -> 38, `cyan` 255 -> 98).
/// Where blue is already the dominant channel and pinned at max, the same
/// reduction has to be expressed by raising the others instead (`azure` green
/// 128 -> 255, `indigo` red 39 -> 187) — which is why those look like they follow
/// a different rule. They don't.
///
/// Nominal `#FFFFFF` reading as sky blue is the same effect at its most obvious.
///
/// Pure primaries (`red`, `green`, `blue`) need no entry: one channel at max and
/// no ratio to correct.
///
/// Deliberately sparse: only presets actually dialled in by eye on the GPU belong
/// here. Anything absent falls back to the shared value in PRESETS above, so this
/// table never claims a calibration that hasn't been eyeballed. Add entries with
/// `jdrgb --gpu tune NAME` and paste the hex it prints.
///
/// The `interp` entries are the one exception, and they say so. Each pair here is
/// really a record of two values that look alike, which makes the whole set a
/// sampled strip-hue -> GPU-hue function — monotonic across all 18 originals, and
/// pure hue, since every saturated value in both tables sits at full saturation.
/// Four presets were added by interpolating it to close the widest gaps. They
/// were chosen to land in stretches where the slope is stable between
/// neighbouring samples; the wild region (`blue`->`purple`, where the slope runs
/// 3.0 -> 7.8 -> 0.6) is already the densest part of the wheel and needed
/// nothing. Verify one in `preview` and drop its marker, or re-tune it.
pub const GPU_PRESETS: &[(&str, (u8, u8, u8))] = &[
    // Kept in PRESETS order so the two tables read side by side. The comment on
    // each line is the strip value it was matched against.
    ("coolwhite", (0xD2, 0x94, 0x32)),  // FFB0D0
    ("warmwhite", (0xFF, 0x56, 0x0A)),  // FA9536
    ("burntorange", (0xFF, 0x10, 0x00)), // FF1D00  interp
    ("orange", (0xFF, 0x20, 0x00)),     // FF3A00
    ("amber", (0xFF, 0x54, 0x00)),      // FF8700
    ("yellow", (0xFF, 0x8C, 0x00)),     // FFD000
    ("chartreuse", (0xFF, 0xC0, 0x00)), // D4FF00
    ("lime", (0xDE, 0xFF, 0x00)),       // 80FF00
    ("seagreen", (0x00, 0xFF, 0x15)),   // 00FF51
    ("teal", (0x00, 0xFF, 0x2F)),       // 00FF80
    ("turquoise", (0x00, 0xFF, 0x49)),  // 00FFBF  interp
    ("cyan", (0x00, 0xFF, 0x62)),       // 00FFFF
    ("sky", (0x00, 0xFF, 0x8C)),        // 00BFFF  interp
    ("azure", (0x00, 0xFF, 0xB6)),      // 0080FF
    ("cobalt", (0x00, 0xC0, 0xFF)),     // 0040FF
    ("sapphire", (0x00, 0x60, 0xFF)),   // 0020FF  interp
    ("indigo", (0xBB, 0x00, 0xFF)),     // 2700FF
    ("purple", (0xFF, 0x00, 0x7F)),     // 4000FF
    ("violet", (0xFF, 0x00, 0x62)),     // 6E00FF
    ("magenta", (0xFF, 0x00, 0x26)),    // FF00FF
    ("cerise", (0xFF, 0x00, 0x1B)),     // FF00BF  interp
    ("hotpink", (0xFF, 0x00, 0x11)),    // FF0080
    ("rose", (0xFF, 0x00, 0x08)),       // FF0040  interp
    ("pink", (0xE7, 0x1F, 0x18)),       // D52A66
];

/// What each preset is *meant to look like*, for on-screen swatches only —
/// `jdrgb presets`/`preview` in the terminal, and the tray's menu circles.
///
/// PRESETS and GPU_PRESETS hold values tuned so the LEDs render correctly, which
/// makes them wrong on a monitor — `coolwhite`'s #FFB0D0 draws as pink and
/// `warmwhite`'s #FA9536 as orange. A swatch painted from those shows what we're
/// *sending*, when the useful thing is what you'd *see*. That's backwards for a
/// preview, whose whole job is to help you recognise a color before it lands.
///
/// So this is the third view of a preset: nominal sRGB, what the name denotes on
/// a screen. Display only — it never reaches a device, so a wrong value here is
/// a cosmetic bug and nothing more.
///
/// Deliberately sparse, like GPU_PRESETS: only names whose tuned value misleads
/// on screen need an entry. Pure hues (`red`, `cyan`, `magenta`) already look
/// like themselves. Note several entries match what PRESETS held before the
/// strip was tuned by eye — the tuning moved the hardware value away from the
/// nominal one, which is exactly the divergence this table records.
///
/// Adjust by eye against your monitor, the same way the device tables were
/// adjusted against the hardware.
pub const SWATCH: &[(&str, (u8, u8, u8))] = &[
    ("coolwhite", (0xF5, 0xF8, 0xFF)), // a clean white, faintly cool
    ("warmwhite", (0xFF, 0xD9, 0xB3)), // ~2700K warm white
    // The strip compresses the red->orange arc, so burntorange's #FF1D00 sits at
    // hue 7 on the wire while reading as hue 15. turquoise/sky/cerise/rose need
    // no entry: their stretch of the wheel is untuned, so wire and appearance
    // already agree.
    //
    // NOTE: this entry predates the rename and may now be understating things.
    // If the strip really reads as burnt orange, the appearance sits nearer hue
    // 25 than 15 — but hue 30 is `orange`, so moving it would crowd the two
    // together. Worth an eyeball against both before changing.
    ("burntorange", (0xFF, 0x40, 0x00)),
    ("orange", (0xFF, 0x7F, 0x00)),
    ("amber", (0xFF, 0xBF, 0x00)),
    ("yellow", (0xFF, 0xFF, 0x00)),
    ("chartreuse", (0xAA, 0xFF, 0x00)),
    ("lime", (0x55, 0xFF, 0x00)),
    // The blue->magenta ramp, evenly spaced in hue as the names imply.
    ("indigo", (0x40, 0x00, 0xFF)),
    ("purple", (0x80, 0x00, 0xFF)),
    ("violet", (0xBF, 0x00, 0xFF)),
    ("pink", (0xFF, 0x9E, 0xB5)), // soft, not the saturated value the strip needs
];

/// A preset name as it should read in a UI: `burntorange` -> `Burnt Orange`.
///
/// The stored names are lowercase single tokens because that is what you type.
/// Nothing is typed at a menu, so a menu can afford to be legible.
///
/// The word split is derived rather than tabulated. Every compound in this
/// palette is a modifier followed by a base colour that is *itself* a preset —
/// `seagreen` ends in `green`, `burntorange` in `orange`, `coolwhite` in
/// `white`. So the table teaches the splitter, and a compound added later reads
/// correctly without anyone remembering to update a map. The longest matching
/// base wins, and the base is split again, so a hypothetical `darkseagreen`
/// would come out as `Dark Sea Green` rather than `Dark Seagreen`.
///
/// The remainder must be a plausible word on its own, or a future two-letter
/// preset would start slicing `red` into `R Ed`.
pub fn display_name(name: &str) -> String {
    const MIN_MODIFIER: usize = 3;

    let base = PRESETS
        .iter()
        .map(|&(base, _)| base)
        .filter(|base| *base != name && name.len() >= base.len() + MIN_MODIFIER)
        .filter(|base| name.ends_with(base))
        .max_by_key(|base| base.len());

    match base {
        Some(base) => {
            let modifier = &name[..name.len() - base.len()];
            format!("{} {}", capitalise(modifier), display_name(base))
        }
        None => capitalise(name),
    }
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The on-screen appearance of a preset, falling back to the strip's value for
/// anything that already looks like itself.
pub fn swatch_rgb(name: &str) -> (u8, u8, u8) {
    SWATCH
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, rgb)| rgb)
        .or_else(|| lookup_preset(name))
        .unwrap_or(DEFAULT_COLOR)
}

/// A color on its way to a device.
///
/// Preset names stay unresolved until the moment of writing, because the right
/// RGB depends on which controller it lands on. An explicit hex is always taken
/// literally — if you type a value, you get that value, which is what makes
/// `tune`'s output directly reusable.
#[derive(Clone, Copy)]
pub enum Paint {
    Literal((u8, u8, u8)),
    Preset(&'static str),
}

impl Paint {
    pub fn rgb(self, gpu: bool) -> (u8, u8, u8) {
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

pub fn lookup_preset(name: &str) -> Option<(u8, u8, u8)> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|&(_, rgb)| rgb)
}

// ---------------------------------------------------------------------------
// Brightness
// ---------------------------------------------------------------------------

/// Dim a device's own value to `level` percent: each channel becomes
/// `(c × level + 50) / 100`, in integers.
///
/// Always applied *after* `Paint::rgb` has resolved the preset for the device,
/// never before: scaling the shared value and then calibrating would dim the
/// strip's value on the card, which is a different color there. And a plain
/// multiplier, because the LEDs are PWM-driven, so equal scaling should keep the
/// ratios — and with them the color — until the 8-bit channels run out of room.
/// Whether a slider wants a perceptual curve is a question for whatever drives
/// this, not for `--level`.
pub fn scale(rgb: (u8, u8, u8), level: u8) -> (u8, u8, u8) {
    let l = u32::from(level.min(100));
    let s = |c: u8| ((u32::from(c) * l + 50) / 100) as u8;
    (s(rgb.0), s(rgb.1), s(rgb.2))
}

/// The lowest level a slider should reach. Below it the 8-bit channels lose
/// their ratios — `coolwhite` at 2% is `(5,4,4)` — and presets start rounding
/// to the same bytes, so a dimmed value can no longer say which preset it was.
///
/// **Provisional.** 10 is the lowest floor at which no two presets share a
/// wire value at any level on either device (`no_two_presets_share_a_wire_value
/// from_min_level_up` pins that; Cerise and Hot Pink both become `(23,0,2)` on
/// the card at 9). Whether 10% still *looks* like the right color on both
/// devices is a question for the eye, and the answer may raise it.
///
/// The CLI accepts the whole 1–100 range; this is the floor the Home Assistant
/// bridge maps its brightness slider onto.
pub const MIN_LEVEL: u8 = 10;

/// Which preset, at which level, `--level` would have turned into exactly this
/// recorded value on this device — the inverse of `scale(Paint::rgb(gpu), l)`.
///
/// The state file holds wire values, so this is how anything reading it learns
/// "Warm White at 40%" rather than "#64140F". The highest matching level wins,
/// then table order, so an undimmed preset is always itself at 100 and `black`
/// — zero at every level — is `black` at 100. Below `MIN_LEVEL` two presets can
/// round to the same bytes and the answer may be the other one; that is what
/// the floor is for.
///
/// 29 presets x 100 levels of integer arithmetic: nothing worth caching.
pub fn recognise(rgb: (u8, u8, u8), gpu: bool) -> Option<(&'static str, u8)> {
    let mut best: Option<(&'static str, u8)> = None;
    for &(name, _) in PRESETS {
        let full = Paint::Preset(name).rgb(gpu);
        if let Some(level) = (1..=100u8).rev().find(|&l| scale(full, l) == rgb) {
            if best.is_none_or(|(_, b)| level > b) {
                best = Some((name, level));
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Appearance: hue and saturation, for a color wheel
// ---------------------------------------------------------------------------

/// HSV hue (degrees, 0 up to 360) and saturation (0 to 1) of an sRGB value.
///
/// HSV, not the HSL that `tune` steps in: a near-white like `coolwhite`'s swatch
/// is ~4% saturated in HSV and 100% in HSL, and a color wheel's saturation is
/// HSV's — distance from the white centre.
pub fn hsv_of((r, g, b): (u8, u8, u8)) -> (f64, f64) {
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max == 0.0 { 0.0 } else { d / max };
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), s)
}

/// Where a preset *looks* like it sits on a color wheel: its `SWATCH`
/// appearance as HSV. Never the wire value — `coolwhite` is pink on the wire.
pub fn appearance_hs(name: &str) -> (f64, f64) {
    hsv_of(swatch_rgb(name))
}

/// The preset nearest a requested hue (degrees) and saturation (0 to 100), the
/// way a color wheel would place them.
///
/// Distance is taken on the disc — `(s·cos h, s·sin h)` — not along hue alone,
/// which is what makes the whites and `pink` reachable: a low-saturation request
/// lands on `coolwhite` or `warmwhite` by its hue, a soft pink on `pink`. Ties go
/// to table order.
///
/// Every preset is a candidate except `black` (not a color) and `white` (an
/// uncalibrated reference that reads green on the strip).
pub fn snap_hs(hue: f64, sat_percent: f64) -> &'static str {
    let point = |h: f64, s: f64| {
        let t = h.to_radians();
        (s * t.cos(), s * t.sin())
    };
    let (x, y) = point(hue, (sat_percent / 100.0).clamp(0.0, 1.0));
    let mut best = ("coolwhite", f64::INFINITY);
    for &(name, _) in PRESETS {
        if name == "black" || name == "white" {
            continue;
        }
        let (h, s) = appearance_hs(name);
        let (px, py) = point(h, s);
        let d = (px - x).powi(2) + (py - y).powi(2);
        if d < best.1 {
            best = (name, d);
        }
    }
    best.0
}

/// The two whites, as color temperatures. Labels chosen to match how they look,
/// not measurements: `warmwhite`'s `SWATCH` comment already calls it ~2700 K,
/// and `coolwhite` reads as a clean daylight white.
pub const WARM_KELVIN: u32 = 2700;
pub const COOL_KELVIN: u32 = 6500;

/// The white nearest a requested color temperature. Nearest in mireds, which is
/// how temperature differences are perceived — the split falls near 3815 K, not
/// at the arithmetic midpoint. Ties go to `warmwhite`.
pub fn snap_kelvin(kelvin: u32) -> &'static str {
    let mired = |k: u32| 1_000_000.0 / f64::from(k.max(1));
    let k = mired(kelvin);
    if (k - mired(WARM_KELVIN)).abs() <= (k - mired(COOL_KELVIN)).abs() {
        "warmwhite"
    } else {
        "coolwhite"
    }
}

/// A white preset's color temperature label, or `None` for anything else.
pub fn kelvin_of(name: &str) -> Option<u32> {
    match name {
        "warmwhite" => Some(WARM_KELVIN),
        "coolwhite" => Some(COOL_KELVIN),
        _ => None,
    }
}

/// Resolve a preset name (case-insensitive) or an `RRGGBB` hex string.
pub fn parse_paint(s: &str) -> Option<Paint> {
    let lower = s.to_ascii_lowercase();
    if let Some(&(name, _)) = PRESETS.iter().find(|(n, _)| *n == lower) {
        return Some(Paint::Preset(name));
    }
    parse_hex(s).map(Paint::Literal)
}

/// The inverse of `Paint::rgb`: which preset, if any, would produce this exact
/// value on this device. Used to name the color in the state file, which records
/// RGB rather than the name that produced it.
///
/// Exact match only — the state file holds values these tables wrote, so there
/// is nothing to approximate. A hand-tuned hex correctly matches nothing.
///
/// The fallback to PRESETS must skip anything carrying a GPU override, or the
/// card would answer `warmwhite` while displaying the *strip's* #FA9536 — a
/// value that renders as something else entirely there. A preset is only shared
/// between devices when it has no override.
pub fn preset_for_rgb(rgb: (u8, u8, u8), gpu: bool) -> Option<&'static str> {
    if gpu {
        if let Some(&(name, _)) = GPU_PRESETS.iter().find(|(_, v)| *v == rgb) {
            return Some(name);
        }
    }
    PRESETS
        .iter()
        .find(|(name, v)| *v == rgb && !(gpu && GPU_PRESETS.iter().any(|(n, _)| n == name)))
        .map(|&(name, _)| name)
}

pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
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

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Path to the "last color" state file in the user's local app data.
///
/// Per-user by construction, which matters more than it looks: the boot task
/// runs as SYSTEM and so writes SYSTEM's copy, not the logged-in user's. The
/// tray therefore cannot see what the boot task applied. See docs/tray-plan.md.
pub fn state_path() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| std::path::PathBuf::from(p).join("jdrgb").join("last"))
}

/// What one device was last set to. Absent from the file means "never recorded",
/// which is different from `Multi` — the first falls back to the default, the
/// second means there's no single color to pick up from.
///
/// `Off` is not `Color((0,0,0))`. They look identical but are different
/// controller states — mode register 0 versus 1, confirmed by reading it back —
/// and the difference is visible to anything that reads this file. Recording
/// `off` as black would make a tray menu claim the `black` preset was active
/// when the controller is actually in its own off mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Last {
    Color((u8, u8, u8)),
    Multi,
    Off,
}

impl Last {
    pub fn encode(self) -> String {
        match self {
            Last::Color((r, g, b)) => format!("{r:02X}{g:02X}{b:02X}"),
            Last::Multi => "multi".to_string(),
            Last::Off => "off".to_string(),
        }
    }

    pub fn decode(s: &str) -> Option<Last> {
        if s.eq_ignore_ascii_case("multi") {
            Some(Last::Multi)
        } else if s.eq_ignore_ascii_case("off") {
            Some(Last::Off)
        } else {
            parse_hex(s).map(Last::Color)
        }
    }
}

/// Everything the state file holds, for both devices.
///
/// The two devices keep separate slots. Sharing one would be actively wrong:
/// the same look is a *different* RGB on each (see GPU_PRESETS), so carrying the
/// strip's last value onto the card hands `tune` a starting point that doesn't
/// render as anything like what was on screen.
///
/// The `*_pattern` fields exist so a per-LED state can be *reproduced*, which
/// `Multi` alone cannot do — it says only "no single color", which is the right
/// answer for the tray's swatch and useless for `jdrgb state`. They are a side
/// channel rather than a new `Last` variant on purpose: `Last` is what the tray
/// matches on, and it wants exactly the three cases it already has.
///
/// **Invariant: a pattern is only ever written by `record_pattern`, and any
/// other write to that slot clears it.** That is what makes a stale pattern
/// impossible rather than merely unlikely — see `record`.
///
/// The `*_lit` fields are what `jdrgb on` brings back. Without them `off` was
/// one-way: it overwrote the only record of what had been showing.
///
/// `rev` counts writes. Every `update` bumps it, whether or not any value
/// changed, so a reader can tell that *someone wrote* even when they wrote
/// identical values — which values alone can't show. A counter rather than the
/// modification time because file timestamps come from a clock that can tick as
/// coarsely as ~15 ms, so two writes could share one.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct State {
    pub mb: Option<Last>,
    pub gpu: Option<Last>,
    pub mb_pattern: Option<Vec<(u8, u8, u8)>>,
    pub gpu_pattern: Option<Vec<(u8, u8, u8)>>,
    pub mb_lit: Option<Lit>,
    pub gpu_lit: Option<Lit>,
    pub rev: u64,
}

/// What a device was showing the last time it went from lit to off.
///
/// Only replayable states are kept — never `Off`, and never a `Multi` whose
/// frame was lost — because the one thing this is for is putting them back.
///
/// It is written only on the way *into* off, and survives everything else on
/// purpose. A second `off` must not trade the colour for "nothing", and a
/// borrowed colour painted over a dark room must not become what `on` brings
/// back (see `keep_lit_records`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Lit {
    Color((u8, u8, u8)),
    Frame(Vec<(u8, u8, u8)>),
}

impl Lit {
    /// Back from the file's two keys. `off` was never a lit state and a
    /// `multi` without its frame can't be replayed; both read as nothing kept,
    /// so `on` falls back rather than guessing.
    fn from_parts(last: Option<Last>, pattern: Option<Vec<(u8, u8, u8)>>) -> Option<Lit> {
        match (last?, pattern) {
            (Last::Color(rgb), _) => Some(Lit::Color(rgb)),
            (Last::Multi, Some(frame)) => Some(Lit::Frame(frame)),
            _ => None,
        }
    }
}

impl State {
    /// `record`'s edit, without the file I/O.
    pub fn record(&mut self, gpu: bool, last: Last) {
        self.record_with(gpu, last, true);
    }

    /// `record`'s edit in keep-lit mode: the slot changes, the lit record
    /// doesn't. See `keep_lit_records`.
    pub fn record_keeping_lit(&mut self, gpu: bool, last: Last) {
        self.record_with(gpu, last, false);
    }

    fn record_with(&mut self, gpu: bool, last: Last, lit_transition: bool) {
        if lit_transition && last == Last::Off && self.slot(gpu) != Some(Last::Off) {
            let showing = self.replayable(gpu);
            *self.lit_mut(gpu) = showing;
        }
        let (slot, pattern) = self.slot_mut(gpu);
        *slot = Some(last);
        *pattern = None;
    }

    /// `record_pattern`'s edit, without the file I/O.
    pub fn record_pattern(&mut self, gpu: bool, colors: &[(u8, u8, u8)]) {
        let (slot, pattern) = self.slot_mut(gpu);
        *slot = Some(Last::Multi);
        *pattern = Some(colors.to_vec());
    }

    pub fn lit(&self, gpu: bool) -> Option<&Lit> {
        if gpu {
            self.gpu_lit.as_ref()
        } else {
            self.mb_lit.as_ref()
        }
    }

    /// What this device is showing, in a form `on` could replay.
    fn replayable(&self, gpu: bool) -> Option<Lit> {
        match (self.slot(gpu), self.pattern(gpu)) {
            (Some(Last::Color(rgb)), _) => Some(Lit::Color(rgb)),
            (Some(Last::Multi), Some(frame)) => Some(Lit::Frame(frame.to_vec())),
            _ => None,
        }
    }

    fn slot_mut(&mut self, gpu: bool) -> (&mut Option<Last>, &mut Option<Vec<(u8, u8, u8)>>) {
        if gpu {
            (&mut self.gpu, &mut self.gpu_pattern)
        } else {
            (&mut self.mb, &mut self.mb_pattern)
        }
    }

    fn lit_mut(&mut self, gpu: bool) -> &mut Option<Lit> {
        if gpu {
            &mut self.gpu_lit
        } else {
            &mut self.mb_lit
        }
    }

    /// One device's slot, by the `Paint::rgb(gpu)` convention used everywhere.
    pub fn slot(&self, gpu: bool) -> Option<Last> {
        if gpu {
            self.gpu
        } else {
            self.mb
        }
    }

    pub fn pattern(&self, gpu: bool) -> Option<&[(u8, u8, u8)]> {
        let p = if gpu { &self.gpu_pattern } else { &self.mb_pattern };
        p.as_deref()
    }
}

/// Format is one `key=value` per line. A legacy file — a bare hex, from before
/// the split — is read as the motherboard's, since that's the only device that
/// ever wrote one.
///
/// Unknown keys are skipped, which is what let the `*.pattern` keys be added
/// without touching anything that already reads this file.
pub fn parse_state(text: &str) -> State {
    let text = text.trim();
    if !text.contains('=') {
        return State { mb: Last::decode(text), ..State::default() };
    }
    let mut s = State::default();
    // A lit record is two keys, like the live slot, so it can only be put
    // together once every line has been seen.
    let (mut mb_lit, mut mb_lit_pattern, mut gpu_lit, mut gpu_lit_pattern) = (None, None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "mb" => s.mb = Last::decode(value),
            "gpu" => s.gpu = Last::decode(value),
            "mb.pattern" => s.mb_pattern = decode_pattern(value),
            "gpu.pattern" => s.gpu_pattern = decode_pattern(value),
            "mb.lit" => mb_lit = Last::decode(value),
            "gpu.lit" => gpu_lit = Last::decode(value),
            "mb.lit.pattern" => mb_lit_pattern = decode_pattern(value),
            "gpu.lit.pattern" => gpu_lit_pattern = decode_pattern(value),
            "rev" => s.rev = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    s.mb_lit = Lit::from_parts(mb_lit, mb_lit_pattern);
    s.gpu_lit = Lit::from_parts(gpu_lit, gpu_lit_pattern);
    s
}

/// A pattern is all-or-nothing: one unparseable entry drops the whole thing,
/// rather than silently restoring a frame with a wrong LED in it. Same spirit
/// as garbage in a colour slot being ignored instead of guessed at.
fn decode_pattern(s: &str) -> Option<Vec<(u8, u8, u8)>> {
    if s.is_empty() {
        return None;
    }
    s.split(',').map(|c| parse_hex(c.trim())).collect()
}

fn encode_pattern(colors: &[(u8, u8, u8)]) -> String {
    colors
        .iter()
        .map(|&(r, g, b)| format!("{r:02X}{g:02X}{b:02X}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn encode_state(s: &State) -> String {
    let mut out = String::new();
    if s.rev != 0 {
        out.push_str(&format!("rev={}\n", s.rev));
    }
    if let Some(v) = s.mb {
        out.push_str(&format!("mb={}\n", v.encode()));
    }
    if let Some(p) = &s.mb_pattern {
        out.push_str(&format!("mb.pattern={}\n", encode_pattern(p)));
    }
    if let Some(v) = s.gpu {
        out.push_str(&format!("gpu={}\n", v.encode()));
    }
    if let Some(p) = &s.gpu_pattern {
        out.push_str(&format!("gpu.pattern={}\n", encode_pattern(p)));
    }
    for (dev, lit) in [("mb", &s.mb_lit), ("gpu", &s.gpu_lit)] {
        match lit {
            Some(Lit::Color(rgb)) => out.push_str(&format!("{dev}.lit={}\n", Last::Color(*rgb).encode())),
            Some(Lit::Frame(f)) => {
                out.push_str(&format!("{dev}.lit=multi\n{dev}.lit.pattern={}\n", encode_pattern(f)))
            }
            None => {}
        }
    }
    out
}

pub fn load_state_full() -> State {
    match state_path() {
        Some(p) => load_state_at(&p),
        None => State::default(),
    }
}

fn load_state_at(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_state(&text),
        Err(_) => State::default(),
    }
}

/// The two colour slots alone — what the tray needs, and all it has ever asked
/// for. Kept so adding patterns changed nothing on that side.
pub fn load_state() -> (Option<Last>, Option<Last>) {
    let s = load_state_full();
    (s.mb, s.gpu)
}

// ---------------------------------------------------------------------------
// Writing the state file: one writer at a time, readers never see half a file
// ---------------------------------------------------------------------------

/// How long a writer waits for another to finish with the state file before
/// giving up on its record. A write takes microseconds; this only matters when
/// something is badly stuck.
const STATE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Exclusive use of the state file and the stash beside it, for one
/// read-modify-write. Released on drop.
///
/// A lock on a sidecar file rather than on the state file itself, because the
/// state file is replaced by rename (see `write_atomic`) and a lock on a file
/// that is about to be replaced protects nothing. Per-file, so it is naturally
/// scoped to the user whose file it is — the SYSTEM boot task has its own.
///
/// **Not re-entrant.** It is a `LockFileEx` lock, held per handle, so a holder
/// that calls `update` (directly, or through `record`) waits on itself and
/// gives up after `STATE_LOCK_WAIT`. Take it only around reads and direct
/// writes, never around anything that records.
pub struct StateLock(std::fs::File);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Take the state lock for this user, or `None` if it couldn't be had in time.
pub fn lock_state() -> Option<StateLock> {
    lock_at(&state_path()?.with_file_name("state.lock"))
}

fn lock_at(path: &Path) -> Option<StateLock> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).ok()?;
    let deadline = std::time::Instant::now() + STATE_LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(StateLock(file)),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Replace `path` with `text` so that a reader sees the old file or the new
/// one, never a truncated one.
///
/// `std::fs::write` truncates and then writes, and the tray — and now a bridge
/// polling once a second — can read between the two. Writing a temporary file
/// and renaming it over the original closes that window: `rename` is
/// `MoveFileExW` with replace on Windows. Rust's `File::open` shares delete
/// access, so a Rust reader holding the file open doesn't block the rename.
///
/// Something else can — a virus scanner opening a fresh file, say, or a reader
/// that didn't share delete — so the rename is retried for a moment. If it
/// still won't go, this **fails** and the original stays exactly as it was. It
/// never falls back to writing in place: that would hand a reader the torn file
/// this exists to prevent, and a write that then failed part-way would destroy
/// the original — for the stash, the only copy of what to put back. Returns
/// whether the file was replaced.
pub fn write_atomic(path: &Path, text: &str) -> bool {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    for _ in 0..20 {
        if std::fs::rename(&tmp, path).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// Read-modify-write the state file, under the state lock, bumping `rev`.
///
/// Best-effort, as it always was: a record that can't be written is not worth
/// failing the command over, since the lights already changed. But a skipped
/// record makes every reader wrong about what's showing, so it is noted
/// (`record_failed`) for `jdrgb.exe` to say so on stderr — which the tray and
/// the bridge both surface. This module itself never prints.
///
/// Only the process that actually wrote to a device calls this — which is always
/// `jdrgb.exe`. The tray and the bridge read this file but never write it.
fn update(edit: impl FnOnce(&mut State)) {
    let Some(path) = state_path() else { return };
    if !update_at(&path, edit) {
        RECORD_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

static RECORD_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether any record in this process failed to land.
pub fn record_failed() -> bool {
    RECORD_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

fn update_at(path: &Path, edit: impl FnOnce(&mut State)) -> bool {
    let Some(_lock) = lock_at(&path.with_file_name("state.lock")) else { return false };
    let mut s = load_state_at(path);
    edit(&mut s);
    s.rev = s.rev.wrapping_add(1);
    write_atomic(path, &encode_state(&s))
}

/// Keep-lit mode, process-wide: `record` changes slots but not lit records.
///
/// For `restore`. Lights off, a UPS hook paints them amber with `--stash`, mains
/// restores them to off — and that restore is a move from lit into off, which
/// on its own would record *amber* as what `on` brings back. The stash put the
/// room back; it must put this back too, which here means leaving it alone.
///
/// Process-wide is safe because `restore` is the whole of that process's work.
/// It replaced an earlier approach that snapshotted both lit records and wrote
/// the snapshot back afterwards, which could overwrite a lit record another
/// process had recorded in between.
static KEEP_LIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn keep_lit_records() {
    KEEP_LIT.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn keeping_lit() -> bool {
    KEEP_LIT.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// The stash's identity
// ---------------------------------------------------------------------------

/// Where `--stash` keeps its snapshot, beside the state file.
pub fn stash_path() -> Option<std::path::PathBuf> {
    state_path().map(|p| p.with_file_name("stash.conf"))
}

/// The comment line that names one borrow. Every `--stash` writes a fresh one,
/// including a `--stash` that keeps an existing snapshot, so that "is this still
/// the stash I read?" has an answer. Contents can't give one: the same lighting
/// stashed twice is the same bytes, and a reused snapshot is the same bytes by
/// definition. A comment, so the config grammar `load` reads is unchanged.
pub const STASH_ID_PREFIX: &str = "# stash-id:";

/// A fresh stash id: milliseconds since the epoch and the process id, which is
/// unique enough for something written a few times a day.
pub fn new_stash_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}-{}", std::process::id())
}

/// What identifies a stash's text: its id line, or — for one written before
/// stashes had ids — the whole text, which is the best a legacy stash offers.
pub fn stash_identity(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(STASH_ID_PREFIX).map(|id| id.trim().to_string()))
        .unwrap_or_else(|| text.to_string())
}

/// The stash on disk's identity, or `None` when there isn't one. The bridge
/// watches this; `restore` compares against it before each leg.
///
/// This reads what's there, whatever its age — judging a stash stale is the
/// CLI's job (`read_stash` in `main.rs`), and it deletes one on sight.
pub fn current_stash_identity() -> Option<String> {
    std::fs::read_to_string(stash_path()?).ok().map(|t| stash_identity(&t))
}

/// What happened when something tried to remove the stash.
#[derive(Debug, PartialEq, Eq)]
pub enum StashRemoval {
    Removed,
    /// There was no stash, or not the one asked about — someone else's now.
    LeftAlone,
    /// The state lock couldn't be had, so nothing was touched. Removing without
    /// it could delete a newer borrow written in between.
    Busy,
}

/// Remove the stash — for a bare apply, which makes what it sets the new
/// normal. Only under the state lock; without it, nothing is removed.
pub fn clear_stash() -> StashRemoval {
    match stash_path() {
        Some(p) => clear_stash_at(&p),
        None => StashRemoval::LeftAlone,
    }
}

fn clear_stash_at(path: &Path) -> StashRemoval {
    let Some(_lock) = lock_at(&path.with_file_name("state.lock")) else { return StashRemoval::Busy };
    match std::fs::remove_file(path) {
        Ok(()) => StashRemoval::Removed,
        Err(_) => StashRemoval::LeftAlone,
    }
}

/// Remove the stash only if it is still the one with identity `ours` — for
/// `restore`, which must not delete a newer borrow created while it ran. Under
/// the state lock, so the check and the removal can't be split by a writer.
pub fn remove_stash_if(ours: &str) -> StashRemoval {
    match stash_path() {
        Some(p) => remove_stash_if_at(&p, ours),
        None => StashRemoval::LeftAlone,
    }
}

fn remove_stash_if_at(path: &Path, ours: &str) -> StashRemoval {
    let Some(_lock) = lock_at(&path.with_file_name("state.lock")) else { return StashRemoval::Busy };
    match std::fs::read_to_string(path) {
        Ok(text) if stash_identity(&text) == ours => match std::fs::remove_file(path) {
            Ok(()) => StashRemoval::Removed,
            Err(_) => StashRemoval::LeftAlone,
        },
        _ => StashRemoval::LeftAlone,
    }
}

/// Give a stash a new id, keeping everything else — the colors it holds are the
/// first snapshot's, which is the point of keeping it.
pub fn restamp_stash(text: &str, id: &str) -> String {
    let line = format!("{STASH_ID_PREFIX} {id}");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with(STASH_ID_PREFIX) {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.insert(0, line);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Record what one device was just set to, leaving the other's slot untouched.
/// `gpu` selects the slot, matching the `Paint::rgb(gpu)` convention used
/// everywhere else.
///
/// Clearing the pattern here — including when `last` is `Multi` — is what keeps
/// a stale frame from outliving the state that produced it. A caller with a
/// pattern in hand calls `record_pattern`; one without is by definition not
/// describing a frame we could replay, so any frame on file is from an older
/// state and must go.
///
/// Recording `Off` over something lit keeps what was lit, for `jdrgb on` —
/// except in keep-lit mode (`keep_lit_records`), where lit records stay put.
pub fn record(gpu: bool, last: Last) {
    let keep = keeping_lit();
    update(|s| {
        if keep {
            s.record_keeping_lit(gpu, last)
        } else {
            s.record(gpu, last)
        }
    });
}

/// Record a per-LED frame: `Multi` for anything reading the colour slot, plus
/// the colours themselves so `jdrgb state` can reproduce it.
pub fn record_pattern(gpu: bool, colors: &[(u8, u8, u8)]) {
    update(|s| s.record_pattern(gpu, colors));
}

/// The last solid color set on one device, if there is one to resume from.
///
/// `Off` yields None alongside `Multi`: black is a useless place to start
/// tuning — nothing is lit, and HSL has no hue there to step through — so the
/// caller's default preset is a better answer than the literal last value.
pub fn load_last(gpu: bool) -> Option<(u8, u8, u8)> {
    match load_state_full().slot(gpu) {
        Some(Last::Color(rgb)) => Some(rgb),
        _ => None,
    }
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
        assert_eq!(p.rgb(true), (0xFF, 0x56, 0x0A)); // GPU, tuned by eye
    }

    #[test]
    fn default_preset_is_calibrated_for_both() {
        // Bare `jdrgb` and `jdrgb --gpu` must each get their own tuned white,
        // since the default is what install.ps1 uses when no color is given.
        let p = Paint::Preset(DEFAULT_PRESET);
        assert_eq!(p.rgb(false), (0xFF, 0xB0, 0xD0));
        assert_eq!(p.rgb(true), (0xD2, 0x94, 0x32));
        assert_ne!(p.rgb(false), p.rgb(true));
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
    fn every_swatch_names_a_real_preset() {
        for &(name, _) in SWATCH {
            assert!(lookup_preset(name).is_some(), "SWATCH has no matching preset: {name}");
        }
    }

    #[test]
    fn swatches_never_reach_a_device() {
        // The appearance table is display-only. If a preset's swatch ever became
        // its painted value, tuning the hardware would silently change what gets
        // sent — so assert the two stay independent where they differ.
        let p = Paint::Preset("coolwhite");
        assert_ne!(swatch_rgb("coolwhite"), p.rgb(false));
        assert_eq!(p.rgb(false), (0xFF, 0xB0, 0xD0));
        assert_eq!(p.rgb(true), (0xD2, 0x94, 0x32));
    }

    #[test]
    fn compound_names_split_on_a_base_that_is_itself_a_preset() {
        assert_eq!(display_name("burntorange"), "Burnt Orange");
        assert_eq!(display_name("coolwhite"), "Cool White");
        assert_eq!(display_name("warmwhite"), "Warm White");
        assert_eq!(display_name("seagreen"), "Sea Green");
        assert_eq!(display_name("hotpink"), "Hot Pink");
    }

    #[test]
    fn single_words_are_only_capitalised() {
        // `chartreuse` ends in "reuse" and `turquoise` in "oise"; neither is a
        // preset, so neither gets carved up.
        assert_eq!(display_name("red"), "Red");
        assert_eq!(display_name("chartreuse"), "Chartreuse");
        assert_eq!(display_name("turquoise"), "Turquoise");
        assert_eq!(display_name("cerise"), "Cerise");
    }

    /// The splitter is derived from the table, so the table can break it —
    /// adding a short preset could make an existing name split somewhere silly.
    /// Pinning every rendered name means that shows up here, not in the menu.
    #[test]
    fn every_preset_renders_the_way_it_should() {
        let rendered: Vec<String> = PRESETS.iter().map(|&(n, _)| display_name(n)).collect();
        assert_eq!(
            rendered,
            [
                "Cool White", "Warm White", "White", "Black", "Red", "Burnt Orange", "Orange",
                "Amber", "Yellow", "Chartreuse", "Lime", "Green", "Sea Green", "Teal",
                "Turquoise", "Cyan", "Sky", "Azure", "Cobalt", "Sapphire", "Blue", "Indigo",
                "Purple", "Violet", "Magenta", "Cerise", "Hot Pink", "Rose", "Pink",
            ]
        );
    }

    /// A display name is never something the CLI would accept, so nothing may
    /// quietly pass one back as an argument.
    #[test]
    fn a_display_name_round_trips_only_through_lowercase() {
        for &(name, _) in PRESETS {
            let shown = display_name(name);
            assert_eq!(shown.to_lowercase().replace(' ', ""), name);
        }
    }

    #[test]
    fn uncalibrated_swatch_falls_back_to_the_preset() {
        // `red` needs no appearance entry — it already looks like itself.
        assert_eq!(swatch_rgb("red"), (0xFF, 0x00, 0x00));
    }

    #[test]
    fn parses_names_and_hex() {
        assert!(matches!(parse_paint("WarmWhite"), Some(Paint::Preset("warmwhite"))));
        assert!(matches!(parse_paint("ff560a"), Some(Paint::Literal((0xFF, 0x56, 0x0A)))));
        assert!(matches!(parse_paint("#FF560A"), Some(Paint::Literal((0xFF, 0x56, 0x0A)))));
        assert!(parse_paint("nonsense").is_none());
    }

    #[test]
    fn default_preset_resolves() {
        assert_eq!(Paint::Preset(DEFAULT_PRESET).rgb(false), DEFAULT_COLOR);
    }

    /// `preset_for_rgb` answers with the first exact match, so a duplicated
    /// value would make the tray name a color after whichever preset happened
    /// to sort first. Distinctness is also just what a palette should have.
    #[test]
    fn preset_values_are_distinct() {
        for (i, &(a, va)) in PRESETS.iter().enumerate() {
            for &(b, vb) in &PRESETS[i + 1..] {
                assert_ne!(va, vb, "{a} and {b} are the same color");
            }
        }
    }

    #[test]
    fn reverse_lookup_round_trips_every_preset() {
        for &(name, _) in PRESETS {
            let p = Paint::Preset(name);
            assert_eq!(preset_for_rgb(p.rgb(false), false), Some(name), "strip: {name}");
            assert_eq!(preset_for_rgb(p.rgb(true), true), Some(name), "gpu: {name}");
        }
    }

    #[test]
    fn reverse_lookup_rejects_the_other_devices_value() {
        // warmwhite is FA9536 on the strip and FF560A on the card. A GPU slot
        // holding FA9536 is not warmwhite — it's an unrecognised value that
        // would render as something else entirely there.
        assert_eq!(preset_for_rgb((0xFA, 0x95, 0x36), false), Some("warmwhite"));
        assert_eq!(preset_for_rgb((0xFA, 0x95, 0x36), true), None);
        // `red` has no override, so it is genuinely the same on both.
        assert_eq!(preset_for_rgb((0xFF, 0x00, 0x00), true), Some("red"));
    }

    #[test]
    fn reverse_lookup_declines_a_hand_tuned_value() {
        assert_eq!(preset_for_rgb((0x12, 0x34, 0x56), false), None);
    }

    /// The two colour slots, with no patterns — the shape most of these
    /// assertions are about.
    fn slots(mb: Option<Last>, gpu: Option<Last>) -> State {
        State { mb, gpu, ..State::default() }
    }

    #[test]
    fn state_slots_are_independent() {
        // The whole point of the split: the same look is a different RGB on each
        // device, so one slot must never answer for the other.
        let text = encode_state(&slots(
            Some(Last::Color((0xFF, 0xB0, 0xD0))),
            Some(Last::Color((0xD2, 0x94, 0x32))),
        ));
        let s = parse_state(&text);
        assert_eq!(s.mb, Some(Last::Color((0xFF, 0xB0, 0xD0))));
        assert_eq!(s.gpu, Some(Last::Color((0xD2, 0x94, 0x32))));
    }

    #[test]
    fn recording_one_slot_leaves_the_other() {
        // record() is read-modify-write; this is the encode half of that.
        let mut s = parse_state(&encode_state(&slots(Some(Last::Color((1, 2, 3))), None)));
        s.gpu = Some(Last::Color((4, 5, 6)));
        let text = encode_state(&s);
        assert_eq!(parse_state(&text), slots(Some(Last::Color((1, 2, 3))), Some(Last::Color((4, 5, 6)))));
    }

    #[test]
    fn legacy_state_reads_as_motherboard() {
        // Files written before the split hold a bare hex, and only the strip
        // ever wrote one — so it must not be handed to the GPU.
        assert_eq!(parse_state("FFB0D0\n"), slots(Some(Last::Color((0xFF, 0xB0, 0xD0))), None));
        assert_eq!(parse_state("multi"), slots(Some(Last::Multi), None));
    }

    #[test]
    fn unrecorded_slot_is_distinct_from_multi() {
        // Never-set falls back to the device's default; multi-colored has no
        // single color to resume from. Both yield None from load_last, but the
        // file has to keep them apart or a `load` would look like a fresh state.
        let s = parse_state(&encode_state(&slots(Some(Last::Multi), None)));
        assert_eq!(s.gpu, None);
        assert_eq!(s.mb, Some(Last::Multi));
    }

    /// `off` and `black` put the same bytes on the wire but leave the
    /// controller in different modes, so the file has to keep them apart. A
    /// reader that saw only the color would report `black` for a device that
    /// is actually off.
    #[test]
    fn off_survives_the_round_trip_as_itself() {
        let text = encode_state(&slots(Some(Last::Off), Some(Last::Color((0, 0, 0)))));
        let s = parse_state(&text);
        assert_eq!(s.mb, Some(Last::Off));
        assert_eq!(s.gpu, Some(Last::Color((0, 0, 0))));
        assert_ne!(s.mb, s.gpu);
    }

    #[test]
    fn a_pattern_survives_the_round_trip() {
        // The reason the pattern keys exist: `multi` alone says only "no single
        // colour", which cannot be replayed.
        let frame = vec![(0xFA, 0x95, 0x36), (0xFF, 0x00, 0x11), (0x00, 0x00, 0x00)];
        let s = State {
            mb: Some(Last::Multi),
            mb_pattern: Some(frame.clone()),
            ..State::default()
        };
        let back = parse_state(&encode_state(&s));
        assert_eq!(back, s);
        assert_eq!(back.pattern(false), Some(&frame[..]));
        assert_eq!(back.pattern(true), None, "the strip's frame answered for the card");
    }

    /// The invariant that makes a stale frame impossible: only `record_pattern`
    /// writes one, and any other write to that slot clears it. This is the pure
    /// half — `record`'s edit, without the file I/O.
    #[test]
    fn a_solid_clears_the_frame_that_preceded_it() {
        let mut s = State {
            mb: Some(Last::Multi),
            mb_pattern: Some(vec![(1, 2, 3)]),
            gpu: Some(Last::Multi),
            gpu_pattern: Some(vec![(4, 5, 6)]),
            ..State::default()
        };
        s.record(false, Last::Color((0xFF, 0x00, 0x00)));
        let back = parse_state(&encode_state(&s));
        assert_eq!(back.pattern(false), None, "a frame outlived the state that produced it");
        assert_eq!(back.pattern(true), Some(&[(4, 5, 6)][..]), "cleared the other device's frame");
    }

    #[test]
    fn a_corrupt_frame_is_dropped_whole() {
        // Restoring a frame with one wrong LED in it would be worse than
        // restoring nothing, because nothing is visibly nothing.
        let s = parse_state("mb=multi\nmb.pattern=FA9536,nonsense,FF0011\n");
        assert_eq!(s.mb, Some(Last::Multi));
        assert_eq!(s.mb_pattern, None);
    }

    /// Neither `off` nor `multi` offers a color to resume tuning from, and
    /// black is a useless starting point besides — no hue to step through.
    #[test]
    fn off_offers_nothing_to_resume_from() {
        assert_eq!(Last::decode("off"), Some(Last::Off));
        assert_eq!(Last::decode("OFF"), Some(Last::Off));
        assert_eq!(Last::Off.encode(), "off");
    }

    #[test]
    fn garbage_state_is_ignored_not_fatal() {
        assert_eq!(parse_state("mb=nonsense\ngpu=\n"), State::default());
        assert_eq!(parse_state(""), State::default());
    }

    const WARM: (u8, u8, u8) = (0xFA, 0x95, 0x36);
    const AMBER: (u8, u8, u8) = (0xFF, 0x87, 0x00);

    /// The regression: `off` overwrote the only record of what was showing, so
    /// nothing could turn the lights back on to it. It has to survive the trip
    /// through the file, too — the next `jdrgb on` is a different process.
    #[test]
    fn turning_off_keeps_what_was_showing() {
        let mut s = slots(Some(Last::Color(WARM)), Some(Last::Color((0xFF, 0x56, 0x0A))));
        s.record(false, Last::Off);
        s.record(true, Last::Off);
        let back = parse_state(&encode_state(&s));
        assert_eq!(back.mb, Some(Last::Off));
        assert_eq!(back.lit(false), Some(&Lit::Color(WARM)));
        assert_eq!(back.lit(true), Some(&Lit::Color((0xFF, 0x56, 0x0A))), "the card kept the strip's colour");
    }

    #[test]
    fn turning_off_keeps_a_per_led_frame() {
        let frame = vec![WARM, (0xFF, 0x00, 0x11), (0, 0, 0)];
        let mut s = State::default();
        s.record_pattern(false, &frame);
        s.record(false, Last::Off);
        let back = parse_state(&encode_state(&s));
        assert_eq!(back.lit(false), Some(&Lit::Frame(frame)));
        assert_eq!(back.pattern(false), None, "the live frame outlived the off");
    }

    /// Pressing off twice is the normal case for a switch. The second press is
    /// a move from off to off, and must not replace the colour with nothing.
    #[test]
    fn a_second_off_keeps_the_first_ones_colour() {
        let mut s = slots(Some(Last::Color(WARM)), None);
        s.record(false, Last::Off);
        s.record(false, Last::Off);
        assert_eq!(s.lit(false), Some(&Lit::Color(WARM)));
    }

    /// Lights off; a UPS hook borrows them with `--stash`; mains restores the
    /// stash, which says `off`. That last step is a move from lit into off, so
    /// on its own it records the *borrowed* amber — and the next `on` comes up
    /// amber instead of the colour the room was turned off from.
    #[test]
    fn putting_back_off_keeps_what_on_brings_back() {
        let mut s = slots(Some(Last::Color(WARM)), None);
        s.record(false, Last::Off);
        s.record(false, Last::Color(AMBER)); // borrowed

        let mut plain = s.clone();
        plain.record(false, Last::Off);
        assert_eq!(plain.lit(false), Some(&Lit::Color(AMBER)), "precondition: off alone records the borrow");

        s.record_keeping_lit(false, Last::Off); // the stash, put back by `restore`
        assert_eq!(s.lit(false), Some(&Lit::Color(WARM)));
        assert_eq!(s.mb, Some(Last::Off));
    }

    // ---- the state file on disk ------------------------------------------

    /// A directory of the test's own, so nothing here touches the real
    /// `%LOCALAPPDATA%\jdrgb`.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jdrgb-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("last")
    }

    #[test]
    fn rev_round_trips_and_a_fresh_state_writes_none() {
        assert!(!encode_state(&State::default()).contains("rev="));
        let s = State { rev: 42, ..slots(Some(Last::Off), None) };
        assert_eq!(parse_state(&encode_state(&s)).rev, 42);
        assert_eq!(parse_state("rev=nonsense\nmb=off\n").rev, 0, "garbage reads as never written");
    }

    /// The reason `rev` exists: a write that repeats the same values is still
    /// a write, and someone watching for "anyone acted" must see it.
    #[test]
    fn every_update_bumps_rev_even_when_nothing_changed() {
        let path = scratch("rev");
        for _ in 0..3 {
            assert!(update_at(&path, |s| s.record(false, Last::Color(WARM))));
        }
        let s = load_state_at(&path);
        assert_eq!(s.rev, 3);
        assert_eq!(s.mb, Some(Last::Color(WARM)));
    }

    /// Two processes recording different devices used to be able to lose one
    /// another's update: both read, both edited, the second write won. Under
    /// the lock, every write lands — which `rev` counts exactly.
    #[test]
    fn concurrent_records_lose_nothing() {
        let path = scratch("concurrent");
        let writers: Vec<_> = (0..4)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..25u8 {
                        let gpu = t % 2 == 1;
                        assert!(update_at(&path, |s| s.record(gpu, Last::Color((t as u8, i, 0)))));
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let s = load_state_at(&path);
        assert_eq!(s.rev, 100, "an update was lost");
        assert!(s.mb.is_some() && s.gpu.is_some(), "a device's record was lost: {s:?}");
    }

    /// A reader polling while writes land must only ever see a whole file. The
    /// old truncate-then-write could hand it an empty one, which reads as
    /// "nothing recorded" — the bridge would have published unknown.
    #[test]
    fn a_reader_never_sees_a_torn_file() {
        let path = scratch("torn");
        assert!(update_at(&path, |s| s.record(false, Last::Color(WARM))));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut reads = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let s = parse_state(&text);
                        assert!(s.rev > 0 && s.mb.is_some(), "read a partial file: {text:?}");
                        reads += 1;
                    }
                }
                reads
            })
        };
        for i in 0..200u8 {
            assert!(update_at(&path, |s| s.record(false, Last::Color((i, 0x95, 0x36)))));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0, "the reader never got a read in");
    }

    #[test]
    fn a_held_state_lock_turns_a_writer_away_rather_than_hanging() {
        let path = scratch("held");
        let held = lock_at(&path.with_file_name("state.lock")).expect("lock");
        let started = std::time::Instant::now();
        assert!(!update_at(&path, |s| s.record(false, Last::Off)), "wrote through a held lock");
        assert!(started.elapsed() < STATE_LOCK_WAIT * 3, "waited far longer than the bound");
        drop(held);
        assert!(update_at(&path, |s| s.record(false, Last::Off)), "the lock wasn't released");
    }

    // ---- brightness ------------------------------------------------------

    #[test]
    fn scale_is_identity_at_100_and_rounds_half_up() {
        assert_eq!(scale(WARM, 100), WARM);
        assert_eq!(scale((255, 255, 255), 50), (128, 128, 128), "127.5 rounds up");
        assert_eq!(scale((255, 176, 208), 2), (5, 4, 4), "coolwhite at 2% — why MIN_LEVEL exists");
        assert_eq!(scale((10, 20, 30), 0), (0, 0, 0));
        assert_eq!(scale(WARM, 200), WARM, "a level above 100 is clamped, never brightened");
    }

    /// Calibrate, then dim. Dimming the shared value and calibrating after
    /// would hand the card the strip's color at 40%.
    #[test]
    fn a_dimmed_preset_is_each_devices_own_value_dimmed() {
        let p = Paint::Preset("warmwhite");
        assert_eq!(scale(p.rgb(true), 40), scale((0xFF, 0x56, 0x0A), 40));
        assert_ne!(scale(p.rgb(true), 40), scale(p.rgb(false), 40));
    }

    /// What `MIN_LEVEL` rests on: from it up, no wire value on either device
    /// could have come from two different presets, at any pair of levels. Below
    /// it, recognising which preset a dimmed value was stops being possible.
    #[test]
    fn no_two_presets_share_a_wire_value_from_min_level_up() {
        for gpu in [false, true] {
            let mut seen: std::collections::HashMap<(u8, u8, u8), &str> = Default::default();
            for &(name, _) in PRESETS {
                let rgb = Paint::Preset(name).rgb(gpu);
                for level in MIN_LEVEL..=100 {
                    if let Some(other) = seen.insert(scale(rgb, level), name) {
                        assert_eq!(other, name, "{name} and {other} collide at {level}% (gpu={gpu})");
                    }
                }
            }
        }
    }

    /// And the floor is not lower than it needs to be: one step down, the
    /// collision that set it is real.
    #[test]
    fn one_below_min_level_two_presets_collide() {
        let cerise = Paint::Preset("cerise").rgb(true);
        let hotpink = Paint::Preset("hotpink").rgb(true);
        assert_eq!(scale(cerise, MIN_LEVEL - 1), scale(hotpink, MIN_LEVEL - 1));
    }

    // ---- recognising what a recorded value was --------------------------

    /// Exhaustively: every preset at every level on both devices comes back as
    /// a pair that produces the same bytes, and from `MIN_LEVEL` up as itself,
    /// at its own level. The bridge reports state from this, so a wrong answer
    /// here would be a wrong light in Home Assistant.
    #[test]
    fn every_dimmed_preset_is_recognised() {
        for gpu in [false, true] {
            for &(name, _) in PRESETS {
                let full = Paint::Preset(name).rgb(gpu);
                for level in 1..=100u8 {
                    let wire = scale(full, level);
                    let (got, at) = recognise(wire, gpu).unwrap_or_else(|| panic!("{name}@{level} unrecognised"));
                    assert_eq!(scale(Paint::Preset(got).rgb(gpu), at), wire, "{name}@{level} → {got}@{at}");
                    if level >= MIN_LEVEL {
                        assert_eq!(got, name, "{name}@{level} (gpu={gpu}) read back as {got}@{at}");
                        if name != "black" {
                            assert_eq!(at, level, "{name}@{level} (gpu={gpu}) read back at {at}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_undimmed_preset_is_itself_at_full_and_a_tuned_hex_is_nothing() {
        assert_eq!(recognise(Paint::Preset("warmwhite").rgb(true), true), Some(("warmwhite", 100)));
        assert_eq!(recognise((0, 0, 0), false), Some(("black", 100)));
        assert_eq!(recognise((0x12, 0x34, 0x56), false), None);
        // The strip's warm white on the card is not the card's warm white.
        assert_ne!(recognise(Paint::Preset("warmwhite").rgb(false), true).map(|r| r.0), Some("warmwhite"));
    }

    // ---- the color wheel ---------------------------------------------------

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn hsv_of_the_primaries_and_white() {
        assert_eq!(hsv_of((255, 0, 0)), (0.0, 1.0));
        let (h, s) = hsv_of((0, 255, 0));
        assert!(near(h, 120.0) && s == 1.0);
        let (h, s) = hsv_of((0, 0, 255));
        assert!(near(h, 240.0) && s == 1.0);
        assert_eq!(hsv_of((255, 255, 255)), (0.0, 0.0));
        assert_eq!(hsv_of((0, 0, 0)), (0.0, 0.0));
        let (_, s) = hsv_of(swatch_rgb("coolwhite"));
        assert!(s < 0.05, "coolwhite looks nearly unsaturated: {s}");
    }

    /// Every preset the wheel can land on must be reachable by pointing at
    /// where it looks like it is — otherwise HomeKit's thumb could never sit on
    /// it, and HA would echo a color the user can't pick.
    #[test]
    fn every_eligible_preset_snaps_to_itself() {
        for &(name, _) in PRESETS {
            if name == "black" || name == "white" {
                continue;
            }
            let (h, s) = appearance_hs(name);
            assert_eq!(snap_hs(h, s * 100.0), name, "pointing at {name} ({h:.1}°, {:.0}%)", s * 100.0);
        }
    }

    #[test]
    fn the_wheel_never_lands_on_black_or_white() {
        for h in (0..360).step_by(5) {
            for s in [0, 10, 50, 100] {
                let got = snap_hs(f64::from(h), f64::from(s));
                assert!(got != "black" && got != "white", "{h}°/{s}% landed on {got}");
            }
        }
        assert_eq!(snap_hs(0.0, 0.0), "coolwhite", "the centre of the wheel is the cleanest white");
    }

    /// The house's Synthwave Dreams scene sets hue 323; Hot Pink (~330) is a
    /// hair nearer than Cerise (~315). The scene names its effect directly, but
    /// the wheel should agree.
    #[test]
    fn hue_323_snaps_to_hot_pink() {
        assert_eq!(snap_hs(323.0, 100.0), "hotpink");
    }

    #[test]
    fn kelvin_splits_between_the_whites_in_mireds() {
        assert_eq!(snap_kelvin(2700), "warmwhite");
        assert_eq!(snap_kelvin(6500), "coolwhite");
        assert_eq!(snap_kelvin(3800), "warmwhite", "just warm of the ~3815 K split");
        assert_eq!(snap_kelvin(3830), "coolwhite", "just cool of it");
        assert_eq!(snap_kelvin(4255), "coolwhite", "the house's Bright scene");
        assert_eq!(snap_kelvin(2000), "warmwhite");
        assert_eq!(snap_kelvin(10_000), "coolwhite");
        assert_eq!(kelvin_of("warmwhite"), Some(WARM_KELVIN));
        assert_eq!(kelvin_of("red"), None);
    }

    // ---- stash identity ---------------------------------------------------

    #[test]
    fn restamping_keeps_the_snapshot_and_replaces_only_the_id() {
        let stash = "# jdrgb stash\n# stash-id: 1-1\n\nmb: FA9536   # warmwhite\ngpu: off\n";
        let again = restamp_stash(stash, "2-2");
        assert_eq!(stash_identity(&again), "2-2");
        assert!(again.contains("mb: FA9536") && again.contains("gpu: off"), "{again}");
        assert_eq!(again.matches(STASH_ID_PREFIX).count(), 1, "{again}");
    }

    /// A stash written before ids existed gets one on its first re-stamp, and
    /// until then is identified by its whole text.
    #[test]
    fn a_legacy_stash_is_identified_by_its_contents_until_stamped() {
        let legacy = "# jdrgb stash\n\nmb: off\n";
        assert_eq!(stash_identity(legacy), legacy);
        let stamped = restamp_stash(legacy, "3-3");
        assert_eq!(stash_identity(&stamped), "3-3");
        assert!(stamped.contains("mb: off"));
    }

    /// A rename that won't go must leave the original exactly as it was — no
    /// in-place fallback, which could tear it or, failing part-way, destroy it.
    /// A reader that doesn't share delete is what makes the rename fail.
    #[test]
    fn a_failed_replace_leaves_the_original_untouched() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = scratch("noreplace");
        std::fs::write(&path, "mb=FA9536\n").unwrap();
        let blocker = std::fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ only */).open(&path).unwrap();
        assert!(!write_atomic(&path, "mb=off\n"), "replaced a file a reader holds without delete sharing");
        drop(blocker);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mb=FA9536\n");
        assert!(!path.with_extension("tmp").exists(), "left its temporary file behind");
    }

    /// Removing the stash without the lock could delete a newer borrow; with
    /// the lock held elsewhere, both kinds of removal must leave it alone.
    #[test]
    fn stash_removal_needs_the_lock() {
        let path = scratch("stashlock").with_file_name("stash.conf");
        std::fs::write(&path, "# stash-id: 1-1\nmb: off\n").unwrap();
        let held = lock_at(&path.with_file_name("state.lock")).expect("lock");
        assert_eq!(clear_stash_at(&path), StashRemoval::Busy);
        assert_eq!(remove_stash_if_at(&path, "1-1"), StashRemoval::Busy);
        assert!(path.exists(), "removed through someone else's lock");
        drop(held);
        assert_eq!(remove_stash_if_at(&path, "2-2"), StashRemoval::LeftAlone, "not ours: a newer borrow");
        assert!(path.exists());
        assert_eq!(remove_stash_if_at(&path, "1-1"), StashRemoval::Removed);
        assert!(!path.exists());
        assert_eq!(clear_stash_at(&path), StashRemoval::LeftAlone, "nothing there");
    }

    #[test]
    fn identical_lighting_stashed_twice_is_two_identities() {
        let body = "mb: FA9536\n";
        let a = restamp_stash(body, "10-1");
        let b = restamp_stash(body, "11-1");
        assert_ne!(stash_identity(&a), stash_identity(&b));
    }

    /// A lit record that can't be replayed is no record. `on` falls back to the
    /// default for it, which beats replaying something wrong.
    #[test]
    fn an_unreplayable_lit_record_reads_as_none() {
        assert_eq!(parse_state("mb=off\nmb.lit=off\n").mb_lit, None);
        assert_eq!(parse_state("mb=off\nmb.lit=multi\n").mb_lit, None, "a multi without its frame");
        assert_eq!(parse_state("mb=off\nmb.lit=multi\nmb.lit.pattern=FA9536,nonsense\n").mb_lit, None);
        // Order in the file doesn't matter: the pattern may come first.
        assert_eq!(
            parse_state("mb.lit.pattern=FA9536,000000\nmb.lit=multi\n").mb_lit,
            Some(Lit::Frame(vec![WARM, (0, 0, 0)]))
        );
    }
}
