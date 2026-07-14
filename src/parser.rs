//! Streaming byte parser that splits a terminal output stream into plain
//! passthrough bytes and kitty graphics APC sequences.
//!
//! The parser is incremental: bytes may arrive in arbitrary splits (a single
//! escape sequence can span many `feed` calls). Everything that is not a
//! kitty graphics APC (`ESC _ G ... ESC \`) is forwarded untouched, including
//! other APC/DCS/OSC sequences.

use crate::protocol::{GraphicsCommand, APC_PREFIX};

/// One event produced by the stream parser.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Bytes that are not part of a kitty graphics sequence; forward verbatim.
    Passthrough(Vec<u8>),
    /// A complete kitty graphics command. `raw` is the exact original
    /// sequence (including `ESC _ G` and `ESC \`) for byte-exact re-emission.
    Graphics { cmd: GraphicsCommand, raw: Vec<u8> },
    /// A sequence that started like a kitty graphics APC but whose control
    /// data failed to parse; forwarded raw so nothing is ever lost.
    Malformed(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not inside any candidate sequence.
    Ground,
    /// Seen ESC (may start ESC _ G).
    Esc,
    /// Seen ESC _ (may be a kitty APC if next byte is G).
    EscUnderscore,
    /// Inside ESC _ G ... body, collecting until ESC \.
    Body,
    /// Inside body, seen ESC (could be the start of ESC \ terminator).
    BodyEsc,
}

/// Upper bound on a single APC sequence we will buffer (32 MiB of base64
/// covers a ~24 MiB image, far above what real programs chunk at).
const MAX_APC_LEN: usize = 32 * 1024 * 1024;

/// Incremental parser. Feed it bytes; it returns events in stream order.
pub struct StreamParser {
    state: State,
    /// Accumulated plain bytes not yet emitted.
    plain: Vec<u8>,
    /// Accumulated candidate APC bytes (starting at ESC) not yet resolved.
    seq: Vec<u8>,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    pub fn new() -> Self {
        StreamParser {
            state: State::Ground,
            plain: Vec::new(),
            seq: Vec::new(),
        }
    }

    fn flush_plain(&mut self, events: &mut Vec<Event>) {
        if !self.plain.is_empty() {
            events.push(Event::Passthrough(std::mem::take(&mut self.plain)));
        }
    }

    /// Abort the current candidate sequence: its bytes become plain output.
    fn abort_seq(&mut self) {
        self.plain.append(&mut self.seq);
        self.state = State::Ground;
    }

    fn finish_seq(&mut self, events: &mut Vec<Event>) {
        let raw = std::mem::take(&mut self.seq);
        // Body sits between "ESC _ G" and trailing "ESC \".
        let body = &raw[APC_PREFIX.len()..raw.len() - 2];
        match GraphicsCommand::parse(body) {
            Ok(cmd) => {
                self.flush_plain(events);
                events.push(Event::Graphics { cmd, raw });
            }
            Err(_) => {
                self.flush_plain(events);
                events.push(Event::Malformed(raw));
            }
        }
        self.state = State::Ground;
    }

