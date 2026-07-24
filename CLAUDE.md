# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Mersey is a strictly-typed, class-based language for web browsers (loaded via
`<script type="text/mersey">` but run by its own engine, not the JS engine) and
standalone use. One mode (strict), no prototypes, sealed nominal classes,
C-style numeric-only implicit conversion, capability-scoped I/O (deny by
default, spec §5.3), no `eval`. The spec lives in `docs/spec/`, the phased plan
in `ROADMAP.md` — consult both before proposing semantics; several decisions
are user-fixed there. Engine strings are UTF-16 (WTF-16) with JS-aligned
code-unit semantics — this is a decided point some older docs contradict, and
benchmark checksum parity depends on it.

## Build and test

```bash
cargo build --release                 # workspace; CLI at target/release/mersey
cargo test                            # Rust unit/integration tests
./target/release/mersey run app.mersey        # check + execute (caps: --allow-read --allow-random …)
./target/release/mersey compile f.mersey      # check + dump verified bytecode (fast typecheck smoke)
./target/release/mersey test [path]           # run *.test.mersey (tests/mersey, tests/conformance)
web/build-and-test.sh                 # build mersey_wasm (wasm32-unknown-unknown), copy into web/, run all web/test/*.mjs
```

`cargo build --release -p mersey_capi` produces `target/release/libmersey_capi.a`,
the staticlib the Chromium/Ladybird forks link (Servo builds the crate itself;
Gecko vendors it). The C ABI header is `crates/mersey_capi/include/mersey.h`;
`msy_abi_version()` must match `MSY_ABI_VERSION` in every host.

## Architecture

Crates (workspace):
- `mersey_front` — lexer/parser/binder/checker. `webapi.gen.mersey` (via
  `tools/webidl-gen` over `@webref/idl`) makes the whole standardized WebIDL
  surface ambient types; `browser:dom` imports resolve any live web global.
- `mersey_interp` — tree-walker + bytecode VM, GC, the host-table trait
  (`web_*` hooks) every embedder implements. Capability gating lives here.
- `mersey_jit` — Cranelift Tier-1. Compiled code reaches the host through the
  same table (typed `web_bind` ids for hot numeric calls like canvas fillRect).
- `mersey_capi` — the C ABI (`msy_context_*`); the one boundary all four
  browser forks and `native/host_demo.c` share.
- `mersey_wasm` — the engine compiled to WASM for the Stage A polyfill.
- `mersey_cli`, `mersey_js` (transpile backend), `mersey_fuzz`.

The web bridge, in one sentence: the engine never touches JS objects — it holds
integer handles and calls host hooks in tiers of decreasing cost
(reflective JSON `web_get/set/call/new` → interned ids `web_*_id/scalars` →
wide UTF-16 `web_*_u16` with a refs bitmask → typed `web_bind` → per-fork
direct-C++ "hot method" paths). Every tier is optional; NULL hooks fall back a
tier, and identical checksums across tiers/browsers are the correctness proof.
`web/mersey-bridge.js` is the canonical reflective implementation (handles are
indices into its table, objects deduped via `handleFor`; mersey closures cross
as `{"__cb__":id}` and re-enter through `__merseyInvoke`).

Browser forks (checkouts live under `~/Work/mersey/browsers/`, NOT in this repo):
- `browsers/firefox` (dom/mersey, branch `mersey`; `browsers/gecko` symlinks to
  it), `browsers/chromium/src` (blink core/script/mersey_script_runner, branch
  `main`), `browsers/servo` (components/script/mersey), `browsers/ladybird`
  (Libraries/LibWeb/Mersey). The full browser source is never stored here: each
  fork's Mersey delta lives in the repo as `<fork>/overlay/` (snapshots) +
  `<fork>/BASELINE` (pinned upstream revision + regen/build hooks), and the fork
  is reconstructed on top of pinned upstream — `scripts/fork-overlay.sh
  apply|verify|bootstrap <fork>` for servo/ladybird/firefox, and the bespoke
  `chromium/{apply,bootstrap,verify}.sh` for Chromium's gclient monorepo (which
  can't be a plain git clone). Regenerated, never stored: the engine staticlib,
  Servo's embedded `web/mersey-bridge.js` (`servo/refresh-bridge.sh`), Ladybird's
  copied `mersey.h`, Firefox's vendored Cranelift crates (`mach vendor rust`).
  The Ladybird fork has NO embedded bridge: its host table is native C++ end to
  end (own handle table, LibJS reflection in C++, closures as NativeFunctions).
- CI's `fork-overlays` job guards every overlay against rot (no binaries,
  well-formed BASELINE, patches parse) without needing a multi-GB checkout.

Benchmarks (`bench/web/`): twenty-five web technologies as line-for-line
`js/<wl>.js` + `mersey/<wl>.mersey` twins, self-timed, checksum-verified
bit-for-bit across every leg. Runners: `run.mjs` (stock Chromium/Firefox via
Playwright), `run-tjs.mjs`, `run-firefox-real.mjs` (system Firefox headless,
driverless — Playwright attaches the debugger, which forces all wasm onto
SpiderMonkey's baseline compiler and inflates Firefox wasm legs 5-7×),
`run-servo.mjs`, `run-ladybird.mjs` (stock
Ladybird via `test-web`, fully self-contained inlined pages),
`run-native{,-servo,-ladybird,-chromium}.mjs` for the forks, and
`run-engine.mjs` (no browser: wasm engine over a deterministic stub realm in
Node — the leg `perf-test.mjs` gates on against the committed
`perf-baselines.json`; checksum mismatch always fails, time/mem have
tolerance factors, `--update` re-baselines). Most take
`WL=name,…` (and ladybird `IMPL=`) filters. Results land in
`results.*.json`; re-running `run-native-ladybird.mjs` drops the `rss` fields —
follow it with `run-native-ladybird-mem.mjs` (a separate peak-PSS poller; the
time runner can't sample test-web's short-lived processes). After ANY results
refresh regenerate all three report surfaces: `report.mjs` → `REPORT.md`;
`gen-report-data.mjs` → `report.html`'s baked DATA block; `report-pertech.mjs`
→ `report-pertech.html` — never hand-edit those numbers. Adding a workload = the two twin files (auto-
discovered) + the hardcoded lists in `run-native-servo.mjs`,
`run-native-ladybird.mjs`, `run-ladybird.mjs`, `run-native-ladybird-mem.mjs`,
`perf-test.mjs` (update list) + `perf-test.mjs --update` for its baseline; if
it needs a web API the engine leg's stub realm (`engine-child.mjs`) may need
the stub too.

## Conventions that bite

- Workload twins must stay line-for-line equivalent and print
  `RESULT <name> <ms> <checksum>`; async workloads self-report from their last
  callback and `pages/js.html` awaits `work()`.
- Benchmarks are timing-sensitive: never run builds and measurements
  concurrently; browser-profile temp dirs can fill `/tmp` — clean
  `/tmp/mersey-*` if the shell starts failing with EDQUOT.
- A fork's every bridge entry point (including the JSON fallbacks) must
  normalize its handle domain before calling the JS bridge — closure/dict
  arguments force the JSON path with whatever receiver the fast tier owned.
- `mersey fmt --write` is the formatter; generated files
  (`web/mersey-bindings.gen.js`, `webapi.gen.mersey`, `bridge.js.h`) are
  regenerated, not edited.
