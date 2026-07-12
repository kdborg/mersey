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
| Guard pages (stack) | ✅ | JIT codegen emits inline stack probes (`enable_probestack`), so a large frame touches each page in order and cannot step *over* a guard page into memory beyond it |
| CFI-compatible codegen | ✅ aarch64 / ❌ x86-64 | aarch64: pointer authentication of return addresses (backward edge, anti-ROP) and BTI landing pads (forward edge). Both are ARM hint-space instructions, so they are NOPs on hardware without them and cost nothing there. x86-64: Cranelift exposes no CET/`endbr64` setting (checked against 0.116 and 0.123) — **not in place**, and not a version we can upgrade our way out of. `mersey_jit::hardening()` reports it as a row that is *off* rather than leaving it out, and `KNOWN_GAPS` records why: a gap that nothing mentions is indistinguishable from a gap nobody noticed. `jit_codegen_is_hardened` fails if anything is off that is *not* in that list |
| Pointer compression + heap-base randomization | ❌ by construction | See below |

### Pointer compression: what it would take, and why it is not here

Pointer compression means the engine owns its heap: one contiguous reservation,
objects addressed as 32-bit offsets from a randomized base, so a corrupted or
forged pointer cannot name memory outside the cage. That requires a custom
allocator over a mapped region, raw pointers, and unsafe code. This engine sets
`unsafe_code = "forbid"` at the workspace root and allocates through `Rc` on the
system allocator. **The two are mutually exclusive**, and the trade was made
deliberately: `forbid(unsafe_code)` removes the class of bug that pointer
compression exists to *contain*.

It is worth being precise about what is lost, because "we forbid unsafe" is not
an answer to a hardening requirement:

* Pointer compression's security value is *containment of memory corruption* —
  it narrows what an attacker who already has a write primitive can reach. In
  this engine, a write primitive would have to come from a bug in safe Rust, in
  `unsafe` inside a dependency, or in the JIT's generated code. The first is what
  the language rules out; the third is the real exposure, and it is why the JIT
  is restricted to a verified non-faulting subset and its output is fuzzed
  differentially against the interpreter.
* Its *performance* value (halved pointer width, better cache density) is simply
  not collected here.
* Mersey the language has no pointers, no pointer arithmetic, and no way to
  observe or fabricate an address, so nothing in a Mersey program can name a
  heap address to begin with — the forging step that compression contains has no
  entry point from the language.

If the native-GC track (Stage B) replaces the `Rc` heap with an owned,
precisely-collected heap, that heap can and should be a randomized cage with
guard pages and compressed pointers, and `unsafe_code` would then be allowed in
that crate alone. Until that heap exists, this row is honestly **not done**
rather than approximated.

### Engine aborts reachable from ordinary Mersey code (found and fixed, 2026-07-12)

A stack overflow is not an exception a program can catch: it is `SIGABRT`. In a
renderer, an abort reachable from a web page is a crash on hostile input, so
these were §5.2 failures even though none of them is a memory-safety bug.

| Case | Was | Now |
|---|---|---|
| Runaway recursion (`function f() { return f(); }`) | Overflowed the Rust stack, aborted the process | `MAX_CALL_DEPTH` (3,000): a catchable `RangeError` |
| The stack trace of a runaway recursion | 3,000 identical frames, a 363 KB error message — a denial of service in its own right | Truncated to head + tail with an elided count |
| Dropping a long chain (500k links, built with an *ordinary loop*) | `Rc` frees a linked structure by recursion; 500k links = 500k Rust frames, aborted | `Drop for GcCell` moves children onto a queue; the outermost drop drains it in a loop. 3M links verified |
| Marking a deep object graph | The collector recursed over user data and aborted | The marker is a worklist; scopes, promises and values are all deferred, never recursed |

The call-depth budget alone would not have caught the last two: those graphs are
built with a loop, not with recursion. Regression tests run the engine in a
subprocess and fail on exit code 134 (`tests/hardening.rs`).

### Fuzzing (the wall-clock criterion)

The roadmap asks for "30 days of continuous fuzzing", which is a statement about
calendar time, not a build step. What is actually in the repository:

* `mersey-fuzz` — mutation fuzzing over the conformance corpus, plus *differential*
  fuzzing (generated well-typed programs run on both engines and compared). It is
  deterministic: a failure prints the seed and reproduces exactly.
* `scripts/fuzz-soak.sh <minutes>` — batched long runs, each batch on its own seed,
  printing the command to reproduce a failing batch.
* CI (`.github/workflows/ci.yml`) runs a 20k-iteration fuzz on every push, seeded by
  the run number so coverage accumulates across pushes rather than repeating; the
  whole suite under `MERSEY_GC_VERIFY=1`; the browser suite in headless Chromium; and
  a stale-golden check, because a conformance diff is a behaviour change rather than
  a formatting one. The soak job runs on a schedule.

The harness and the gate are here. The 30 days are calendar time on someone's CI, and
this document should not pretend otherwise.

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
