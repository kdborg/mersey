# Mersey Blink — Chromium integration (Stage B)

> ⚠️ **Mersey Blink (Experimental).** This is an experimental Mersey-engine fork
> of Chromium, **not for production use**. It is not affiliated with, endorsed
> by, or supported by Google or the Chromium project; "Chromium" and "Blink"
> here name the upstream engine this fork is built on, not the shipping product.
> The build is renamed to **Mersey Blink** so it can't be confused with a real
> Chromium/Chrome install.

**The fork exists.** `<script type="text/mersey">` in a page is compiled and
executed by the Mersey engine inside Blink — no V8, no WASM in the stack —
through the same C ABI that `native/host_demo.c` and
`crates/mersey_capi/tests/abi.rs` drive in this repository.

The Chromium tree is not *in* this repo (a checkout is 29GB), but the fork is
built and committed in the checkout beside it:

```
browsers/chromium/src   branch: main
  arm64 linux host: system gperf instead of the missing CIPD package
  Mersey: run <script type="text/mersey"> on the Mersey engine
  Mersey: document the arm64-host toolchain substitutions
```

## What landed

| | |
|---|---|
| `//third_party/mersey` | the engine behind `include/mersey.h`. A prebuilt Rust staticlib, refreshed by `refresh.sh` from this repo. |
| `core/script/mersey_script_runner.{h,cc}` | one engine context per `Document` (a `Supplement<Document>`), the host table wired to the console, the element surface and Blink's task runners. An event listener re-enters the engine through `msy_context_invoke`. |
| `core/script/script_loader.{h,cc}` | `ScriptTypeAtPrepare::kMersey`. An **inline** Mersey script runs at prepare time, the way an inline speculation-rules block is consumed. |
| `build/config/clang/clang.gni` | `use_tot_clang_flags`, so a released system clang can carry an arm64 build. |

## Building it

```bash
chromium/setup-arm64-host.sh browsers/chromium/src   # the host substitutions, scripted
cp chromium/args.arm64.gn browsers/chromium/src/out/mersey-arm64/args.gn
cd browsers/chromium/src && gn gen out/mersey-arm64
autoninja -C out/mersey-arm64 libblink_core.so
```

Chromium ships **no hermetic toolchain for linux-arm64** — every prebuilt in
`third_party` is x86-64, and an arm64 host cannot run them. So the build is
carried by native tools: clang (plus a shadow resource-dir tree, because
Ubuntu's `clang_rt` layout is not Chromium's), the Rust toolchain (**rustc 1.98
exactly** — a newer nightly mangles the allocator symbols differently from the
std it ships, and nothing links), `bindgen` and `rustfmt` (needing a *real*
`libclang.so`, not a symlink), lld instead of mold, ninja instead of siso, node
pinned to the exact version `third_party/node` expects, and gperf built from
source because **no linux-arm64 CIPD package for it exists at all**.

Two fork-side GN args carry the rest: `use_tot_clang_flags` (five flags only
tip-of-tree clang knows) and `rust_std_in_executable_only` (Chromium's Rust
allocator crate is linked into the executable, not into each component `.so`;
outside the hermetic toolchain that leaves std's allocator symbols unresolved in
every DSO and `-Wl,-z,defs` kills every link — they resolve at load time, which
is what a component build *is*, and `no_unresolved_symbols` already makes the
same exemption for sanitizer instrumentation).

**None of this is needed on x86-64 Linux, macOS or Windows**, where the hermetic
toolchain works and only `target_cpu`/`target_os` change. That is what makes the
rest of the matrix a configuration exercise rather than another archaeology, and
it is why it is scripted rather than left in a shell history.

## Why the ABI came first

The C ABI was a year behind the engine it wrapped: five hooks (`print`,
`error`, and three fake-DOM calls from the Phase 6 demo) against a `Host`
trait that had grown the universal object bridge, promises, capabilities,
time and entropy. Blink would have been talking to the demo, not the engine.

So before any Blink code: **`msy_abi_version`**, the full host table (bridge,
caps, time, entropy), **module graphs** (`scan_imports` → the host fetches →
`run_graph`, the same payload the browser loader builds), **callbacks with
arguments**, **`MSY_FLAG_NO_JIT`** for sandboxes that forbid a second JIT,
and — the part that matters most — the loader itself moved into
`mersey_interp::embed`, so the WASM boundary and the C boundary now *share one
implementation* rather than drifting apart.

`crates/mersey_capi/tests/abi.rs` drives all of it through the `extern "C"`
symbols against a mock page: a handle table of fake DOM objects, events with
JSON payloads, a promise the host settles **after** the script returned, and a
capability list the engine enforces. When the Blink glue misbehaves, that file
is the evidence for which side of the boundary is wrong.

## Next

1. **The universal bridge in Blink.** The runner wires the console and the
   small element surface; `web_get`/`web_set`/`web_call`/`web_new` over real
   DOM objects is the same table, filled in — and the engine side is already
   proven by the WASM path's live-realm assertions.
2. **External scripts.** `scan_imports`/`run_graph` are in the ABI; wiring
   them to `ScriptResource` is what makes `<script src="app.mersey">` work
   (CORS/SRI/CSP are Blink's, and stay Blink's).
3. **Build the crate graph with GN** (`rust_static_library`) rather than a
   prebuilt, so all six platform/arch combinations come from one build. This
   is what turns "arm64 Linux works" into the full matrix.
4. **DevTools.** Stack traces already cross as strings; CDP is post-fork.