    /// Feed a chunk of bytes, receiving zero or more events.
    pub fn feed(&mut self, input: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &b in input {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.seq.push(b);
                        self.state = State::Esc;
                    } else {
                        self.plain.push(b);
                    }
                }
                State::Esc => {
                    if b == b'_' {
                        self.seq.push(b);
                        self.state = State::EscUnderscore;
                    } else if b == 0x1b {
                        // ESC ESC: first ESC is plain, stay in Esc with new one.
                        self.plain.push(0x1b);
                        self.seq.clear();
                        self.seq.push(b);
                    } else {
                        self.seq.push(b);
                        self.abort_seq();
                    }
                }
                State::EscUnderscore => {
                    if b == b'G' {
                        self.seq.push(b);
                        self.state = State::Body;
                    } else {
                        // Some other APC (ESC _ X ...): not ours, forward raw.
                        self.seq.push(b);
                        self.abort_seq();
                    }
                }
                State::Body => {
                    if b == 0x1b {
                        self.seq.push(b);
                        self.state = State::BodyEsc;
                    } else {
                        self.seq.push(b);
                        if self.seq.len() > MAX_APC_LEN {
                            self.abort_seq();
                        }
                    }
                }
                State::BodyEsc => {
                    if b == b'\\' {
                        self.seq.push(b);
                        self.finish_seq(&mut events);
                    } else if b == 0x1b {
                        // Literal ESC inside body followed by another ESC.
                        self.seq.push(b);
                        // stay in BodyEsc: the new ESC may start the terminator
                    } else {
                        self.seq.push(b);
                        self.state = State::Body;
                    }
                }
            }
        }
        self.flush_plain(&mut events);
        events
    }

    /// Signal end of stream. Any partial sequence is returned as passthrough
    /// so no bytes are ever swallowed.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        self.plain.append(&mut self.seq);
        self.state = State::Ground;
        if !self.plain.is_empty() {
            events.push(Event::Passthrough(std::mem::take(&mut self.plain)));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(parser: &mut StreamParser, input: &[u8]) -> Vec<Event> {
        let mut ev = parser.feed(input);
        ev.extend(parser.finish());
        ev
    }

    #[test]
    fn plain_text_passes_through() {
        let mut p = StreamParser::new();
        let ev = collect(&mut p, b"hello \x1b[31mred\x1b[0m world");
        assert_eq!(
            ev,
            vec![Event::Passthrough(
                b"hello \x1b[31mred\x1b[0m world".to_vec()
            )]
        );
    }

    #[test]
    fn extracts_graphics_between_text() {
        let mut p = StreamParser::new();
        let ev = collect(&mut p, b"pre\x1b_Ga=T,f=100;QUJD\x1b\\post");
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0], Event::Passthrough(b"pre".to_vec()));
        match &ev[1] {
            Event::Graphics { cmd, raw } => {
                assert_eq!(cmd.action(), "T");
                assert_eq!(raw, b"\x1b_Ga=T,f=100;QUJD\x1b\\");
            }
            other => panic!("expected graphics event, got {other:?}"),
        }
        assert_eq!(ev[2], Event::Passthrough(b"post".to_vec()));
    }

    #[test]
    fn survives_byte_at_a_time_feeding() {
        let input = b"a\x1b_Gi=1,a=T;QQ==\x1b\\b\x1b_Gm=0;Qg==\x1b\\";
        let mut p = StreamParser::new();
        let mut events = Vec::new();
        for &b in input.iter() {
            events.extend(p.feed(&[b]));
        }
        events.extend(p.finish());
        let graphics: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::Graphics { .. }))
            .collect();
        assert_eq!(graphics.len(), 2);
    }

    #[test]
    fn other_apc_sequences_pass_through() {
        let mut p = StreamParser::new();
        let ev = collect(&mut p, b"\x1b_Xnot-kitty\x1b\\tail");
        // Entire foreign APC is forwarded verbatim.
        let all: Vec<u8> = ev
            .iter()
            .flat_map(|e| match e {
                Event::Passthrough(b) => b.clone(),
                _ => panic!("unexpected event"),
            })
            .collect();
        assert_eq!(all, b"\x1b_Xnot-kitty\x1b\\tail".to_vec());
    }

    #[test]
    fn incomplete_sequence_flushes_on_finish() {
        let mut p = StreamParser::new();
        let mut ev = p.feed(b"\x1b_Ga=T;QUJ");
        assert!(ev.is_empty());
        ev = p.finish();
        assert_eq!(ev, vec![Event::Passthrough(b"\x1b_Ga=T;QUJ".to_vec())]);
    }

    #[test]
    fn malformed_control_is_reported_not_lost() {
        let mut p = StreamParser::new();
        let ev = collect(&mut p, b"\x1b_Gbogus;AA\x1b\\");
        assert_eq!(ev, vec![Event::Malformed(b"\x1b_Gbogus;AA\x1b\\".to_vec())]);
    }

    #[test]
    fn csi_and_osc_untouched() {
        let bytes = b"\x1b]0;title\x07\x1b[2J\x1b[H".to_vec();
        let mut p = StreamParser::new();
        let ev = collect(&mut p, &bytes);
        let all: Vec<u8> = ev
            .iter()
            .flat_map(|e| match e {
                Event::Passthrough(b) => b.clone(),
                _ => panic!("unexpected event"),
            })
            .collect();
        assert_eq!(all, bytes);
    }
}
