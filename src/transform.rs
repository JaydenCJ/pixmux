//! The core translation pipeline: parser events in, target-specific bytes out.

use std::fmt;
use std::str::FromStr;

use crate::assemble::{Assembled, Assembler};
use crate::parser::{Event, StreamParser};
use crate::protocol::GraphicsCommand;
use crate::sixel;
use crate::tmux;

/// Where the translated stream is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Detect from environment ($ZELLIJ, then $TMUX), else None.
    Auto,
    /// Wrap kitty graphics in tmux passthrough DCS (requires
    /// `allow-passthrough on` and a kitty-capable outer terminal).
    Tmux,
    /// Transcode kitty graphics to sixel (zellij renders sixel natively).
    Zellij,
    /// Remove kitty graphics entirely (plain terminals / logs).
    Strip,
    /// Leave the stream untouched.
    None,
}

impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Target::Auto),
            "tmux" => Ok(Target::Tmux),
            "zellij" => Ok(Target::Zellij),
            "strip" => Ok(Target::Strip),
            "none" => Ok(Target::None),
            other => Err(format!(
                "unknown target {other:?} (expected auto|tmux|zellij|strip|none)"
            )),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Target::Auto => "auto",
            Target::Tmux => "tmux",
            Target::Zellij => "zellij",
            Target::Strip => "strip",
            Target::None => "none",
        };
        f.write_str(s)
    }
}

impl Target {
    /// Resolve `Auto` against the process environment.
    pub fn resolve(self) -> Target {
        match self {
            Target::Auto => {
                if std::env::var_os("ZELLIJ").is_some() {
                    Target::Zellij
                } else if std::env::var_os("TMUX").is_some() {
                    Target::Tmux
                } else {
                    Target::None
                }
            }
            other => other,
        }
    }
}

/// Counters describing what the transformer saw and did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Complete kitty graphics commands seen on the input.
    pub graphics_commands: usize,
    /// Commands re-encoded for the target (wrapped or transcoded).
    pub translated: usize,
    /// Commands dropped or passed through untranslated (with reason logged).
    pub untranslated: usize,
    /// Full images decoded (zellij target only).
    pub images_decoded: usize,
}

/// Streaming transformer: feed raw bytes, get translated bytes.
pub struct Transformer {
    target: Target,
    parser: StreamParser,
    assembler: Assembler,
    stats: Stats,
    /// Human-readable notes about untranslated commands (deduplicated).
    notes: Vec<String>,
}

impl Transformer {
    /// `target` must already be resolved (not `Auto`).
    pub fn new(target: Target) -> Self {
        debug_assert!(target != Target::Auto, "resolve() the target first");
        Transformer {
            target,
            parser: StreamParser::new(),
            assembler: Assembler::new(),
            stats: Stats::default(),
            notes: Vec::new(),
        }
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    fn note(&mut self, msg: String) {
        if !self.notes.iter().any(|n| n == &msg) {
            self.notes.push(msg);
        }
    }

    fn handle_graphics(&mut self, cmd: GraphicsCommand, raw: Vec<u8>, out: &mut Vec<u8>) {
        self.stats.graphics_commands += 1;
        match self.target {
            // Auto is resolved in the constructor; treat defensively as None.
            Target::Auto | Target::None => out.extend_from_slice(&raw),
            Target::Strip => {
                self.stats.translated += 1;
            }
            Target::Tmux => {
                out.extend_from_slice(&tmux::wrap_graphics(&cmd, &raw));
                self.stats.translated += 1;
            }
            Target::Zellij => match self.assembler.push(&cmd) {
                Assembled::Incomplete => {}
                Assembled::Image { first, image } => {
                    self.stats.images_decoded += 1;
                    // Only a=T (transmit + display) draws immediately; a
                    // plain transmit (a=t) is stored for a later a=p.
                    if first.action() == "T" {
                        out.extend_from_slice(&sixel::encode(&image));
                        self.stats.translated += 1;
                    }
                }
                Assembled::Display { image, .. } => {
                    out.extend_from_slice(&sixel::encode(&image));
                    self.stats.translated += 1;
                }
                Assembled::Deleted => {
                    // Sixel is immediate-mode: already-drawn cells cannot be
                    // erased retroactively. Count as handled.
                    self.stats.translated += 1;
                }
                Assembled::Unsupported(reason) => {
                    self.stats.untranslated += 1;
                    self.note(reason);
                }
            },
        }
    }

    /// Feed input bytes; returns translated output bytes.
    pub fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for event in self.parser.feed(input) {
            match event {
                Event::Passthrough(bytes) => out.extend_from_slice(&bytes),
                Event::Malformed(bytes) => {
                    self.stats.untranslated += 1;
                    self.note("malformed kitty graphics sequence passed through".into());
                    out.extend_from_slice(&bytes);
                }
                Event::Graphics { cmd, raw } => self.handle_graphics(cmd, raw, &mut out),
            }
        }
        out
    }

