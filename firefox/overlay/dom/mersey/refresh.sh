#!/usr/bin/env bash
# Sync the engine's C ABI header from its repository.
#
# Note what is NOT here: no `cargo build`, no prebuilt library, no soname, no
# rpath, no sysroot. The engine is a *crate* (dom/mersey/rust) in gkrust's
# graph — `mach build` compiles it from source with Firefox's own Rust, into
# Firefox's own libxul, against Firefox's own `std`. Editing the engine and
# rebuilding the browser is one command.
#
# That is the difference from the Chromium fork, where the engine is a prebuilt
# `.so` that must be linked against Chromium's sysroot and kept from colliding
# with Chromium's `std`.
set -euo pipefail
MERSEY_REPO="${MERSEY_REPO:-$HOME/Work/mersey}"
HERE="$(cd "$(dirname "$0")" && pwd)"
cp "$MERSEY_REPO/crates/mersey_capi/include/mersey.h" "$HERE/include/"

# Regenerate the embedded bridge from the tested mersey-bridge.js: strip the ES
# import (empty generated-binding tables => pure reflection), and wrap it to
# instantiate __merseyBridge in the page realm.
python3 - "$MERSEY_REPO" "$HERE" <<'PYEOF'
import sys
repo, here = sys.argv[1], sys.argv[2]
src = open(f"{repo}/web/mersey-bridge.js").read()
src = src.replace(
    'import { CALLS, GETS, SETS, CTORS } from "./mersey-bindings.gen.js";',
    'const CALLS = new Map(), GETS = new Map(), SETS = new Map(), CTORS = new Map();')
src = src.replace('export function makeBridge', 'function makeBridge')
boot = src + """
globalThis.__merseyBridge = makeBridge(globalThis, function (cb, argsJson) {
  return globalThis.__merseyInvoke(cb, argsJson);
});
// Mersey runs natively here: the Stage A polyfill loader sees this and
// stands down (no WASM fetch, no double execution).
globalThis.merseyNative = true;
"""
open(f"{here}/MerseyBridgeSource.h", "w").write(
    '/* Generated from mersey-bridge.js by dom/mersey/refresh.sh. Do not edit. */\n'
    '#ifndef mozilla_dom_MerseyBridgeSource_h\n'
    '#define mozilla_dom_MerseyBridgeSource_h\n'
    'namespace mozilla::dom {\n'
    'static const char* const kMerseyBridgeSource = R"MSYJS(\n'
    + boot +
    '\n)MSYJS";\n'
    '}  // namespace mozilla::dom\n'
    '#endif\n')
PYEOF
echo "refreshed header + bridge from $MERSEY_REPO"
