# Web-platform benchmarks

Performance and memory for real web technologies, across three ways of running
the same program, in four browsers (Chromium, Firefox, Servo, and Ladybird).
Twenty-five technologies are measured:

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
| `websocket`| WebSockets         | one connection to `/bench/ws`, then sequential echo roundtrips chained through the message event (handshake inside the timed region) |
| `idb`      | IndexedDB          | put + get roundtrip in a fresh readwrite transaction per iteration, chained through request success events |
| `streams`  | Streams            | pull-sourced ReadableStream, one chunk per pull, consumed via the reader's read() promise chain |
| `xhr`      | XMLHttpRequest     | one request per iteration to `/bench/echo`, chained through the load event (fetch's legacy counterpart, same endpoint) |
| `worker`   | Web Workers        | one echo worker (`worker-echo.js`), N sequential postMessage roundtrips via the message event (startup inside the timed region) |
| `compression` | Compression Streams | gzip compress + decompress roundtrip per iteration; checksum on the roundtripped text (compressed bytes differ across engines) |
| `bchannel` | Broadcast Channel   | sender + receiver channel pair, N postMessage roundtrips chained through the receiver's message event |
| `geometry` | Geometry interfaces | a DOMMatrix per iteration, translate + scale chained, three components read back (all integral, exact checksum) |
| `blob`     | File API            | a two-part Blob per iteration (array argument across the bridge), size read + a slice's size — fully synchronous |
| `sse`      | Server-sent events  | one EventSource, N server-pushed events counted via the message event; the server holds the stream open (ending it would auto-reconnect) |
| `urlpattern` | URL Pattern       | one compiled URLPattern; a test() + exec() per iteration with the matched pathname input read back |
| `locks`    | Web Locks           | N sequential exclusive acquisitions of one lock, chained through the request promise; the granted callback crosses into the lock manager |
| `msgchannel` | Channel Messaging | one MessageChannel, N postMessage roundtrips port1 → port2 via port2's (explicitly started) message event |

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
- Native is measured in **all four forks** (Gecko, Chromium, Servo, Ladybird),
  each to its architectural limit — see the per-leg absence list under
  "Running" for exactly what each can't measure and why.

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
# ~/Work/mersey/browsers/servo/target/release/servoshell (console.log is read from stdout;
# Playwright does not drive Servo). Override the binary with SERVO_BIN=…:
node bench/web/run-servo.mjs    # -> results.servo.json

# Native, via the Firefox fork (needs the fork built at ~/Work/mersey/browsers/firefox/obj-mersey):
node bench/web/run-native.mjs   # -> results.native.json

# Native, via the Servo fork (needs servoshell built at ~/Work/mersey/browsers/servo with the
# components/script/mersey engine; reflective bridge, Cranelift JIT vendored):
node bench/web/run-native-servo.mjs   # -> results.native.servo.json

# Native Ladybird (needs the fork built at ~/Work/mersey/browsers/ladybird via ladybird/apply.sh;
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

# Engine only — no browser: the wasm engine over a deterministic stub realm
# in Node (real URL/TextEncoder/crypto/JSON/fetch, spec-faithful DOM stubs;
# every checksum matches the browser legs bit-for-bit). One fresh child
# process per sample; memory is the child's peak RSS (VmHWM) minus a blank
# engine child, `heap` is the engine's wasm linear memory. WL=… to filter:
node bench/web/run-engine.mjs   # -> results.engine.json

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
(`server.mjs`, rooted at the repo) rather than `file://`, so the fetch, xhr
and websocket workloads' same-origin requests work (`/bench/echo`,
`/bench/ws` — the Ladybird runners rewrite both to absolute URLs at inline
time, since `test-web` loads pages from `file://`). Known per-leg absences,
all recorded as null rows or list exclusions:

- **Chromium fork**: no async result path at all — fetch, websocket, idb,
  streams, xhr, worker, compression, bchannel, sse, locks, msgchannel (the
  engine's `fetch` import itself reports absent there). Its direct-C++
  bridge's constructor set now covers DOMMatrix and Blob (geometry and blob
  run natively); urlpattern stays absent — Blink's URLPattern API is
  ScriptState-bound, which the no-V8 glue deliberately avoids.
- **Servo**: no IndexedDB and no Web Locks anywhere — everything else runs
  natively, websocket included (the glue pushes the entry-script settings
  around each mersey run, per the spec's "prepare to run a script", so
  bindings that consult `entry_global()` — Location's cross-origin getters —
  work through the bridge).
