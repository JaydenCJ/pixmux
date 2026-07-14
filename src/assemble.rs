//! Assembles kitty graphics transmissions into decoded RGBA images.
//!
//! Handles chunked direct transmissions (`m=1` continuations), base64
//! decoding, optional zlib decompression (`o=z`), and the three data formats
//! kitty defines: `f=32` (RGBA), `f=24` (RGB), `f=100` (PNG).

use std::collections::HashMap;
use std::fmt;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use flate2::read::ZlibDecoder;
use std::io::Read;

use crate::protocol::GraphicsCommand;
use crate::sixel::Image;

/// Why a transmission could not be turned into an image.
#[derive(Debug)]
pub struct AssembleError(pub String);

impl fmt::Display for AssembleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot assemble image: {}", self.0)
    }
}

impl std::error::Error for AssembleError {}

fn err(msg: impl Into<String>) -> AssembleError {
    AssembleError(msg.into())
}

/// Decode a fully assembled payload into an RGBA image.
pub fn decode_image(
    format: u32,
    width: Option<u32>,
    height: Option<u32>,
    data: &[u8],
) -> Result<Image, AssembleError> {
    match format {
        100 => decode_png(data),
        24 | 32 => {
            let (w, h) = match (width, height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
                _ => return Err(err("raw format requires s= and v= dimensions")),
            };
            let bpp = if format == 32 { 4 } else { 3 };
            let expected = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(bpp))
                .ok_or_else(|| err("image dimensions overflow"))?;
            if data.len() != expected {
                return Err(err(format!(
                    "raw payload is {} bytes, expected {} for {w}x{h} f={format}",
                    data.len(),
                    expected
                )));
            }
            let pixels = if format == 32 {
                data.to_vec()
            } else {
                let mut px = Vec::with_capacity(w as usize * h as usize * 4);
                for rgb in data.chunks_exact(3) {
                    px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
                px
            };
            Image::new(w, h, pixels).ok_or_else(|| err("dimension mismatch"))
        }
        other => Err(err(format!("unsupported pixel format f={other}"))),
    }
}

fn decode_png(data: &[u8]) -> Result<Image, AssembleError> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| err(format!("invalid PNG: {e}")))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| err(format!("PNG decode failed: {e}")))?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let pixels = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut px = Vec::with_capacity(buf.len() / 3 * 4);
            for rgb in buf.chunks_exact(3) {
                px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            px
        }
        png::ColorType::Grayscale => {
            let mut px = Vec::with_capacity(buf.len() * 4);
            for &g in &buf {
                px.extend_from_slice(&[g, g, g, 255]);
            }
            px
        }
        png::ColorType::GrayscaleAlpha => {
            let mut px = Vec::with_capacity(buf.len() * 2);
            for ga in buf.chunks_exact(2) {
                px.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
            px
        }
        png::ColorType::Indexed => return Err(err("indexed PNG not expanded")),
    };
    Image::new(w, h, pixels).ok_or_else(|| err("PNG dimension mismatch"))
}

/// A pending chunked transmission being accumulated.
struct Pending {
    first: GraphicsCommand,
    data: Vec<u8>,
}

/// The result of pushing one graphics command into the assembler.
pub enum Assembled {
    /// Command consumed; more chunks expected.
    Incomplete,
    /// A full transmission finished; here is the image plus the command that
    /// started it (carrying action/placement keys).
    Image {
        first: GraphicsCommand,
        image: Image,
    },
    /// A display command (`a=p`) referencing an already-stored image id.
    Display { cmd: GraphicsCommand, image: Image },
    /// Delete command; ids were dropped from the store.
    Deleted,
    /// Command cannot produce an image (query, file-based medium, decode
    /// error, animation frames, ...). The caller decides what to do with it.
    Unsupported(String),
}

/// Accumulates chunked transmissions and keeps decoded images by id so that
/// later `a=p` display commands can be honored.
#[derive(Default)]
pub struct Assembler {
    pending: Option<Pending>,
    store: HashMap<u32, Image>,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of images currently retained for `a=p` display.
    pub fn stored(&self) -> usize {
        self.store.len()
    }

    pub fn push(&mut self, cmd: &GraphicsCommand) -> Assembled {
        // Continuation chunks: only m= (and optionally q=) keys.
        if self.pending.is_some() && (cmd.keys.contains_key("m") && cmd.get("a").is_none()) {
            let done = !cmd.more_chunks();
            {
                let p = self.pending.as_mut().unwrap();
                p.data.extend_from_slice(&cmd.payload);
            }
            if !done {
                return Assembled::Incomplete;
            }
            let p = self.pending.take().unwrap();
            return self.finish(p);
        }

        match cmd.action() {
            "t" | "T" => {
                if cmd.medium() != "d" {
                    return Assembled::Unsupported(format!(
                        "transmission medium t={} not supported for transcoding",
                        cmd.medium()
                    ));
                }
                let p = Pending {
                    first: cmd.clone(),
                    data: cmd.payload.clone(),
                };
                if cmd.more_chunks() {
                    self.pending = Some(p);
                    Assembled::Incomplete
                } else {
                    self.finish(p)
                }
            }
            "p" => match cmd.image_id().and_then(|id| self.store.get(&id)) {
                Some(img) => Assembled::Display {
                    cmd: cmd.clone(),
                    image: img.clone(),
                },
                None => Assembled::Unsupported("a=p references unknown image id".into()),
            },
            "d" => {
                match cmd.image_id() {
                    Some(id) => {
                        self.store.remove(&id);
                    }
                    None => self.store.clear(),
                }
                Assembled::Deleted
            }
            "q" => Assembled::Unsupported("query command".into()),
            other => Assembled::Unsupported(format!("action a={other} not supported")),
        }
    }

