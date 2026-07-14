//! Minimal deterministic sixel encoder.
//!
//! Used for the zellij target: zellij renders sixel but not the kitty
//! graphics protocol, so kitty images are transcoded to sixel on the fly.
//!
//! Quantization uses a fixed 3-3-2 palette (256 registers): deterministic,
//! allocation-light, and good enough for charts and screenshots. Pixels with
//! alpha < 128 are treated as fully transparent (sixel `P2=1` mode).

/// A decoded RGBA image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGBA, row-major, 4 bytes per pixel.
    pub pixels: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        if (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            != Some(pixels.len())
        {
            return None;
        }
        Some(Image {
            width,
            height,
            pixels,
        })
    }
}

/// Map an opaque RGB color to a 3-3-2 palette register (0..=255).
fn register_of(r: u8, g: u8, b: u8) -> u8 {
    (r >> 5) << 5 | (g >> 5) << 2 | (b >> 6)
}

/// Representative RGB (0..=255) of a 3-3-2 register.
fn color_of(reg: u8) -> (u8, u8, u8) {
    let r3 = (reg >> 5) & 0x7;
    let g3 = (reg >> 2) & 0x7;
    let b2 = reg & 0x3;
    // Scale level midpoints back to 0..=255.
    let r = (r3 as u16 * 255 / 7) as u8;
    let g = (g3 as u16 * 255 / 7) as u8;
    let b = (b2 as u16 * 255 / 3) as u8;
    (r, g, b)
}

fn pct(v: u8) -> u16 {
    (v as u16 * 100 + 127) / 255
}

/// Append a run-length-encoded sixel run to `out`.
fn push_run(out: &mut Vec<u8>, ch: u8, count: usize) {
    if count == 0 {
        return;
    }
    if count > 3 {
        out.extend_from_slice(format!("!{count}").as_bytes());
        out.push(ch);
    } else {
        for _ in 0..count {
            out.push(ch);
        }
    }
}

/// Encode an RGBA image as a complete sixel sequence (`ESC P ... q ... ESC \`).
pub fn encode(img: &Image) -> Vec<u8> {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut out = Vec::with_capacity(w * h / 2 + 256);
    // P2=1: pixels not written stay transparent.
    out.extend_from_slice(b"\x1bP0;1;0q");
    out.extend_from_slice(format!("\"1;1;{w};{h}").as_bytes());

    // Quantize each pixel to a register; u16::MAX marks transparency.
    const TRANSPARENT: u16 = u16::MAX;
    let mut quantized: Vec<u16> = Vec::with_capacity(w * h);
    let mut used = [false; 256];
    for px in img.pixels.chunks_exact(4) {
        if px[3] < 128 {
            quantized.push(TRANSPARENT);
        } else {
            let reg = register_of(px[0], px[1], px[2]);
            used[reg as usize] = true;
            quantized.push(reg as u16);
        }
    }

    // Palette definitions for used registers only, ascending: deterministic.
    for reg in 0..=255u16 {
        if used[reg as usize] {
            let (r, g, b) = color_of(reg as u8);
            out.extend_from_slice(
                format!("#{};2;{};{};{}", reg, pct(r), pct(g), pct(b)).as_bytes(),
            );
        }
    }

    // Emit bands of 6 rows.
    let mut band_regs: Vec<u16> = Vec::new();
    for band_start in (0..h).step_by(6) {
        let band_rows = (h - band_start).min(6);
        // Which registers appear in this band?
        band_regs.clear();
        {
            let mut seen = [false; 256];
            for row in 0..band_rows {
                let off = (band_start + row) * w;
                for &q in &quantized[off..off + w] {
                    if q != TRANSPARENT && !seen[q as usize] {
                        seen[q as usize] = true;
                        band_regs.push(q);
                    }
                }
            }
            band_regs.sort_unstable();
        }

        for (ci, &reg) in band_regs.iter().enumerate() {
            out.extend_from_slice(format!("#{reg}").as_bytes());
            let mut run_char = 0u8;
            let mut run_len = 0usize;
            for x in 0..w {
                let mut bits = 0u8;
                for row in 0..band_rows {
                    let q = quantized[(band_start + row) * w + x];
                    if q == reg {
                        bits |= 1 << row;
                    }
                }
                let ch = 0x3f + bits;
                if ch == run_char {
                    run_len += 1;
                } else {
                    push_run(&mut out, run_char, run_len);
                    run_char = ch;
                    run_len = 1;
                }
            }
            // Trailing empty columns can be dropped; emit only if non-blank.
            if run_char != 0x3f {
                push_run(&mut out, run_char, run_len);
            }
            if ci + 1 < band_regs.len() {
                out.push(b'$'); // carriage return within band
            }
        }
        out.push(b'-'); // next band
    }

    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_dimension_check() {
        assert!(Image::new(2, 2, vec![0; 16]).is_some());
        assert!(Image::new(2, 2, vec![0; 15]).is_none());
        assert!(Image::new(u32::MAX, u32::MAX, vec![]).is_none());
    }

    #[test]
    fn register_roundtrip_extremes() {
        assert_eq!(register_of(0, 0, 0), 0);
        assert_eq!(register_of(255, 255, 255), 255);
        assert_eq!(color_of(0), (0, 0, 0));
        assert_eq!(color_of(255), (255, 255, 255));
    }

    #[test]
    fn encodes_single_red_pixel() {
        let img = Image::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        let out = encode(&img);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bP0;1;0q\"1;1;1;1"));
        // Register for pure red under 3-3-2: (7<<5) = 224.
        assert!(s.contains("#224;2;100;0;0"));
        // One pixel in row 0 of the band: sixel char '?'+1 = '@'.
        assert!(s.contains("#224@"));
        assert!(s.ends_with("-\x1b\\"));
    }

    #[test]
    fn transparent_pixels_are_skipped() {
        // 2x1: opaque white then transparent.
        let img = Image::new(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 0]).unwrap();
        let out = encode(&img);
        let s = String::from_utf8(out).unwrap();
        // Only the white register defined; black never appears.
        assert!(s.contains("#255;2;100;100;100"));
        assert!(!s.contains("#0;2;0;0;0"));
    }

    #[test]
    fn run_length_encoding_kicks_in() {
        // 10x1 solid green row -> a !10 run.
        let mut px = Vec::new();
        for _ in 0..10 {
            px.extend_from_slice(&[0, 255, 0, 255]);
        }
        let img = Image::new(10, 1, px).unwrap();
        let s = String::from_utf8(encode(&img)).unwrap();
        assert!(s.contains("!10@"), "expected RLE run in {s:?}");
    }

    #[test]
    fn deterministic_output() {
        let img = Image::new(3, 8, vec![128; 3 * 8 * 4]).unwrap();
        assert_eq!(encode(&img), encode(&img));
    }
}
