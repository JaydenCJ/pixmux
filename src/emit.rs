//! Emitting kitty graphics sequences for local images (`pixmux cat`).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::protocol::GraphicsCommand;
use crate::tmux::CHUNK_SIZE;

/// Build the kitty graphics APC sequences (chunked at 4096 payload bytes)
/// that transmit and display a PNG file's bytes inline (f=100, t=d, a=T).
pub fn png_to_kitty(png_bytes: &[u8], image_id: Option<u32>) -> Vec<u8> {
    let b64 = B64.encode(png_bytes);
    let data = b64.as_bytes();
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[]]
    } else {
        data.chunks(CHUNK_SIZE).collect()
    };
    let last = chunks.len() - 1;
    let mut out = Vec::with_capacity(b64.len() + chunks.len() * 24);
    for (idx, chunk) in chunks.iter().enumerate() {
        let mut cmd = GraphicsCommand::default();
        if idx == 0 {
            cmd.keys.insert("a".into(), "T".into());
            cmd.keys.insert("f".into(), "100".into());
            // Suppress terminal responses: we are a one-shot writer.
            cmd.keys.insert("q".into(), "2".into());
            if let Some(id) = image_id {
                cmd.keys.insert("i".into(), id.to_string());
            }
        }
        if chunks.len() > 1 {
            cmd.keys
                .insert("m".into(), if idx == last { "0" } else { "1" }.into());
        }
        cmd.payload = chunk.to_vec();
        out.extend_from_slice(&cmd.to_apc());
    }
    out
}

/// Basic sanity check that a byte buffer looks like a PNG file.
pub fn looks_like_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_png_single_apc() {
        let out = png_to_kitty(b"12345", Some(7));
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=100,i=7,q=2;"));
        assert!(s.ends_with("\x1b\\"));
        assert!(!s.contains("m="), "no chunk key for single chunk: {s:?}");
    }

    #[test]
    fn large_png_is_chunked() {
        let data = vec![0u8; CHUNK_SIZE * 3]; // base64 expands 4/3
        let out = png_to_kitty(&data, None);
        let s = String::from_utf8(out).unwrap();
        let count = s.matches("\x1b_G").count();
        assert!(count >= 2, "expected multiple chunks, got {count}");
        assert!(s.contains("m=1"));
        assert!(s.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn png_signature_check() {
        assert!(looks_like_png(&[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2
        ]));
        assert!(!looks_like_png(b"GIF89a"));
    }
}