- **Ladybird stock + fork**: worker is impossible from `file://` pages — the
  worker script would be a cross-origin (absolute-http) URL, which dedicated
  workers refuse. Everything else runs natively: the C++ glue re-enters the
  engine from promise reactions and event tasks alike. (A caution from this
  leg's history: async workloads once read as absent here, but the cause was
  the generated test page ending — `test(() => {})` — before an async RESULT
  could land, not the glue. Async pages now hold the test open; verify the
  harness's completion model before believing an absence.)

## Perf regression tests

The engine-only leg doubles as a pass/fail suite — no browsers, deterministic
checksums, a couple of minutes end to end:

```sh
node bench/web/perf-test.mjs            # compare against perf-baselines.json; exit 1 on regression
node bench/web/perf-test.mjs --update   # re-baseline (commit the new perf-baselines.json)
PERF_WL=storage,json node bench/web/perf-test.mjs   # filter workloads
```

Each technology's time (min of 2 runs), peak RSS and wasm heap are checked
against the committed `perf-baselines.json`. A checksum mismatch always fails
(that is a correctness regression, not a perf question). Tolerances are
generous by design — `PERF_TIME_TOL` (default 1.5×, with a 20 ms floor) and
`PERF_MEM_TOL` (default 1.4×, 8 MiB floor) — this suite exists to catch an
accidental O(n²) or a leaked handle table, not 5% noise. Baselines are
machine-relative: after moving to new hardware, `--update` once. Don't run it
concurrently with builds or other benchmarks.

**Baselines are per platform, and a workload with none is not run at all.** The
file is `{"linux": {…}, "macos": {…}}`, and the no-`--update` path takes its
workload list from the current platform's section — so a platform that has never
been baselined gates *nothing*, silently and with a zero exit. macOS carried two
of twenty-nine for a while, which meant the whole engine leg was passing there
without running. It now carries twenty-eight.

Three of the Linux twenty-nine have no macOS baseline, and the reasons divide:

| workload | why | engine's fault? |
|---|---|---|
| `urlpattern` | `URLPattern is not a constructor` | no — Node 20 on this host has no `URLPattern` |
| `websocket` | `WebSocket is not a constructor` | no — same, no global `WebSocket` |
| `locks` | `stale handle 1` from the bridge | **unresolved** — reproduces at `6c69de6`, so not from the frame sweep or the setter, but not yet explained |

The first two want a newer Node, not engine work. The third is a real question
about handle lifetime under a closure that crosses into the host and a promise
that chains the next iteration; it is recorded here rather than fixed because it
predates the work that found it.

When extending a platform's coverage, filter with `PERF_WL` to the workloads you
are *adding*: `--update` merges `{...baselines, ...fresh}`, so re-running it over
a workload that already has a baseline silently re-records it at today's number
and erases whatever signal it held.

### What a number here can actually resolve

The fork runners (`run-native-*.mjs`) take `n=3` per workload, and for some
workloads that is not enough to call a small difference. Measured on
2026-07-31, same binary, repeat runs of `run-native-chromium.mjs`:

| workload | readings (ms) | spread |
|---|---|---|
| `bchannel` | 116.4 / 134.2 / 138.0 / 153.3 | **32%** |
| `blob` | 2105 / 2195 / 2248 / 2546 | **21%** |
| `streams` | 77.2 / 77.5 / 87.5 / 90.8 | **17%** |
| `frameworkui2` | 43.8 / 49.9 | **12%** |
| `locks` | 163.2 / 169.7 / 173.0 / 177.7 | **9%** |

Three more, measured 2026-08-01 the same way, after a change that could not have
touched them — which is what makes them a noise reading rather than a result:

| workload | readings (ms) | spread |
|---|---|---|
| `worker` | 67.4 / 76.7 / 84.1 / 85.9 | **27%** |
| `json` | 3.36 / 3.44 / 3.48 / 3.92 | **17%** |
| `timers` | 44.4 / 48.7 / 49.7 / 50.8 | **14%** |

`json` is the caution worth keeping: 17% of 3.5ms is 0.6ms, so a percentage on a
sub-5ms workload says almost nothing. Read those rows in absolute terms.
| `websocket` | 41.4 / 41.7 / 43.9 / 44.4 | **7%** |

So a 5% or even 10% move on an async or IPC-shaped workload (`bchannel`,
`websocket`, `streams`, `blob`, `locks`) says nothing on its own. That is
not hypothetical: five of these were once reported as regressions from an
engine change and every one turned out to be inside its own spread. If a
difference on one of them matters, run it several times and quote the
range, or raise `n`.

The compute- and string-shaped workloads (`json`, `mathk`, `encoding`,
`compute`) are far steadier, and a 15%+ move on those is real.