    /// Flush any incomplete trailing sequence at end of stream.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for event in self.parser.finish() {
            if let Event::Passthrough(bytes) = event {
                out.extend_from_slice(&bytes);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    fn kitty_apc(body: &[u8]) -> Vec<u8> {
        let mut v = b"\x1b_G".to_vec();
        v.extend_from_slice(body);
        v.extend_from_slice(b"\x1b\\");
        v
    }

    #[test]
    fn target_parses_from_str() {
        assert_eq!("tmux".parse::<Target>().unwrap(), Target::Tmux);
        assert!("kitty".parse::<Target>().is_err());
    }

    #[test]
    fn none_target_is_identity() {
        let mut t = Transformer::new(Target::None);
        let input = [b"text".to_vec(), kitty_apc(b"a=T,f=100;QUJD")].concat();
        let mut out = t.feed(&input);
        out.extend(t.finish());
        assert_eq!(out, input);
        assert_eq!(t.stats().graphics_commands, 1);
    }

    #[test]
    fn strip_target_removes_graphics_keeps_text() {
        let mut t = Transformer::new(Target::Strip);
        let input = [b"A".to_vec(), kitty_apc(b"a=T,f=100;QUJD"), b"B".to_vec()].concat();
        let mut out = t.feed(&input);
        out.extend(t.finish());
        assert_eq!(out, b"AB".to_vec());
    }

    #[test]
    fn tmux_target_wraps_graphics_only() {
        let mut t = Transformer::new(Target::Tmux);
        let input = [b"$ ".to_vec(), kitty_apc(b"a=T,f=100;QUJD")].concat();
        let out = t.feed(&input);
        let s = String::from_utf8_lossy(&out);
        assert!(s.starts_with("$ \u{1b}Ptmux;"));
        assert!(s.contains("\u{1b}\u{1b}_Ga=T,f=100;QUJD"));
        assert_eq!(t.stats().translated, 1);
    }

    #[test]
    fn zellij_target_transcodes_to_sixel() {
        // 1x1 red RGBA image, direct transmission with display.
        let payload = B64.encode([255u8, 0, 0, 255]);
        let body = format!("a=T,f=32,s=1,v=1,i=3;{payload}");
        let mut t = Transformer::new(Target::Zellij);
        let out = t.feed(&kitty_apc(body.as_bytes()));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\u{1b}P0;1;0q"), "expected sixel in {s:?}");
        assert!(!s.contains("\u{1b}_G"), "kitty APC must not leak: {s:?}");
        assert_eq!(t.stats().images_decoded, 1);
    }

    #[test]
    fn zellij_unsupported_medium_is_dropped_with_note() {
        let mut t = Transformer::new(Target::Zellij);
        let out = t.feed(&kitty_apc(b"a=T,t=f,f=100;L3RtcA=="));
        assert!(out.is_empty());
        assert_eq!(t.stats().untranslated, 1);
        assert!(!t.notes().is_empty());
    }
}
