#!/usr/bin/env bash
# Assemble the probes. Binaryen's `wasm-as` rather than wabt, because wabt 1.0.39
# cannot parse a `(ref $type)` local — the GC proposal's own syntax.
#
#   npm install binaryen      (or have wasm-as on PATH)
#   ./build.sh
set -euo pipefail
cd "$(dirname "$0")"

WASM_AS="${WASM_AS:-$(command -v wasm-as || echo ./node_modules/binaryen/bin/wasm-as)}"
if [ ! -x "$WASM_AS" ] && ! command -v "$WASM_AS" >/dev/null 2>&1; then
  echo "wasm-as not found. npm install binaryen, or set WASM_AS." >&2
  exit 1
fi

for f in probes/*.wat; do
  out="probes/$(basename "$f" .wat).wasm"
  "$WASM_AS" "$f" -o "$out" --enable-gc --enable-reference-types --enable-strings
  echo "built $out"
done
