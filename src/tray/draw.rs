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

/// The ring for `Empty`. A mid grey, so it reads against a light or a dark menu
/// without knowing which it is on.
const RING: (u8, u8, u8) = (0x9A, 0x9A, 0x9A);

/// The rim colour for a swatch — which for a solid is its own fill, i.e. none.
///
/// Solids used to get an outline where they risked dissolving into the menu
/// background: `white` and `coolwhite` against a light theme, `black` against a
/// dark one. It was the wrong fix, and not because the colours read fine —
/// because of *where the outline has to come from*. The disc already spans the
/// whole bitmap, so a rim can only be carved out of the fill. That makes a
/// rimmed swatch draw a visibly smaller circle than the bare ones beside it,
/// and when the rim also happens to sit near the menu's own colour, the swatch
/// simply looks undersized for no visible reason.
///
/// Fixing that properly would mean growing the bitmap to hold a rim outside the
/// fill, which is a real change for a problem only two presets have on a theme
/// this machine doesn't use. So: solids are bare, and `Empty` keeps its ring,
/// where the ring is the entire point rather than a decoration.
fn rim(s: Swatch) -> (u8, u8, u8) {
    match s {
        Swatch::Empty => RING,
        // Blending a colour towards itself is a no-op, so the drawing loop needs
        // no special case and the antialiased edge is identical either way.
        Swatch::Solid(rgb) => rgb,
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

    /// The defect that retired rims: an outline is carved out of the fill, so a
    /// rimmed swatch draws a smaller circle than the bare ones next to it. It
    /// shows up as `white` and `coolwhite` looking undersized rather than
    /// outlined, because the rim sat close to the menu's own colour.
    #[test]
    fn every_solid_swatch_is_exactly_the_same_size() {
        use jdrgb::palette::{PRESETS, swatch_rgb};

        let size = 24;
        let opaque = |s| {
            pixels(size, s)
                .iter()
                .filter(|&&v| (v >> 24) as u8 == 255)
                .count()
        };
        let reference = opaque(Swatch::Solid((0xFF, 0x00, 0x00)));
        for &(name, _) in PRESETS {
            assert_eq!(
                opaque(Swatch::Solid(swatch_rgb(name))),
                reference,
                "{name} draws a different size from its neighbours"
            );
        }
    }

    /// Every solid is one flat colour edge to edge. The near-neutrals are the
    /// ones that used to be special-cased, so they are the ones worth naming.
    #[test]
    fn no_solid_carries_an_outline() {
        let size = 24;
        for rgb in [
            (0xFF, 0xFF, 0xFF), // white
            (0xF5, 0xF8, 0xFF), // coolwhite
            (0x00, 0x00, 0x00), // black
            (0xFF, 0xD9, 0xB3), // warmwhite
        ] {
            let px = pixels(size, Swatch::Solid(rgb));
            let fills: Vec<_> = px
                .iter()
                .filter(|&&v| (v >> 24) as u8 == 255)
                .map(|&v| ((v >> 16) as u8, (v >> 8) as u8, v as u8))
                .collect();
            assert!(!fills.is_empty());
            assert!(fills.iter().all(|&c| c == rgb), "{rgb:?} picked up an outline");
        }
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
        // 16px at 100% through 40px at 250%. `Empty`'s ring is the thing that
        // can vanish at small sizes, which is what the .max(1.0) on the ring
        // width is for.
        for size in [16, 20, 24, 28, 32, 40] {
            let solid = pixels(size, Swatch::Solid((0xFF, 0xFF, 0xFF)));
            assert!(
                solid.iter().any(|&v| (v >> 24) as u8 == 255),
                "size {size} drew nothing"
            );
            let empty = pixels(size, Swatch::Empty);
            assert!(
                empty.iter().any(|&v| (v >> 24) as u8 == 255),
                "size {size} lost the Off ring"
            );
            // ...and stayed hollow, rather than the ring closing over the middle.
            let centre = empty[((size / 2) * size + size / 2) as usize];
            assert_eq!((centre >> 24) as u8, 0, "size {size} filled the Off ring in");
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

}
