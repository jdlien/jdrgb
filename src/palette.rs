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

/// The two devices keep separate slots. Sharing one would be actively wrong:
/// the same look is a *different* RGB on each (see GPU_PRESETS), so carrying the
/// strip's last value onto the card hands `tune` a starting point that doesn't
/// render as anything like what was on screen.
///
/// Format is one `dev=value` per line. A legacy file — a bare hex, from before
/// the split — is read as the motherboard's, since that's the only device that
/// ever wrote one.
pub fn parse_state(text: &str) -> (Option<Last>, Option<Last>) {
    let text = text.trim();
    if !text.contains('=') {
        return (Last::decode(text), None);
    }
    let (mut mb, mut gpu) = (None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "mb" => mb = Last::decode(value.trim()),
            "gpu" => gpu = Last::decode(value.trim()),
            _ => {}
        }
    }
    (mb, gpu)
}

pub fn load_state() -> (Option<Last>, Option<Last>) {
    match state_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => parse_state(&text),
        None => (None, None),
    }
}

pub fn encode_state(mb: Option<Last>, gpu: Option<Last>) -> String {
    let mut out = String::new();
    if let Some(v) = mb {
        out.push_str(&format!("mb={}\n", v.encode()));
    }
    if let Some(v) = gpu {
        out.push_str(&format!("gpu={}\n", v.encode()));
    }
    out
}

/// Record what one device was just set to, leaving the other's slot untouched.
/// `gpu` selects the slot, matching the `Paint::rgb(gpu)` convention used
/// everywhere else. Best-effort; failures are ignored.
///
/// Only the process that actually wrote to a device calls this — which is always
/// `jdrgb.exe`. The tray reads this file but never writes it.
pub fn record(gpu: bool, last: Last) {
    let Some(path) = state_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let (old_mb, old_gpu) = load_state();
    let (mb, gpu_slot) = if gpu { (old_mb, Some(last)) } else { (Some(last), old_gpu) };
    let _ = std::fs::write(path, encode_state(mb, gpu_slot));
}

/// The last solid color set on one device, if there is one to resume from.
///
/// `Off` yields None alongside `Multi`: black is a useless place to start
/// tuning — nothing is lit, and HSL has no hue there to step through — so the
/// caller's default preset is a better answer than the literal last value.
pub fn load_last(gpu: bool) -> Option<(u8, u8, u8)> {
    let (mb, gpu_slot) = load_state();
    match if gpu { gpu_slot } else { mb } {
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

    #[test]
    fn state_slots_are_independent() {
        // The whole point of the split: the same look is a different RGB on each
        // device, so one slot must never answer for the other.
        let text = encode_state(Some(Last::Color((0xFF, 0xB0, 0xD0))), Some(Last::Color((0xD2, 0x94, 0x32))));
        let (mb, gpu) = parse_state(&text);
        assert_eq!(mb, Some(Last::Color((0xFF, 0xB0, 0xD0))));
        assert_eq!(gpu, Some(Last::Color((0xD2, 0x94, 0x32))));
    }

    #[test]
    fn recording_one_slot_leaves_the_other() {
        // record() is read-modify-write; this is the encode half of that.
        let (mb, _) = parse_state(&encode_state(Some(Last::Color((1, 2, 3))), None));
        let text = encode_state(mb, Some(Last::Color((4, 5, 6))));
        assert_eq!(parse_state(&text), (Some(Last::Color((1, 2, 3))), Some(Last::Color((4, 5, 6)))));
    }

    #[test]
    fn legacy_state_reads_as_motherboard() {
        // Files written before the split hold a bare hex, and only the strip
        // ever wrote one — so it must not be handed to the GPU.
        assert_eq!(parse_state("FFB0D0\n"), (Some(Last::Color((0xFF, 0xB0, 0xD0))), None));
        assert_eq!(parse_state("multi"), (Some(Last::Multi), None));
    }

    #[test]
    fn unrecorded_slot_is_distinct_from_multi() {
        // Never-set falls back to the device's default; multi-colored has no
        // single color to resume from. Both yield None from load_last, but the
        // file has to keep them apart or a `load` would look like a fresh state.
        let (_, gpu) = parse_state(&encode_state(Some(Last::Multi), None));
        assert_eq!(gpu, None);
        let (mb, _) = parse_state(&encode_state(Some(Last::Multi), None));
        assert_eq!(mb, Some(Last::Multi));
    }

    /// `off` and `black` put the same bytes on the wire but leave the
    /// controller in different modes, so the file has to keep them apart. A
    /// reader that saw only the color would report `black` for a device that
    /// is actually off.
    #[test]
    fn off_survives_the_round_trip_as_itself() {
        let text = encode_state(Some(Last::Off), Some(Last::Color((0, 0, 0))));
        let (mb, gpu) = parse_state(&text);
        assert_eq!(mb, Some(Last::Off));
        assert_eq!(gpu, Some(Last::Color((0, 0, 0))));
        assert_ne!(mb, gpu);
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
        assert_eq!(parse_state("mb=nonsense\ngpu=\n"), (None, None));
        assert_eq!(parse_state(""), (None, None));
    }
}
