#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kirk D. Brown

# Build the Stage A engine, place it next to the demo page, and run the
# headless end-to-end test. To try it in a real browser afterwards:
#   cd web && python3 -m http.server 8000   # then open http://localhost:8000
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p mersey_wasm --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/mersey_wasm.wasm web/mersey_wasm.wasm
node web/test/harness.mjs
node web/test/platform.mjs
node web/test/modules.mjs
node web/test/lazy.mjs
node web/test/random.mjs
node web/test/pixels.mjs
node web/test/sw.mjs
node web/test/components.mjs
node web/test/native-skip.mjs
node web/test/repl-browser.mjs
node web/test/errors.mjs
node web/test/security.mjs
