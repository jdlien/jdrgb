//! Circle swatches, drawn a pixel at a time.
//!
//! The menu needs a colored dot per preset and the notification area needs the
//! same dot as an icon. Both are a distance test over a small square — no GDI+,
//! no Direct2D, and no image files anywhere in the project.
//!
//! Everything here produces 32bpp **premultiplied** BGRA, top-down. Premultiplied
//! because that is what GDI's alpha compositing expects; skip it and the
//! antialiased rim picks up a bright halo against dark menus.

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::{copy_nonoverlapping, null_mut};

use windows_sys::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    Solid((u8, u8, u8)),
    /// `off`, and "we don't know yet": a ring around nothing.
    ///
    /// A filled circle cannot express this. `off` and the `black` preset would
    /// both be a black disc, and they are genuinely different controller states
    /// — so the one that means "dark" gets a shape, not a color.
    Empty,
}

/// Relative luminance, good enough to decide light-on-dark.
fn luma((r, g, b): (u8, u8, u8)) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

/// Saturation, as the plain max-minus-min kind. Enough to tell a colour from a
/// grey, which is all this needs to do.
fn chroma((r, g, b): (u8, u8, u8)) -> f32 {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    (r.max(g).max(b) - r.min(g).min(b)) / 255.0
}

// --- Rim policy ------------------------------------------------------------
//
// Which swatches get an outline. Bare discs look better, so the default is
// none: menu backgrounds are neutral greys, and a saturated colour reads
// cleanly against a neutral of *any* lightness. An RGB strip set to grey isn't
// really a thing, so most of the palette is never at risk.
//
// The exceptions are the near-neutrals at the ends of the range: `white` and
// `coolwhite` dissolve into a light menu, `black` into a dark one. Those need
// an outline or they read as a hole.
//
// To change the policy, these three numbers are the whole of it. Rim
// everything: set MAX_CHROMA above 1.0. Rim nothing: set it to 0.0.
const RIM_MAX_CHROMA: f32 = 0.25;
const RIM_LUMA_HI: f32 = 0.85;
const RIM_LUMA_LO: f32 = 0.12;

/// True when a fill is close enough to neutral, and far enough towards one end
/// of the range, to disappear into a menu background.
fn needs_rim(rgb: (u8, u8, u8)) -> bool {
    let outside_the_safe_band = !(RIM_LUMA_LO..=RIM_LUMA_HI).contains(&luma(rgb));
    chroma(rgb) < RIM_MAX_CHROMA && outside_the_safe_band
}

/// The rim colour for a given fill, or the fill itself where no rim is wanted —
/// blending a colour towards itself is a no-op, so the drawing loop needs no
/// special case and the antialiased edge stays identical either way.
fn rim(s: Swatch) -> (u8, u8, u8) {
    match s {
        // No fill to take a cue from, so a mid grey that reads on both.
        Swatch::Empty => (0x9A, 0x9A, 0x9A),
        Swatch::Solid(rgb) if !needs_rim(rgb) => rgb,
        // Light fills get a dark rim and dark fills a light one, so whichever
        // end the background is at, the outline is at the other.
        Swatch::Solid(rgb) if luma(rgb) > 0.5 => (0x3A, 0x3A, 0x3A),
        Swatch::Solid(_) => (0xCF, 0xCF, 0xCF),
    }
}

/// Premultiplied BGRA pixels for one swatch, top-down, `size` x `size`.
pub fn pixels(size: i32, s: Swatch) -> Vec<u32> {
    let mut px = vec![0u32; (size * size) as usize];

    let c = (size as f32 - 1.0) / 2.0;
    let r_out = size as f32 / 2.0 - 0.5;
    // Thin enough to read as an outline rather than a donut, but never sub-pixel
    // at 100% DPI, where 16px leaves no room to be subtle.
    let r_in = r_out - (size as f32 * 0.09).max(1.0);

    let fill = match s {
        Swatch::Solid(rgb) => rgb,
        Swatch::Empty => (0, 0, 0),
    };
    let (rr, rg, rb) = rim(s);
    let (fr, fg, fb) = fill;

    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();

            // Coverage of the disc, fading over the outermost pixel.
            let cov = (r_out + 0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            // 0 well inside the fill, 1 out in the rim, blended across one pixel.
            let t = (d - r_in + 0.5).clamp(0.0, 1.0);

            let mix = |f: u8, r: u8| f as f32 + (r as f32 - f as f32) * t;
            let a = match s {
                Swatch::Solid(_) => cov,
                // Hollow: only the rim is drawn at all.
                Swatch::Empty => cov * t,
            };

            let pm = |v: f32| (v * a).round().clamp(0.0, 255.0) as u32;
            px[(y * size + x) as usize] = ((a * 255.0).round() as u32) << 24
                | pm(mix(fr, rr)) << 16
                | pm(mix(fg, rg)) << 8
                | pm(mix(fb, rb));
        }
    }
    px
}

