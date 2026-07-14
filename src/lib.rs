//! pixmux: kitty graphics protocol passthrough shim for tmux and zellij.
//!
//! The library exposes the streaming translation pipeline used by the CLI:
//!
//! - [`parser`]: incremental scanner splitting terminal byte streams into
//!   plain output and kitty graphics APC sequences;
//! - [`protocol`]: kitty graphics control-data parsing/serialization;
//! - [`tmux`]: tmux passthrough (DCS) wrapping with spec-sized re-chunking;
//! - [`assemble`]: chunk reassembly + base64/zlib/PNG decoding;
//! - [`sixel`]: deterministic sixel encoder (zellij renders sixel natively);
//! - [`transform`]: the target-aware pipeline tying it all together;
//! - [`emit`]: producing kitty sequences from local PNGs (`pixmux cat`);
//! - [`doctor`]: environment diagnosis;
//! - [`pty`]: running a child under a PTY with live translation (Unix only).

pub mod assemble;
pub mod doctor;
pub mod emit;
pub mod parser;
pub mod protocol;
pub mod sixel;
pub mod tmux;
pub mod transform;

#[cfg(unix)]
pub mod pty;
