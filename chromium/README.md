# Chromium integration package (Stage B)

This directory is the prepared patch set for the Chromium fork described in
`docs/architecture/browser-integration.md`. **It is not built in this
repository** — a Chromium checkout and build farm are outside this repo's
scope — but every interface it depends on is already real and tested here:

- the engine behind the boundary: `crates/mersey_capi` (C ABI), proven by a
  native host with a fake DOM in `native/host_demo.c` — load, execute,
  DOM writes, event callbacks, diagnostic reporting, all with no V8 and no
  WASM in the stack;
- the behavior contract: `tests/conformance/` — the same goldens the native
  engine, the WASM engine, and the future Blink-hosted engine must match
  (the harness already runs the full runtime suite in the WASM build).

## Patch plan (dependency order)

1. **`//components/mersey`** — C++ wrapper over `mersey_capi` (this
   directory's sources). Rust static lib linked via `//build/rust`;
   `MerseyContext` owns one `msy_context` per execution context.
2. **Script loading** — add `MerseyScript` beside `ClassicScript`/
   `ModuleScript` in Blink (`third_party/blink/renderer/core/script/`).
   `ScriptLoader::DetermineScriptType` learns `type="text/mersey"`;
   fetching reuses `ScriptResource` (CORS/SRI/CSP checks are type-agnostic
   there — spec §5.4 falls out of reusing them).
3. **Execution contexts** — one `MerseyContext` per Document, created on
   first Mersey script, torn down with the ExecutionContext
   (`Supplement<Document>` pattern).
4. **DOM bindings** — the `msy_host_table` grows per-API host functions,
   generated from Blink's WebIDL by a second codegen backend
   (`mersey_bindings_generator`). The v1 hand surface (getElementById,
   createElement, textContent, value, appendChild, remove,
   addEventListener) maps 1:1 to what the loader shim and the C demo host
   already implement.
5. **Scheduling** — Mersey callbacks post to the Document's task runners;
   `msy_context_invoke` is called from the posted task (the engine never
   owns a loop — embedding-api.md rule 1).
6. **DevTools** — CDP Debugger/Runtime domains implemented over the
   engine's diagnostics; deferred until the bytecode tier exposes
   breakpoints.

Build flag: `enable_mersey = true` (GN arg), default off.

## Contents

- `components/mersey/mersey_context.h/.cc` — the C++ wrapper over the ABI
  (compiles against `mersey.h`; Blink types stubbed behind `#if`).
