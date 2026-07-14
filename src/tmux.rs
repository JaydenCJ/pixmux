//! tmux passthrough encoding.
//!
//! tmux forwards an escape sequence to the outer terminal when it is wrapped
//! in a passthrough DCS: `ESC P tmux ; <data> ESC \`, where every ESC inside
//! `<data>` is doubled. Requires `set -gq allow-passthrough on` in tmux
//! (>= 3.3). Reference: tmux(1) manual, section on `allow-passthrough`.

use crate::protocol::GraphicsCommand;

/// Maximum base64 payload bytes per kitty chunk. The kitty spec requires
/// chunks of at most 4096 payload bytes; programs that emit oversized single
/// APCs are re-chunked to this size before wrapping.
pub const CHUNK_SIZE: usize = 4096;

/// Wrap a complete escape sequence in a tmux passthrough DCS, doubling ESC.
pub fn wrap_passthrough(seq: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(seq.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Split an oversized direct-transmission graphics command into spec-sized
/// chunks (first chunk keeps all keys + `m=1`, middle chunks are `m=1`, the
/// final chunk is `m=0`), then wrap each chunk for tmux.
///
/// Commands that are already small enough, or that do not carry an inline
/// payload, are wrapped as a single passthrough unit.
pub fn wrap_graphics(cmd: &GraphicsCommand, raw: &[u8]) -> Vec<u8> {
    let needs_rechunk = cmd.medium() == "d" && cmd.payload.len() > CHUNK_SIZE && !cmd.more_chunks();
    if !needs_rechunk {
        return wrap_passthrough(raw);
    }

    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = cmd.payload.chunks(CHUNK_SIZE).collect();
    let last = chunks.len() - 1;
    for (idx, chunk) in chunks.iter().enumerate() {
        let apc = if idx == 0 {
            let mut first = cmd.clone();
            first.keys.insert("m".into(), "1".into());
            first.payload = chunk.to_vec();
            first.to_apc()
        } else {
            let mut cont = GraphicsCommand::default();
            cont.keys
                .insert("m".into(), if idx == last { "0" } else { "1" }.into());
            // Quiet continuation chunks: never trigger terminal responses.
            if let Some(q) = cmd.get("q") {
                cont.keys.insert("q".into(), q.into());
            }
            cont.payload = chunk.to_vec();
            cont.to_apc()
        };
        out.extend_from_slice(&wrap_passthrough(&apc));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_doubles_esc() {
        let wrapped = wrap_passthrough(b"\x1b_Ga=T;QQ==\x1b\\");
        assert_eq!(
            wrapped,
            b"\x1bPtmux;\x1b\x1b_Ga=T;QQ==\x1b\x1b\\\x1b\\".to_vec()
        );
    }

    #[test]
    fn small_command_wrapped_as_is() {
        let raw = b"\x1b_Ga=T,f=100;QUJD\x1b\\";
        let cmd = GraphicsCommand::parse(b"a=T,f=100;QUJD").unwrap();
        let out = wrap_graphics(&cmd, raw);
        assert_eq!(out, wrap_passthrough(raw));
    }

    #[test]
    fn oversized_payload_is_rechunked() {
        let payload = vec![b'A'; CHUNK_SIZE * 2 + 100];
        let mut body = b"a=T,f=100,i=9;".to_vec();
        body.extend_from_slice(&payload);
        let cmd = GraphicsCommand::parse(&body).unwrap();
        let raw = cmd.to_apc();
        let out = wrap_graphics(&cmd, &raw);
        let s = String::from_utf8_lossy(&out);
        // Three passthrough units expected.
        assert_eq!(s.matches("\x1bPtmux;").count(), 3);
        // First chunk keeps keys and sets m=1.
        assert!(s.contains("a=T,f=100,i=9,m=1;"));
        // Final chunk closes the stream with m=0.
        assert!(s.contains("\x1b\x1b_Gm=0;"));
    }

    #[test]
    fn already_chunked_stream_not_rechunked() {
        // A continuation chunk (m=1) must pass through unmodified even if big.
        let payload = vec![b'B'; CHUNK_SIZE + 5];
        let mut body = b"m=1;".to_vec();
        body.extend_from_slice(&payload);
        let cmd = GraphicsCommand::parse(&body).unwrap();
        let raw = cmd.to_apc();
        let out = wrap_graphics(&cmd, &raw);
        assert_eq!(out, wrap_passthrough(&raw));
    }
}
