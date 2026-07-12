# Security review — spec §5 vs. implementation (Phase 7, 2026-07-12)

Line-by-line status of `docs/spec/05-security.md` against the current
engine. ✅ = implemented and tested, 🟡 = implemented at documented scale,
⏳ = deferred with the native-engine track.

## §5.1 Language-level guarantees

| Guarantee | Status | Where |
|---|---|---|
| No dynamic code evaluation (`eval`, `Function(string)`) | ✅ | Not in the grammar; `eval` gets a targeted E0301; dynamic `import()` is inert in the MVP runtime |
| No prototype pollution | ✅ | No prototype mechanism exists; `__proto__`/`prototype` members get targeted E0403; sealed shapes enforced at runtime (`set_member`) and statically (E0403) |
| Memory safety by construction | ✅ | Engine is Rust with `unsafe_code = "forbid"` outside the three FFI crates (`mersey_wasm`, `mersey_jit`, `mersey_capi`); array/string access bounds-checked (RangeError), no uninitialized reads (binder TDZ + checker) |
| Defined arithmetic, no UB | ✅ | Wrapping ops, division traps, masked shifts — identical across tree-walker, VM, and JIT (differential-tested) |
| Access control real at runtime boundaries | 🟡 | `private`/`protected` enforced statically (E0404); the runtime object model does not yet re-check on reflective paths because no reflection API exists (spec defers `std:reflect`) |

## §5.2 Engine requirements

| Requirement | Status | Notes |
|---|---|---|
| W^X JIT | ✅ | cranelift-jit maps pages writable, flips to read-execute at `finalize_definitions`; no page is ever W+X |
| Sandbox-friendly (no engine syscalls) | ✅ | All I/O flows through the `Host` trait / `msy_host_table`; the WASM build imports only host functions |
| Heap isolation per context | ✅ | One `Interp` per context (`msy_context`, WASM instance); no cross-context references are constructible |
| Pointer compression, guard pages, CFI | ⏳ | Belongs to the precise-GC native engine (deferred with Phase 2's scale note) |

## §5.3 Standalone runtime capabilities

| Requirement | Status | Notes |
|---|---|---|
| Deny by default | ✅ | `Host` trait defaults deny; conformance case pins the denial messages |
| `--allow-read` / `--allow-env` | ✅ | CLI flags; browser/WASM host cannot grant them at all |
| Queryable/droppable from inside (`std:caps`) | ✅ | `caps.has/list/drop`; demo app sheds `read` after initialization |

## §5.4 Browser profile

| Requirement | Status | Notes |
|---|---|---|
| CSP / same-origin / SRI | 🟡 | Stage A: loader uses plain `fetch` (same-origin + CSP apply to it as to any script); native SRI/CSP enforcement is Blink's, reused in Stage B by design (`chromium/README.md` step 2) |
| No ambient authority (explicit `browser:dom` import) | ✅ | DOM is import-gated; `mersey audit` shows the surface |

## §5.5 Supply chain

| Requirement | Status | Notes |
|---|---|---|
| `mersey audit` capability report | ✅ | Static import analysis (exact — imports are static) |
| Lockfiles/hashes for module graphs | ⏳ | Waits on the multi-module loader |

## Hardening evidence (Phase 7)

- **Mutation fuzzing** of decode→lex→parse→bind→check over the conformance
  corpus and **grammar-aware differential fuzzing** (generated well-typed
  programs executed on both engines, outputs compared):
  `cargo run --release -p mersey_fuzz -- all <iters> <seed>`.
- 80,000+ iterations across seeds at review time: **one finding, fixed** —
  a parser panic slicing 1-byte unterminated string/template tokens from
  error recovery (regression case `lexer/err-unterminated-min`); zero
  findings since.
- The bytecode verifier runs on every compiled chunk (debug assert + tools);
  the JIT accepts only the provably non-faulting opcode subset.
- Deterministic seeds make every fuzz run reproducible.

## Standing risks (tracked)

1. Rc reference-counting heap: cycles leak (availability, not safety).
2. ~~Parser recursion depth~~ — **fixed during this review**: expression/
   type nesting is capped (MAX_DEPTH 200, diagnostic E0201); 5000-deep
   input verified to error cleanly instead of overflowing the stack.
3. The JIT trusts the bytecode verifier; both are in-tree and fuzzed
   together, but the verifier has no independent second implementation.
