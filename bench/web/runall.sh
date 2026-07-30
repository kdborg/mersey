#!/bin/sh

REPEATS=15 node run.mjs                 # stock Chromium + Firefox (js, poly)
REPEATS=15 node run-native.mjs          # Firefox fork (native)
REPEATS=15 node run-native-chromium.mjs # Chromium fork (native)
REPEATS=15 node run-engine.mjs          # wasm engine over the Node stub realm

# A subshell, so the report generators below still run from bench/web — a bare
# `cd ../cli` left them looking for report.mjs in bench/cli, where there is none,
# and the `&&` chain died on its first command.
(cd ../cli && REPEATS=25 node run.mjs)   # node/bun/deno vs the Mersey CLI

node report.mjs && node report-pertech.mjs && node report-jsnative.mjs
# report.html's DATA block is a manual paste, and it is the linux/CI view:
#   BENCH_PLATFORM=macos node gen-report-data.mjs   # then paste into report.html

