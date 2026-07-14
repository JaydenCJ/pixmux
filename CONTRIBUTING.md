# Contributing to pixmux

Thanks for your interest in improving pixmux. This document explains how to
get a development environment running and what we expect from contributions.

## Development setup

Requirements: stable Rust (1.75+) and Python 3 (stdlib only, used to
regenerate test fixtures).

```bash
git clone https://github.com/JaydenCJ/pixmux
cd pixmux
cargo build
cargo test
bash scripts/smoke.sh   # end-to-end check; must print "SMOKE OK"
```

## Project layout

| Path | Purpose |
|---|---|
| `src/parser.rs` | Incremental scanner: terminal bytes -> events |
| `src/protocol.rs` | Kitty graphics control-data parse/serialize |
| `src/tmux.rs` | tmux passthrough wrapping + re-chunking |
| `src/assemble.rs` | Chunk reassembly, base64/zlib/PNG decoding |
| `src/sixel.rs` | Deterministic sixel encoder (zellij target) |
| `src/transform.rs` | Target-aware translation pipeline |
| `src/pty.rs` | `pixmux run` PTY plumbing (Unix) |
| `tests/` | Unit + wire-format sample + CLI integration tests |
| `scripts/gen_fixtures.py` | Regenerates `tests/fixtures/` deterministically |

## Guidelines

- **Tests first-class**: every parser/encoder change needs a test. If you
  touch the wire format handling, extend the synthetic fixtures via
  `scripts/gen_fixtures.py` rather than hand-editing binary files.
- **No new dependencies without discussion**: open an issue first. The
  dependency budget is deliberately small.
- **Code style**: `cargo fmt` and `cargo clippy -- -D warnings` must pass.
- **Comments and identifiers in English.**
- **Byte fidelity is the contract**: pixmux must never alter bytes it does
  not understand. When in doubt, pass through and add a note, never drop.

## Reporting issues

Please include: your terminal, multiplexer and versions (`tmux -V` /
`zellij --version`), the program that emits the graphics, and if possible a
recording of the raw byte stream (`program | tee /tmp/rec.bin` outside the
multiplexer). Small recordings make parser bugs reproducible in minutes.

## License

By contributing you agree that your contributions are licensed under the MIT
License of this repository.
