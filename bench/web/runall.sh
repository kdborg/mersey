#!/bin/sh

#REPEATS=15 node run.mjs                 # stock Chromium + Firefox (js, poly)
#REPEATS=15 node run-native.mjs          # Firefox fork (native)
REPEATS=15 node run-native-chromium.mjs # Chromium fork (native)
REPEATS=15 node run-engine.mjs          # wasm engine over the Node stub realm

cd ../cli && REPEATS=25 node run.mjs     # node/bun/deno vs the Mersey CLI


node report.mjs && node gen-report-data.mjs && node report-pertech.mjs && node report-jsnative.mjs

