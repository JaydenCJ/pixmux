//! Kitty graphics protocol control-data parsing and serialization.
//!
//! A kitty graphics escape sequence is an APC of the form:
//! `ESC _ G <control data> ; <base64 payload> ESC \`
//! where control data is a comma-separated list of `key=value` pairs.
//! Reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::collections::BTreeMap;
use std::fmt;

/// APC introducer for a kitty graphics command: ESC _ G
pub const APC_PREFIX: &[u8] = b"\x1b_G";
/// String terminator: ESC \
pub const ST: &[u8] = b"\x1b\\";

/// Error type for protocol-level failures.
#[derive(Debug, PartialEq, Eq)]
pub struct ProtocolError(pub String);

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kitty graphics protocol error: {}", self.0)
    }
}

impl std::error::Error for ProtocolError {}

/// A parsed kitty graphics command: ordered control keys + raw payload bytes.
///
/// Keys are stored in a sorted map so serialization is deterministic; the
/// original wire form is preserved separately by the stream parser when
/// byte-exact passthrough is needed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphicsCommand {
    pub keys: BTreeMap<String, String>,
    /// Raw payload as it appeared on the wire (usually base64), NOT decoded.
    pub payload: Vec<u8>,
}

impl GraphicsCommand {
    /// Parse the body of an APC sequence (bytes between `ESC _ G` and `ESC \`).
    pub fn parse(body: &[u8]) -> Result<Self, ProtocolError> {
        let (ctrl, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], body[i + 1..].to_vec()),
            None => (body, Vec::new()),
        };
        let ctrl_str = std::str::from_utf8(ctrl)
            .map_err(|_| ProtocolError("control data is not valid UTF-8".into()))?;
        let mut keys = BTreeMap::new();
        for pair in ctrl_str.split(',') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair
                .split_once('=')
                .ok_or_else(|| ProtocolError(format!("malformed key-value pair: {pair:?}")))?;
            if k.is_empty() {
                return Err(ProtocolError(format!("empty key in pair: {pair:?}")));
            }
            keys.insert(k.to_string(), v.to_string());
        }
        Ok(GraphicsCommand { keys, payload })
    }

    /// Serialize back to a full APC sequence (`ESC _ G ... ESC \`).
    pub fn to_apc(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload.len() + 64);
        out.extend_from_slice(APC_PREFIX);
        let ctrl: Vec<String> = self.keys.iter().map(|(k, v)| format!("{k}={v}")).collect();
        out.extend_from_slice(ctrl.join(",").as_bytes());
        if !self.payload.is_empty() {
            out.push(b';');
            out.extend_from_slice(&self.payload);
        }
        out.extend_from_slice(ST);
        out
    }

    /// Get a key's value as string.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(|s| s.as_str())
    }

    /// Get a key's value parsed as u32 (missing key -> None, bad number -> None).
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key).and_then(|v| v.parse().ok())
    }

    /// Action key `a`; kitty defaults to `t` (transmit) when absent.
    pub fn action(&self) -> &str {
        self.get("a").unwrap_or("t")
    }

    /// Pixel format key `f`; kitty defaults to 32 (RGBA) when absent.
    pub fn format(&self) -> u32 {
        self.get_u32("f").unwrap_or(32)
    }

    /// `m=1` means more chunks follow.
    pub fn more_chunks(&self) -> bool {
        self.get("m") == Some("1")
    }

    /// Transmission medium `t`; kitty defaults to `d` (direct / inline base64).
    pub fn medium(&self) -> &str {
        self.get("t").unwrap_or("d")
    }

    /// Image id `i`, if present.
    pub fn image_id(&self) -> Option<u32> {
        self.get_u32("i")
    }

    /// True when this command is a support query (`a=q`).
    pub fn is_query(&self) -> bool {
        self.action() == "q"
    }
}

/// Build the terminal's OK response to a graphics command, echoing back the
/// `i`/`I` identifiers as a real kitty terminal does.
pub fn ok_response(cmd: &GraphicsCommand) -> Vec<u8> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(i) = cmd.get("i") {
        ids.push(format!("i={i}"));
    }
    if let Some(n) = cmd.get("I") {
        ids.push(format!("I={n}"));
    }
    let mut out = Vec::new();
    out.extend_from_slice(APC_PREFIX);
    out.extend_from_slice(ids.join(",").as_bytes());
    out.extend_from_slice(b";OK");
    out.extend_from_slice(ST);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_control_and_payload() {
        let cmd = GraphicsCommand::parse(b"a=T,f=100,i=42;QUJD").unwrap();
        assert_eq!(cmd.action(), "T");
        assert_eq!(cmd.format(), 100);
        assert_eq!(cmd.image_id(), Some(42));
        assert_eq!(cmd.payload, b"QUJD");
    }

    #[test]
    fn parses_without_payload() {
        let cmd = GraphicsCommand::parse(b"a=d,d=A").unwrap();
        assert_eq!(cmd.action(), "d");
        assert!(cmd.payload.is_empty());
    }

    #[test]
    fn defaults_match_kitty_spec() {
        let cmd = GraphicsCommand::parse(b"i=7;QQ==").unwrap();
        assert_eq!(cmd.action(), "t");
        assert_eq!(cmd.format(), 32);
        assert_eq!(cmd.medium(), "d");
        assert!(!cmd.more_chunks());
    }

    #[test]
    fn rejects_malformed_pair() {
        assert!(GraphicsCommand::parse(b"a=T,junk;AAAA").is_err());
        assert!(GraphicsCommand::parse(b"=x;AAAA").is_err());
    }

    #[test]
    fn roundtrips_to_apc() {
        let cmd = GraphicsCommand::parse(b"a=T,f=100;QUJD").unwrap();
        assert_eq!(cmd.to_apc(), b"\x1b_Ga=T,f=100;QUJD\x1b\\".to_vec());
    }

    #[test]
    fn ok_response_echoes_ids() {
        let cmd = GraphicsCommand::parse(b"a=q,i=31,s=1,v=1;QUJD").unwrap();
        assert!(cmd.is_query());
        assert_eq!(ok_response(&cmd), b"\x1b_Gi=31;OK\x1b\\".to_vec());
    }
}
