#!/usr/bin/env bash
# Regenerate the Servo fork's bridge prelude from the canonical, tested
# web/mersey-bridge.js — the same reflective bridge the WASM polyfill drives.
# Strips the ES import (empty generated-binding tables => pure reflection) and
# appends the epilogue that instantiates __merseyBridge in the page realm,
# wired to the __merseyInvoke native the module injects.
#
# Mirrors ~/gecko/dom/mersey/refresh.sh. Called by servo/apply.sh; run directly
# to refresh the in-repo servo/mersey/bridge.js after editing mersey-bridge.js.
#
# Usage:  servo/refresh-bridge.sh [OUT] [MERSEY_REPO]
#         default OUT: servo/mersey/bridge.js
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
MERSEY_REPO="${2:-$(cd "$HERE/.." && pwd)}"
OUT="${1:-$HERE/mersey/bridge.js}"

python3 - "$MERSEY_REPO" "$OUT" <<'PY'
import sys
repo, out = sys.argv[1], sys.argv[2]
src = open(f"{repo}/web/mersey-bridge.js").read()
src = src.replace(
    'import { CALLS, GETS, SETS, CTORS } from "./mersey-bindings.gen.js";',
    'const CALLS = new Map(), GETS = new Map(), SETS = new Map(), CTORS = new Map();')
src = src.replace('export function makeBridge', 'function makeBridge')
open(out, "w").write(src + """
globalThis.__merseyBridge = makeBridge(globalThis, function (cb, argsJson) {
  return globalThis.__merseyInvoke(cb, argsJson);
});
// Mersey runs natively here: the Stage A polyfill loader sees this and
// stands down (no WASM fetch, no double execution).
globalThis.merseyNative = true;
""")
print(f"wrote {out}")
PY
