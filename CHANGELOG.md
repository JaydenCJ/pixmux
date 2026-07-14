# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-08

### Added

- Streaming parser for kitty graphics protocol APC sequences that preserves
  all non-graphics bytes exactly, survives arbitrary read-boundary splits,
  and never drops data on malformed or truncated input.
- `pixmux run`: spawn any command under a PTY and translate its kitty
  graphics output live; propagates the child's exit code, sets the initial
  window size, and forwards outer-terminal resizes (SIGWINCH) to the wrapped
  program while it runs.
- `pixmux filter`: stdin-to-stdout stream translator for pipelines.
- `pixmux cat`: display a local PNG through the kitty graphics protocol,
  automatically adapted to the detected multiplexer.
- `pixmux doctor`: environment diagnosis (multiplexer detection, tmux
  `allow-passthrough` hint, outer-terminal capability heuristics).
- tmux target: wraps graphics sequences in `ESC Ptmux;` passthrough DCS with
  ESC doubling, and re-chunks oversized single transmissions to the kitty
  spec's 4096-byte chunk size.
- zellij target: transcodes kitty graphics (f=100 PNG, f=24/32 raw, `o=z`
  zlib, chunked transmissions, `a=p` display-by-id, `a=d` delete) into sixel,
  which zellij renders natively; answers `a=q` capability queries in run mode.
- strip target: removes graphics sequences for plain terminals and logs.
- Wire-format sample test suite (synthetic streams that reproduce real
  emitter output) plus deterministic fixture generator
  (`scripts/gen_fixtures.py`) with an independent Python implementation of
  tmux wrapping as a cross-check golden.
