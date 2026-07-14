#!/usr/bin/env bash
# Smoke test: exercises the pixmux CLI end to end (build + core commands).
# Self-asserting; prints "SMOKE OK" and exits 0 only if every check passes.
set -euo pipefail

cd "$(dirname "$0")/.."

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

fail() {
  echo "SMOKE FAIL: $1" >&2
  exit 1
}

echo "[smoke] building release binary..."
cargo build --release --quiet
BIN=target/release/pixmux
[ -x "$BIN" ] || fail "binary not built"

echo "[smoke] regenerating fixtures (determinism check)..."
python3 scripts/gen_fixtures.py >/dev/null
SUM1=$(cksum tests/fixtures/icat_chunked.bin)
python3 scripts/gen_fixtures.py >/dev/null
SUM2=$(cksum tests/fixtures/icat_chunked.bin)
[ "$SUM1" = "$SUM2" ] || fail "fixture generator is not deterministic"

echo "[smoke] --version / --help..."
"$BIN" --version | grep -q '^pixmux 0\.1\.0$' || fail "--version mismatch"
"$BIN" --help | grep -q 'filter' || fail "--help missing subcommands"

echo "[smoke] filter --target tmux matches golden..."
"$BIN" filter --target tmux <tests/fixtures/icat_chunked.bin >"$WORKDIR/tmux.out"
cmp tests/fixtures/icat_chunked.tmux.golden "$WORKDIR/tmux.out" \
  || fail "tmux translation differs from golden"

echo "[smoke] filter --target zellij produces sixel, no kitty APC leaks..."
"$BIN" filter --target zellij <tests/fixtures/icat_chunked.bin >"$WORKDIR/zellij.out"
grep -q $'\x1bP0;1;0q' "$WORKDIR/zellij.out" || fail "no sixel header in zellij output"
if grep -q $'\x1b_G' "$WORKDIR/zellij.out"; then
  fail "kitty APC leaked into zellij output"
fi

echo "[smoke] cat renders a PNG for every target..."
"$BIN" cat --target none tests/fixtures/sample.png | grep -q $'\x1b_Ga=T,f=100' \
  || fail "cat --target none missing kitty APC"
"$BIN" cat --target tmux tests/fixtures/sample.png | grep -q $'\x1bPtmux;' \
  || fail "cat --target tmux missing passthrough wrapper"
"$BIN" cat --target zellij tests/fixtures/sample.png | grep -q $'\x1bP0;1;0q' \
  || fail "cat --target zellij missing sixel"

echo "[smoke] run: PTY round-trip translates child output..."
RUN_OUT=$("$BIN" run --target tmux -- printf 'X\033_Ga=T,f=100;QUJD\033\\Y' | cat -v)
echo "$RUN_OUT" | grep -q 'Ptmux;' || fail "run mode did not wrap graphics"
echo "$RUN_OUT" | grep -q 'X' || fail "run mode lost plain output"

echo "[smoke] run: exit code propagation..."
set +e
"$BIN" run --target none -- sh -c 'exit 5'
CODE=$?
set -e
[ "$CODE" -eq 5 ] || fail "expected exit 5, got $CODE"

echo "[smoke] doctor exits 0..."
"$BIN" doctor >/dev/null || fail "doctor failed"

echo "[smoke] error handling: missing file -> non-zero + stderr message..."
set +e
ERR=$("$BIN" cat /no/such/file.png 2>&1 >/dev/null)
CODE=$?
set -e
[ "$CODE" -ne 0 ] || fail "missing file should fail"
echo "$ERR" | grep -q 'cannot read' || fail "missing human-readable error"

echo "SMOKE OK"