/// Wrap premultiplied pixels in a DIB section suitable for `hbmpItem`.
///
/// # Safety
/// Returns an owned `HBITMAP`; the caller must `DeleteObject` it.
pub unsafe fn dib(size: i32, px: &[u32]) -> HBITMAP {
    let mut bi: BITMAPINFO = unsafe { zeroed() };
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size,
        biHeight: -size, // negative = top-down, matching `pixels`
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { zeroed() }
    };

    let mut bits: *mut c_void = null_mut();
    let bmp = unsafe {
        CreateDIBSection(null_mut(), &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0)
    };
    if !bmp.is_null() && !bits.is_null() {
        unsafe { copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len()) };
    }
    bmp
}

/// The 1bpp AND mask for an alpha icon: set bits are transparent.
///
/// A 32bpp icon renders from its alpha channel, so this is a fallback — but an
/// all-zero mask claims the whole square is opaque, and anything that consults
/// the mask on its own then draws a black box. Deriving it from alpha costs
/// nothing and keeps the two consistent.
///
/// Row order doesn't matter here even though `CreateBitmap`'s is ambiguous: a
/// circle is symmetric about its horizontal axis.
unsafe fn and_mask(size: i32, px: &[u32]) -> HBITMAP {
    let stride = (((size + 15) / 16) * 2) as usize; // rows are WORD-aligned
    let mut bits = vec![0u8; stride * size as usize];
    for y in 0..size {
        for x in 0..size {
            if px[(y * size + x) as usize] >> 24 < 128 {
                bits[y as usize * stride + (x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
    }
    unsafe { CreateBitmap(size, size, 1, 1, bits.as_ptr() as *const c_void) }
}

/// Build a tray icon from premultiplied pixels.
///
/// # Safety
/// Returns an owned `HICON`; the caller must `DestroyIcon` it.
pub unsafe fn icon(size: i32, px: &[u32]) -> HICON {
    let color = unsafe { dib(size, px) };
    let mask = unsafe { and_mask(size, px) };

    let ii = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let h = unsafe { CreateIconIndirect(&ii) };

    // CreateIconIndirect copies both bitmaps, so they are ours to free straight
    // away — keeping them would leak two GDI objects per icon update.
    unsafe {
        DeleteObject(color as _);
        DeleteObject(mask as _);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(px: &[u32], size: i32, x: i32, y: i32) -> (u8, u8, u8, u8) {
        let v = px[(y * size + x) as usize];
        (
            (v >> 24) as u8,
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
        )
    }

    #[test]
    fn corners_are_transparent() {
        // A circle inscribed in the square must not paint its corners, or the
        // swatch reads as a block and the menu looks broken.
        for size in [16, 20, 24, 32] {
            let px = pixels(size, Swatch::Solid((0xFF, 0x00, 0x00)));
            for (x, y) in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1)] {
                assert_eq!(at(&px, size, x, y).0, 0, "size {size} corner ({x},{y})");
            }
        }
    }

    #[test]
    fn centre_is_the_fill_colour() {
        let size = 24;
        let px = pixels(size, Swatch::Solid((0x12, 0x34, 0x56)));
        let (a, r, g, b) = at(&px, size, size / 2, size / 2);
        assert_eq!(a, 255, "centre must be opaque");
        assert_eq!((r, g, b), (0x12, 0x34, 0x56));
    }

    #[test]
    fn edge_pixels_are_premultiplied() {
        // The invariant that matters: no channel may exceed alpha, or GDI
        // composites a halo. White is the worst case.
        for size in [16, 24, 32] {
            let px = pixels(size, Swatch::Solid((0xFF, 0xFF, 0xFF)));
            for (i, &v) in px.iter().enumerate() {
                let (a, r, g, b) = ((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8);
                assert!(
                    r <= a && g <= a && b <= a,
                    "size {size} px {i}: {r},{g},{b} exceeds alpha {a}"
                );
            }
        }
    }

    #[test]
    fn a_white_swatch_has_a_dark_rim() {
        // Without this, `white` and `coolwhite` are invisible on a light menu.
        let size = 24;
        let px = pixels(size, Swatch::Solid((0xFF, 0xFF, 0xFF)));
        let (_, r, _, _) = at(&px, size, size / 2, size / 2);
        // Walk out along the horizontal radius; the rim must be darker than fill.
        let rim_px = (0..size)
            .map(|x| at(&px, size, x, size / 2))
            .filter(|&(a, ..)| a == 255)
            .map(|(_, r, ..)| r)
            .min()
            .unwrap();
        assert_eq!(r, 0xFF);
        assert!(rim_px < 0x80, "rim {rim_px:#04X} is not dark against a white fill");
    }

    #[test]
    fn a_black_swatch_has_a_light_rim() {
        // The mirror case: invisible on a dark menu without it.
        let size = 24;
        let px = pixels(size, Swatch::Solid((0x00, 0x00, 0x00)));
        let rim_px = (0..size)
            .map(|x| at(&px, size, x, size / 2))
            .filter(|&(a, ..)| a == 255)
            .map(|(_, r, ..)| r)
            .max()
            .unwrap();
        assert!(rim_px > 0x80, "rim {rim_px:#04X} is not light against a black fill");
    }

    #[test]
    fn off_is_hollow_so_it_never_reads_as_black() {
        let size = 24;
        let empty = pixels(size, Swatch::Empty);
        let black = pixels(size, Swatch::Solid((0, 0, 0)));

        // Centre: `off` shows nothing, `black` shows an opaque disc.
        assert_eq!(at(&empty, size, size / 2, size / 2).0, 0);
        assert_eq!(at(&black, size, size / 2, size / 2).0, 255);

        // But both still have a rim, so `off` is a ring rather than a blank.
        assert!(empty.iter().any(|&v| (v >> 24) as u8 == 255));
    }

    /// Render every swatch over a light and a dark background and write a BMP.
    ///
    /// Ignored by default because it writes a file and asserts nothing a machine
    /// can judge. The assertions above check the arithmetic; this is for the
    /// only question that matters — whether you can tell the colours apart, and
    /// whether `white` and `black` survive their respective backgrounds.
    ///
    ///     cargo test --bin jdrgb-tray -- --ignored contact_sheet
    #[test]
    #[ignore]
    fn contact_sheet() {
        use jdrgb::palette::{PRESETS, swatch_rgb};

        const CELL: i32 = 48;
        const DOT: i32 = 32;
        const COLS: i32 = 10;

        let cells: Vec<Swatch> = PRESETS
            .iter()
            .map(|&(n, _)| Swatch::Solid(swatch_rgb(n)))
            .chain(std::iter::once(Swatch::Empty))
            .collect();
        let rows = (cells.len() as i32 + COLS - 1) / COLS;
        let (w, h) = (COLS * CELL, rows * CELL * 2);

        // Windows 11's own menu backgrounds, near enough.
        let backgrounds = [(0xF3u8, 0xF3u8, 0xF3u8), (0x2Bu8, 0x2Bu8, 0x2Bu8)];
        let mut img = vec![0u8; (w * h * 3) as usize];
        for (band, bg) in backgrounds.iter().enumerate() {
            let y0 = band as i32 * rows * CELL;
            for y in y0..y0 + rows * CELL {
                for x in 0..w {
                    let i = ((y * w + x) * 3) as usize;
                    img[i] = bg.2;
                    img[i + 1] = bg.1;
                    img[i + 2] = bg.0;
                }
            }
            for (i, &s) in cells.iter().enumerate() {
                let px = pixels(DOT, s);
                let cx = (i as i32 % COLS) * CELL + (CELL - DOT) / 2;
                let cy = y0 + (i as i32 / COLS) * CELL + (CELL - DOT) / 2;
                for y in 0..DOT {
                    for x in 0..DOT {
                        let v = px[(y * DOT + x) as usize];
                        let (a, r, g, b) = (v >> 24, (v >> 16) as u8, (v >> 8) as u8, v as u8);
                        let inv = 255 - a;
                        let d = (((cy + y) * w + cx + x) * 3) as usize;
                        // Source is premultiplied, so this is a plain over.
                        img[d] = (b as u32 + img[d] as u32 * inv / 255) as u8;
                        img[d + 1] = (g as u32 + img[d + 1] as u32 * inv / 255) as u8;
                        img[d + 2] = (r as u32 + img[d + 2] as u32 * inv / 255) as u8;
                    }
                }
            }
        }

        let stride = ((w * 3 + 3) / 4 * 4) as usize;
        let mut bmp = Vec::new();
        let size = 54 + stride * h as usize;
        bmp.extend(b"BM");
        bmp.extend((size as u32).to_le_bytes());
        bmp.extend([0u8; 4]);
        bmp.extend(54u32.to_le_bytes());
        bmp.extend(40u32.to_le_bytes());
        bmp.extend(w.to_le_bytes());
        bmp.extend((-h).to_le_bytes()); // top-down
        bmp.extend(1u16.to_le_bytes());
        bmp.extend(24u16.to_le_bytes());
        bmp.extend([0u8; 24]);
        for y in 0..h {
            let row = ((y * w) * 3) as usize;
            bmp.extend(&img[row..row + (w * 3) as usize]);
            bmp.resize(bmp.len() + stride - (w * 3) as usize, 0);
        }

        let path = std::env::var("JDRGB_SHEET").map(std::path::PathBuf::from).unwrap_or_else(|_| {
            std::env::temp_dir().join("jdrgb-swatches.bmp")
        });
        std::fs::write(&path, &bmp).unwrap();
        println!("wrote {}", path.display());
    }

    #[test]
    fn scales_to_every_dpi_we_might_see() {
        // 16px at 100% through 32px at 200%. The rim must never vanish, which
        // is what the .max(1.0) in the radius is for.
        for size in [16, 20, 24, 28, 32, 40] {
            let px = pixels(size, Swatch::Solid((0xFF, 0xFF, 0xFF)));
            let opaque = px.iter().filter(|&&v| (v >> 24) as u8 == 255).count();
            assert!(opaque > 0, "size {size} drew nothing");
            let rim_present = (0..size)
                .map(|x| px[((size / 2) * size + x) as usize])
                .any(|v| (v >> 24) as u8 == 255 && ((v >> 16) as u8) < 0x80);
            assert!(rim_present, "size {size} lost its rim");
        }
    }

    /// A saturated colour reads fine against any neutral background, so it gets
    /// no outline — a bare disc simply looks better. This is the common case:
    /// only the near-neutral extremes are rimmed.
    #[test]
    fn a_saturated_colour_is_a_bare_disc() {
        let size = 24;
        for rgb in [(0xFF, 0x00, 0x00), (0x00, 0xFF, 0x00), (0x00, 0x00, 0xFF), (0xFF, 0xD0, 0x00)] {
            let px = pixels(size, Swatch::Solid(rgb));
            let opaque: Vec<_> = px
                .iter()
                .filter(|&&v| (v >> 24) as u8 == 255)
                .map(|&v| ((v >> 16) as u8, (v >> 8) as u8, v as u8))
                .collect();
            assert!(!opaque.is_empty());
            assert!(
                opaque.iter().all(|&c| c == rgb),
                "{rgb:?} picked up an outline it doesn't need"
            );
        }
    }

    /// Exactly which presets are rimmed, so tuning a colour into or out of the
    /// near-neutral corner shows up here rather than in the menu.
    #[test]
    fn only_the_near_neutral_extremes_are_rimmed() {
        use jdrgb::palette::{PRESETS, swatch_rgb};

        let rimmed: Vec<&str> = PRESETS
            .iter()
            .map(|&(n, _)| n)
            .filter(|n| needs_rim(swatch_rgb(n)))
            .collect();
        assert_eq!(rimmed, vec!["coolwhite", "white", "black"]);
    }

    #[test]
    fn warmwhite_and_yellow_keep_their_bare_disc() {
        use jdrgb::palette::swatch_rgb;
        // Both are light, but both carry enough colour to show against a light
        // menu on their own. They sit closest to the threshold, so they are the
        // ones worth pinning down.
        assert!(!needs_rim(swatch_rgb("warmwhite")));
        assert!(!needs_rim(swatch_rgb("yellow")));
    }
}
