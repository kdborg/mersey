# Web-platform benchmarks

Performance and memory for real web technologies, across three ways of running
the same program, in four browsers (Chromium, Firefox, Servo, and Ladybird).
Twelve technologies are measured:

| workload | technology | per iteration |
|---|---|---|
| `storage`  | Web Storage        | setItem + getItem roundtrip |
| `json`     | JSON (platform's)  | stringify a small object |
| `url`      | URL                | parse + read pathname/search |
| `crypto`   | Web Crypto         | getRandomValues into a 16-byte buffer |
| `canvas`   | Canvas 2D          | fillRect |
| `dom`      | DOM mutation       | createElement + textContent + appendChild |
| `events`   | DOM events         | new Event + dispatchEvent (listener re-enters the engine) |
| `cssom`    | CSSOM              | className set, classList.contains, style setProperty + getPropertyValue |
| `query`    | Selectors          | querySelectorAll over a 200-node list + NodeList length/index/text reads |
| `encoding` | Encoding           | TextEncoder.encode + TextDecoder.decode roundtrip |
| `timers`   | Timers             | setTimeout arm + clearTimeout disarm (registration cost — a fired chain measures the spec's 4ms nesting clamp, not the API) |
| `fetch`    | Fetch              | sequential HTTP GETs of `/bench/echo` (no-store), status + body read, chained via `.then` |

`fetch` and any future async workloads self-report RESULT from their last
callback; `pages/js.html` awaits `work()` so a Promise-returning js twin times
the same way. The bench server exposes `/bench/echo?i=N` (deterministic
payload, `cache-control: no-store`) so every fetch iteration is a real request.

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

- **Native runs with the Cranelift JIT in every fork** (Gecko, Chromium, Servo,
  Ladybird — see each fork's README for how the engine links). The polyfill is
  the WASM interpreter; plain JS is the browser's own JIT. The web workloads
  are bridge-bound, so the engine tier matters less than the bridge path — the
  JIT mainly shows on `compute`.
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

# REAL Firefox (js + transpiled + polyfill), the system binary launched
# headless with no driver. Playwright drives Firefox with the debugger
# attached, which forces ALL wasm onto SpiderMonkey's baseline compiler
# (microsoft/playwright#11102) — its Firefox wasm numbers are 5-7× slow.
# One fresh profile+process per sample; pages are instrumented at serve time
# to POST their RESULT line back; each sample baselines memory on a blank
# page and self-navigates to the workload in the same process tree.
# WL=…/IMPL=…/FIREFOX_BIN=…/TIMEOUT_MS=… to filter/override:
node bench/web/run-firefox-real.mjs   # -> results.firefox-real.json

# Stock Servo (js + polyfill + transpiled), headless, built from source at
# ~/servo-src/target/release/servoshell (console.log is read from stdout;
# Playwright does not drive Servo). Override the binary with SERVO_BIN=…:
node bench/web/run-servo.mjs    # -> results.servo.json

# Native, via the Firefox fork (needs the fork built at ~/gecko/obj-mersey):
node bench/web/run-native.mjs   # -> results.native.json

# Native, via the Servo fork (needs servoshell built at ~/servo-src with the
# components/script/mersey engine; reflective bridge, Cranelift JIT vendored):
node bench/web/run-native-servo.mjs   # -> results.native.servo.json

# Native Ladybird (needs the fork built at ~/ladybird via ladybird/apply.sh;
# reflective C++→LibJS bridge, Cranelift JIT via the linked libmersey_capi.a).
# Driven through Ladybird's `test-web` harness (no --headless binary exists);
# RESULT is read from each test's captured log. Override with LADYBIRD_SRC/TEST_WEB:
node bench/web/run-native-ladybird.mjs  # -> results.native.ladybird.json

# Stock Ladybird (js / transpiled-JS / WASM-poly, mersey module dormant), via
# test-web with fully self-contained pages — the workload, bridge, bindings and
# base64'd WASM engine are inlined per page because file:// fetch and relative
# module imports are refused there. RESULT comes from each test's actual.txt
# (a println'd console hook); memory is peak PSS minus a blank baseline.
# fetch is skipped (no http origin); compute is js-only (poly/tjs compute would
# measure LibWasm's interpreter interpreting an interpreter):
node bench/web/run-ladybird.mjs         # -> results.ladybird.json

# Merge into REPORT.md:
node bench/web/report.mjs

# Standalone per-technology page (just the 4-implementations × 4-browsers
# panels, statically generated from the results JSONs):
node bench/web/report-pertech.mjs   # -> bench/web/report-pertech.html
```

`run.mjs` needs Playwright's browsers (`cd web && npx playwright install`).
`run-native.mjs` needs the fork built with the `MERSEY_CONSOLE_STDOUT` hook in
`dom/mersey/MerseyScriptRunner.cpp` (it echoes `console.log` to stdout so the
headless harness can read the `RESULT` line).

The Gecko and Chromium native runners serve their generated pages over http
(`server.mjs`, rooted at the repo) rather than `file://`, so the fetch
workload's same-origin `/bench/echo` requests work. The Ladybird native leg
skips `fetch`: `test-web` loads pages from `file://` and its `test(() => {})`
harness completes before an async RESULT lands.
