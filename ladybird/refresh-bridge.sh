#!/usr/bin/env bash
# Regenerate the Ladybird fork's reflective bridge from the canonical, tested
# web/mersey-bridge.js — the same bridge the WASM polyfill and the Gecko/Servo
# forks drive. Strips the ES import (empty generated-binding tables => pure
# reflection) and appends the epilogue that instantiates __merseyBridge in the
# page realm, wired to the __merseyInvoke native the module injects.
#
# Emits two files:
#   mersey/bridge.js     the readable JS (parity with servo/mersey/bridge.js;
#                        for humans and diffs — not compiled directly)
#   mersey/bridge.js.h   a C++ header wrapping that JS in one raw string literal
#                        (namespace Web::Mersey { constexpr StringView BRIDGE_JS })
#                        — what MerseyScriptRunner.cpp #includes, the C++ stand-in
#                        for Servo's include_str!("bridge.js").
#
# Mirrors servo/refresh-bridge.sh. Called by ladybird/apply.sh; run directly to
# refresh the in-repo files after editing web/mersey-bridge.js.
#
# Usage:  ladybird/refresh-bridge.sh [OUT_JS] [MERSEY_REPO]
#         default OUT_JS: ladybird/mersey/bridge.js (the .h lands beside it)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
MERSEY_REPO="${2:-$(cd "$HERE/.." && pwd)}"
OUT_JS="${1:-$HERE/mersey/bridge.js}"

python3 - "$MERSEY_REPO" "$OUT_JS" <<'PY'
import sys
repo, out_js = sys.argv[1], sys.argv[2]
src = open(f"{repo}/web/mersey-bridge.js").read()
src = src.replace(
    'import { CALLS, GETS, SETS, CTORS } from "./mersey-bindings.gen.js";',
    'const CALLS = new Map(), GETS = new Map(), SETS = new Map(), CTORS = new Map();')
src = src.replace('export function makeBridge', 'function makeBridge')
src += """
globalThis.__merseyBridge = makeBridge(globalThis, function (cb, argsJson) {
  return globalThis.__merseyInvoke(cb, argsJson);
});
"""
open(out_js, "w").write(src)
print(f"wrote {out_js}")

# The raw-string delimiter must not occur in the JS; MERSEYBRIDGE is safe.
assert ')MERSEYBRIDGE"' not in src, "delimiter collision in bridge JS"
out_h = out_js + ".h"
header = (
    "// Generated from web/mersey-bridge.js by ladybird/refresh-bridge.sh.\n"
    "// Do not edit by hand — edit web/mersey-bridge.js and re-run the script.\n"
    "#pragma once\n"
    "#include <AK/StringView.h>\n"
    "namespace Web::Mersey {\n"
    "inline constexpr AK::StringView BRIDGE_JS = R\"MERSEYBRIDGE(\n"
    + src +
    ")MERSEYBRIDGE\"sv;\n"
    "}\n"
)
open(out_h, "w").write(header)
print(f"wrote {out_h}")
PY
