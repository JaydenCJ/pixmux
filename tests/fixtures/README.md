# Test fixtures

Synthetic terminal byte streams used by `tests/stream_fixtures.rs` and
`tests/cli.rs`. All files are produced by `scripts/gen_fixtures.py` — they
reproduce the wire format of real emitters but are not captures of live
terminal sessions. All files are small (< 2 KB) and deterministic.

| File | Contents |
|---|---|
| `sample.png` | Real 16x16 RGB PNG (gradient), decodable by any PNG reader |
| `icat_chunked.bin` | Synthetic shell session stream: prompt/CSI output interleaved with a 3-chunk kitty graphics transmission (`a=T,f=100`, 4096-byte-style chunking, image id, `q=2`) and a delete command (`a=d`), reproducing `kitty +kitten icat` wire output |
| `icat_chunked.tmux.golden` | The same stream translated for tmux by an independent Python implementation of passthrough wrapping (cross-check golden) |
| `plain_vim_session.bin` | Graphics-free synthetic stream (alternate screen, OSC title, CSI moves, a foreign non-kitty APC) that must pass through every target unchanged |

Regenerate with:

```bash
python3 scripts/gen_fixtures.py
```

The generator is stdlib-only and deterministic; running it twice produces
identical bytes.
