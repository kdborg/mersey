#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kirk D. Brown

# Conformance gate for the JS backend: every runtime golden, transpiled and
# run under node, must match the engine's expected output byte for byte.
# Usage: tools/jsconf.sh [case-name]   (no arg = all cases)
set -u
cd "$(dirname "$0")/.."
BIN=target/release/mersey
DIR=tests/conformance/runtime
OUT=/tmp/msyjs
mkdir -p "$OUT"
pass=0; fail=0; skip=0; failed=()
for f in "$DIR"/*.mersey; do
  name=$(basename "$f" .mersey)
  [ $# -ge 1 ] && [ "$name" != "$1" ] && continue
  exp="$DIR/$name.expect"
  [ -f "$exp" ] || continue
  # Single-module gate, like the WASM harness: cases that import another
  # file need the module-graph loader.
  if grep -qE 'from "\.\.?/|import\("\.\.?/' "$f"; then skip=$((skip+1)); continue; fi
  if ! "$BIN" js "$f" > "$OUT/$name.mjs" 2>"$OUT/$name.diag"; then
    fail=$((fail+1)); failed+=("$name (transpile)"); continue
  fi
  got=$(cd "$OUT" && timeout 20 node "$name.mjs" 2>&1)
  if [ "$got" == "$(cat "$exp")" ]; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); failed+=("$name")
    diff <(cat "$exp") <(echo "$got") > "$OUT/$name.diff" 2>&1
  fi
done
echo "pass=$pass fail=$fail skip=$skip"
for f in "${failed[@]:-}"; do [ -n "$f" ] && echo "  FAIL $f"; done
