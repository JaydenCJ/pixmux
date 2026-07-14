//! Environment diagnosis for `pixmux doctor`.

use crate::transform::Target;

/// A single diagnostic line: label, value, and an optional hint.
pub struct Finding {
    pub label: &'static str,
    pub value: String,
    pub hint: Option<String>,
}

/// Snapshot of the environment variables we care about.
pub struct EnvSnapshot {
    pub tmux: Option<String>,
    pub zellij: Option<String>,
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub kitty_window_id: Option<String>,
}

impl EnvSnapshot {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        EnvSnapshot {
            tmux: get("TMUX"),
            zellij: get("ZELLIJ"),
            term: get("TERM"),
            term_program: get("TERM_PROGRAM"),
            kitty_window_id: get("KITTY_WINDOW_ID"),
        }
    }

    /// Which multiplexer are we (most likely) inside?
    pub fn multiplexer(&self) -> &'static str {
        if self.zellij.is_some() {
            "zellij"
        } else if self.tmux.is_some() {
            "tmux"
        } else {
            "none"
        }
    }

    /// The target `--target auto` would resolve to in this environment.
    pub fn auto_target(&self) -> Target {
        match self.multiplexer() {
            "zellij" => Target::Zellij,
            "tmux" => Target::Tmux,
            _ => Target::None,
        }
    }

    /// Does the outer terminal look kitty-graphics capable?
    pub fn outer_looks_kitty(&self) -> bool {
        self.kitty_window_id.is_some()
            || self
                .term
                .as_deref()
                .is_some_and(|t| t.contains("kitty") || t.contains("ghostty"))
            || self.term_program.as_deref().is_some_and(|t| {
                t.eq_ignore_ascii_case("wezterm") || t.eq_ignore_ascii_case("ghostty")
            })
    }

    pub fn findings(&self) -> Vec<Finding> {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unset)".into());
        let mux = self.multiplexer();
        let mut out = vec![
            Finding {
                label: "TERM",
                value: show(&self.term),
                hint: None,
            },
            Finding {
                label: "TERM_PROGRAM",
                value: show(&self.term_program),
                hint: None,
            },
            Finding {
                label: "multiplexer",
                value: mux.into(),
                hint: None,
            },
            Finding {
                label: "auto target",
                value: self.auto_target().to_string(),
                hint: None,
            },
        ];
        match mux {
            "tmux" => {
                out.push(Finding {
                    label: "tmux passthrough",
                    value: "required".into(),
                    hint: Some("run: tmux set -gq allow-passthrough on (tmux >= 3.3)".into()),
                });
                if !self.outer_looks_kitty() {
                    out.push(Finding {
                        label: "outer terminal",
                        value: "kitty graphics support not detected".into(),
                        hint: Some(
                            "passthrough only helps if the terminal running tmux \
                             supports the kitty graphics protocol (kitty, ghostty, wezterm)"
                                .into(),
                        ),
                    });
                }
            }
            "zellij" => {
                out.push(Finding {
                    label: "zellij mode",
                    value: "kitty graphics will be transcoded to sixel".into(),
                    hint: Some("zellij renders sixel; no zellij config needed".into()),
                });
            }
            _ => {
                out.push(Finding {
                    label: "note",
                    value: "no multiplexer detected; pixmux will pass bytes through".into(),
                    hint: None,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(tmux: Option<&str>, zellij: Option<&str>, term: Option<&str>) -> EnvSnapshot {
        EnvSnapshot {
            tmux: tmux.map(String::from),
            zellij: zellij.map(String::from),
            term: term.map(String::from),
            term_program: None,
            kitty_window_id: None,
        }
    }

    #[test]
    fn zellij_wins_over_tmux() {
        let s = snap(Some("/tmp/tmux-0/default,123,0"), Some("0"), Some("xterm"));
        assert_eq!(s.multiplexer(), "zellij");
        assert_eq!(s.auto_target(), Target::Zellij);
    }

    #[test]
    fn tmux_detection_and_hint() {
        let s = snap(
            Some("/tmp/tmux-0/default,123,0"),
            None,
            Some("screen-256color"),
        );
        assert_eq!(s.multiplexer(), "tmux");
        let findings = s.findings();
        assert!(findings.iter().any(|f| f
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("allow-passthrough"))));
    }

    #[test]
    fn kitty_outer_detected_via_term() {
        let s = snap(None, None, Some("xterm-kitty"));
        assert!(s.outer_looks_kitty());
        assert_eq!(s.auto_target(), Target::None);
    }
}