    fn finish(&mut self, p: Pending) -> Assembled {
        let first = p.first;
        let decoded = B64.decode(&p.data);
        let raw = match decoded {
            Ok(d) => d,
            Err(e) => return Assembled::Unsupported(format!("base64 decode failed: {e}")),
        };
        let raw = if first.get("o") == Some("z") {
            let mut z = ZlibDecoder::new(raw.as_slice());
            let mut out = Vec::new();
            if let Err(e) = z.read_to_end(&mut out) {
                return Assembled::Unsupported(format!("zlib decompression failed: {e}"));
            }
            out
        } else {
            raw
        };
        match decode_image(first.format(), first.get_u32("s"), first.get_u32("v"), &raw) {
            Ok(image) => {
                if let Some(id) = first.image_id() {
                    self.store.insert(id, image.clone());
                }
                Assembled::Image { first, image }
            }
            Err(e) => Assembled::Unsupported(e.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn b64(data: &[u8]) -> Vec<u8> {
        B64.encode(data).into_bytes()
    }

    #[test]
    fn decodes_raw_rgb() {
        let img = decode_image(24, Some(2), Some(1), &[255, 0, 0, 0, 255, 0]).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn rejects_wrong_raw_length() {
        assert!(decode_image(24, Some(2), Some(2), &[0; 5]).is_err());
        assert!(decode_image(32, None, None, &[0; 4]).is_err());
    }

    #[test]
    fn single_chunk_rgba_transmission() {
        let mut asm = Assembler::new();
        let mut body = b"a=T,f=32,s=1,v=1,i=5;".to_vec();
        body.extend_from_slice(&b64(&[1, 2, 3, 4]));
        let cmd = GraphicsCommand::parse(&body).unwrap();
        match asm.push(&cmd) {
            Assembled::Image { image, first } => {
                assert_eq!(image.pixels, vec![1, 2, 3, 4]);
                assert_eq!(first.image_id(), Some(5));
            }
            _ => panic!("expected image"),
        }
        assert_eq!(asm.stored(), 1);
    }

    #[test]
    fn chunked_transmission_reassembles() {
        let mut asm = Assembler::new();
        let full = b64(&[9, 8, 7, 255]);
        let (a, b) = full.split_at(4);
        let mut body1 = b"a=T,f=32,s=1,v=1,m=1;".to_vec();
        body1.extend_from_slice(a);
        let mut body2 = b"m=0;".to_vec();
        body2.extend_from_slice(b);
        let c1 = GraphicsCommand::parse(&body1).unwrap();
        let c2 = GraphicsCommand::parse(&body2).unwrap();
        assert!(matches!(asm.push(&c1), Assembled::Incomplete));
        match asm.push(&c2) {
            Assembled::Image { image, .. } => assert_eq!(image.pixels, vec![9, 8, 7, 255]),
            _ => panic!("expected image after final chunk"),
        }
    }

    #[test]
    fn zlib_compressed_payload() {
        let raw = [10u8, 20, 30, 40];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw).unwrap();
        let compressed = enc.finish().unwrap();
        let mut body = b"a=T,f=32,s=1,v=1,o=z;".to_vec();
        body.extend_from_slice(&b64(&compressed));
        let cmd = GraphicsCommand::parse(&body).unwrap();
        let mut asm = Assembler::new();
        match asm.push(&cmd) {
            Assembled::Image { image, .. } => assert_eq!(image.pixels, raw.to_vec()),
            _ => panic!("expected image"),
        }
    }

    #[test]
    fn display_by_id_and_delete() {
        let mut asm = Assembler::new();
        let mut body = b"a=t,f=32,s=1,v=1,i=77;".to_vec();
        body.extend_from_slice(&b64(&[1, 1, 1, 255]));
        asm.push(&GraphicsCommand::parse(&body).unwrap());
        let show = GraphicsCommand::parse(b"a=p,i=77").unwrap();
        assert!(matches!(asm.push(&show), Assembled::Display { .. }));
        let del = GraphicsCommand::parse(b"a=d,d=i,i=77").unwrap();
        assert!(matches!(asm.push(&del), Assembled::Deleted));
        assert_eq!(asm.stored(), 0);
        assert!(matches!(asm.push(&show), Assembled::Unsupported(_)));
    }

    #[test]
    fn file_medium_is_unsupported() {
        let cmd = GraphicsCommand::parse(b"a=T,t=f,f=100;L3RtcC94LnBuZw==").unwrap();
        let mut asm = Assembler::new();
        assert!(matches!(asm.push(&cmd), Assembled::Unsupported(_)));
    }
}
