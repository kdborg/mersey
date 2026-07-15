# Web-platform benchmarks

Performance and memory for real web technologies — Web Storage, JSON, URL, Web
Crypto, Canvas 2D, and DOM mutation — across three ways of running the same
program, in two browsers.

The three implementations of each workload:

| impl | what runs | engine | bridge to web APIs |
|---|---|---|---|
| **js** | the workload in plain JavaScript | the browser's own JS engine (JIT) | direct |
| **polyfill** | the *same workload in Mersey* | Mersey engine compiled to WASM, in a stock browser | JS ↔ WASM |
| **native** | the *same Mersey source* | Mersey engine hosted inside the browser fork | C++ ↔ JS realm |

The `js/*.js` and `mersey/*.mersey` files are line-for-line equivalent, and every
run reports the same checksum, so the three are doing identical work.

## What is measured

- **Time** — wall-clock of the workload loop only, self-timed *inside the
  language* (`performance.now` for JS, `time.monotonic()` for Mersey). Engine
  startup and page load are excluded. Median of 3 runs.
- **Memory** — PSS (proportional set size) of the whole browser process tree,
  workload page minus a blank page. PSS counts shared libraries once, so a delta
  that spans a freshly-spawned renderer is not inflated by libxul/libchrome.

## Caveats (read before quoting numbers)

- **Native runs the Tier-0 interpreter, not the JIT.** The fork embeds the
  engine with `default-features = false`, so Cranelift is not compiled in.
  Native and polyfill are therefore *both* interpreters; plain JS is JIT-compiled.
  Enabling the native JIT is future work and would mainly help compute-bound
  workloads — these are bridge-bound.
- These workloads are dominated by **bridge crossings** (each iteration calls a
  web API), not arithmetic. That is why native (short in-process C++ bridge)
  beats the polyfill (JS→WASM round trip), and why the interpreter tier matters
  less here than the bridge path length.
- The **native memory delta excludes the engine** (it is in the browser binary);
  the **polyfill delta includes it** (~40–52 MB of WASM engine + heap loaded per
  page). That difference is the point, not noise.
- Native is measured only in the Firefox fork so far; the Chromium fork needs the
  same bridge port (pending).

## Running

```sh
# Stock browsers (js + polyfill, Chromium + Firefox) via Playwright:
node bench/web/run.mjs          # -> results.stock.json

# Native, via the Firefox fork (needs the fork built at ~/gecko/obj-mersey):
node bench/web/run-native.mjs   # -> results.native.json

# Merge into REPORT.md:
node bench/web/report.mjs
```

`run.mjs` needs Playwright's browsers (`cd web && npx playwright install`).
`run-native.mjs` needs the fork built with the `MERSEY_CONSOLE_STDOUT` hook in
`dom/mersey/MerseyScriptRunner.cpp` (it echoes `console.log` to stdout so the
headless harness can read the `RESULT` line).
