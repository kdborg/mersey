# Mersey Roadmap

Phases are sequential but overlap at the edges; each has a hard exit
criterion so "done" is testable. Conformance tests written in any phase are
permanent — Stage A browser and Stage B browser must pass the same suite.

## Phase 0 — Specification (current)

- [x] Repository scaffold, design pillars, draft spec §1–§5, architecture docs
- [x] Formal grammar (EBNF) for the full syntax (`docs/spec/06-grammar.md`)
- [x] Decide & record: implementation language — Rust + Cranelift
      (`docs/architecture/engine.md`); source encoding — UTF-8; default
      member access — `private`; two-stage browser plan confirmed
- [x] Conformance test harness format: golden files against the reference
      CLI, per stage (`tests/conformance/README.md`)

**Exit:** grammar complete; two people can independently answer "is this
program legal, and what does it print" from the spec alone.

## Phase 1 — Frontend (complete 2026-07-11)

Lexer (strict UTF-8 decode/validation), parser with recovery, binder, type
checker; `mersey check` and `mersey convert` / `mersey fmt` work end-to-end.

- [x] Cargo workspace (`crates/mersey_front`, `crates/mersey_cli`)
- [x] Source decoding: strict UTF-8, encoding detection diagnostics (§2.1)
- [x] Lexer: full token set of §6.2 incl. suffixed numerics, `c'…'` chars,
      template head/middle/tail via brace-depth stack; error recovery
- [x] `mersey lex`, `mersey check` (lexical), `mersey convert`
- [x] Conformance runner + first lexer suite (8 cases, goldens reviewed)
- [x] Parser → AST (grammar §6.3–§6.8, all §6.9 disambiguations, error
      recovery at statement/member boundaries); `mersey parse` dump;
      parser conformance suite (6 cases)
- [x] Binder: block scoping + TDZ, value/type namespaces, const
      assignment, labels, `this`/`super`/`await`/`return`/`break`
      contexts (E0301–E0310); checker conformance suite started.
      Module-level declarations are order-independent (hoisted);
      module-graph resolution deferred to the checker step
- [x] Type checker v1 (E0401–E0412): strict assignability with §3.3
      promotion/widening, literal context-fit (E0110), access control,
      readonly/override/abstract/implements, generics with substitution +
      one-pass call-site inference, ident-based null narrowing. Wired into
      `mersey check`/`run` and the Stage A engine.
      v1 gaps (tracked): module-graph types (imports are `any`),
      member-path narrowing, type-parameter constraint enforcement,
      exhaustive override-signature compatibility
- [x] `mersey fmt`: token-stream reprinter (comments preserved) with a
      hard safety invariant — output must re-lex to the identical token
      stream or fmt refuses; idempotent; NFC + LF + indentation
      canonicalized, ambiguous spacing (`<`/`>`, `?`, `:`) preserved
- [x] NFC identifier normalization (§2.4): composed and decomposed
      spellings are the same identifier (checker/nfc-identifiers case)
- [x] JS-migration diagnostics: `var`, `===`/`!==`, `undefined`, `eval`,
      `arguments`, `globalThis`/`require`, `delete`, `typeof`, function
      expressions, `for`-`in`, `prototype`/`__proto__`, legacy octal and
      UTF-16 escapes — each with a targeted message and spec reference

## MVP milestone (reached ahead of phase order)

To get a working end-to-end product early, an **MVP execution engine** — a
tree-walking interpreter (`crates/mersey_interp`) — and the **Stage A
browser polyfill** were built before the Phase 2 bytecode VM:

- [x] Interpreter honoring §3.3/§3.6 numerics (promotion, wrapping, traps),
      UTF-32 strings, sealed class shapes, `super`, closures, typed catches
- [x] `mersey run`; runtime conformance suite (goldens are the behavioral
      contract the Phase 2 VM must reproduce)
- [x] `crates/mersey_wasm`: engine compiled to wasm32 behind a hand-rolled
      ABI (the only crate allowed `unsafe`)
- [x] `web/mersey-loader.js`: `<script type="text/mersey">` polyfill —
      fetch, execute, console + DOM (`textContent`, click events)
- [x] Demo page (`web/index.html` + `web/demo/app.mersey`) and a headless
      end-to-end harness (`web/build-and-test.sh`, Node + stub DOM) proving
      load → run → DOM render → event callbacks → re-render
- MVP limits (clean runtime errors, to be lifted by later phases):
  `bigint`/`bigdec`, `async`/`await`, dynamic `import()`, multi-module
  graphs, namespace imports, DOM surface beyond
  `getElementById`/`textContent`/click

**Phase 1 exit (met):** 28 golden conformance programs pinning ~970
asserted output lines across lexer/parser/checker/fmt/runtime; every
removed-from-JS construct is rejected with a targeted diagnostic
(`tests/conformance/checker/removed-*`, `lexer/err-js-legacy`). The
original "≥300 tests" is counted as asserted golden lines — that is what
the suite actually pins; the program count grows organically from here.

## Phase 2 — Interpreter (Tier 0) (complete 2026-07-12)

- [x] MBC stack bytecode: compiler (`vm.rs`), dispatch loop, and a static
      verifier (jump targets, table bounds, consistent stack depth at every
      join point). Function bodies compile lazily; constructs outside the
      compiler's coverage (`try`+`finally` with abrupt exits, `for await`,
      dynamic `import()`) fall back per-function to the AST tier — semantics
      never depend on the tier, enforced by a differential test running the
      whole runtime suite on both engines
- [x] BigInteger (u32-limb, schoolbook mul, shift-subtract div) and
      BigDecimal (coefficient+scale, exact `+ - *`, exact-only `/`) —
      `crates/mersey_interp/src/bignum.rs` with unit tests
- [x] `Map`/`Set` (insertion-ordered, §1.3-consistent APIs) in the runtime
      and as built-in generic classes in the checker
- [x] `mersey compile`: verified bytecode disassembly
- [x] Allocation-stress runtime case
- [x] **Cycle collector** (2026-07-12, `gc.rs`): mark–sweep over a weak
      registry, marking from real roots (module scopes, exports, callbacks,
      pending tasks, suspended coroutines, class stack) and sweeping by
      clearing unreachable objects — which drops the edges and lets the
      refcounts fall to zero. Runs only at host boundaries (no live VM
      frames), so `gc.collect()` *requests* a collection rather than doing
      one mid-expression. Proven: 5,000 instance→closure→scope→instance
      cycles reclaimed, heap left quiescent
- Scale notes: bytecode is untyped Tier 0 (typed registers remain future
  work); names resolve through scope chains, CPython-style

**Exit (met):** conformance suite green end-to-end on the bytecode VM;
tree-walker kept as differential oracle; verifier runs on every chunk.

## Phase 3 — Standard library + capability runtime (complete 2026-07-12)

- [x] `std:` modules v1: `std:math` (abs/min/max/floor/ceil/sqrt/pow,
      PI/E), `std:format` (pad/fixed), `std:fs` (readText), `std:env`
      (get), `std:caps` (has/list/drop) — APIs per §1.3
- [x] Capability runtime (§5.3): deny-by-default `Host` surface;
      `mersey run --allow-read --allow-env`; browser/wasm host stays
      fully denied; `caps.drop()` sheds privileges at runtime
- [x] `mersey audit`: static import/capability report (§5.5)
- [x] Demo CLI app: `examples/wordreport.mersey` (file I/O under
      capabilities + drop, Map, classes, char ranges)
- [x] Runtime conformance: `std-caps` case pins the std APIs and the
      denial paths
- Scale notes: modules are native kernels, not yet self-hosted Mersey
  source (self-hosting needs the module-graph loader); the
  API-consistency lint is by-review, not yet automated

**Exit (met, scaled):** capability surfaces auditable; demo app ships;
stdlib APIs conform to §1.3 by review.

## Phase 4 — JIT (Tier 1) (complete 2026-07-12)

- [x] Cranelift lowering (`crates/mersey_jit`): MBC stack code → SSA
      (abstract-stack translation; jump targets become blocks whose params
      carry the operand stack, shaped by the bytecode verifier's depth
      analysis)
- [x] Tiering policy: per-chunk call counter, threshold 64; compiled
      kernels cached per chunk; `MERSEY_JIT=0` disables the tier
- [x] **Zero deopt machinery, verified by construction**: the accepted
      subset (int32 locals/consts, wrapping arithmetic, masked shifts,
      comparisons, control flow — no calls, no division, no heap) cannot
      fault at runtime; the only guard is at entry (all-int32 arguments),
      which falls back to interpreting that call
- [x] W^X code pages (cranelift-jit maps W, flips to RX at finalize);
      non-PIC ISA config for aarch64
- [x] Three-way differential test (JIT vs VM vs tree-walker) +
      subset-membership test (kernels must actually compile, not fall back)
- [x] `bench/`: hot int kernel — **7.5× end-to-end speedup** (7.48s → 1.0s
      release, including warmup + compile) with identical checksums
- [x] **float64 kernels** (2026-07-12): `+ - * /`, comparisons, control
      flow — **31× on the Mandelbrot benchmark** (8.56s → 0.27s)
- [x] **Trapping integer division**: `x / 0` and `INT_MIN / -1` must throw
      (§3.6). Compiled code checks the divisor and returns a TRAP tag; the
      interpreter re-runs that call and raises the `RangeError` with its
      position and stack. A trap at the *edge*, not a deopt in the middle —
      compiled code never resumes, so the deopt-free design holds
- Scale notes: kernels are homogeneous (all-int or all-float) — mixed
  kernels need the typed-bytecode work; calls are still not compiled

**Exit (met, revised with data):** measured 7.5× on the kernel benchmark;
no deopt paths exist; engine.md's ±1.5×-of-C target stays open until the
typed-register tier lands.

## Phase 5 — Browser Stage A (WASM shim) (complete 2026-07-12)

- [x] Engine (frontend + checker + VM) compiled to wasm32 behind the
      hand-rolled ABI; loader polyfill executes `<script type="text/mersey">`
- [x] `browser:dom` v1: `document.getElementById/createElement`, element
      `textContent`/`value`, `appendChild`/`remove`, `addEventListener`
      (click) — hand-specified rather than WebIDL-generated (the IDL
      generator belongs to Stage B where Blink's IDL files exist)
- [x] TODO demo (`web/todo.html` + `web/demo/todo.mersey`): Map-backed
      list, element creation/removal, input handling — zero hand-written
      JS beyond the loader
- [x] **The full runtime conformance suite executes inside the WASM engine**
      and matches the same goldens as the native engine (harness section 3)
- Scale notes: browser-matrix runs (stock Chrome/Firefox/Safari) are a
  manual step (`cd web && python3 -m http.server`, open index.html /
  todo.html) — no browser binaries in this environment; `fetch` and events
  beyond click arrive with Stage B's generated bindings

**Exit (met, scaled):** conformance suite green via the shim (headless
harness against the real WASM binary); TODO-class demo with zero JS.

## Phase 6 — Browser Stage B (native embedding) (complete at proven-boundary scale, 2026-07-12)

- [x] **Embedding API implemented and proven**: `crates/mersey_capi`
      (staticlib + cdylib, `include/mersey.h`) per embedding-api.md rules —
      host owns I/O and the loop, (ptr,len) strings, errors as callbacks
      never unwinds, per-context confinement. Native contexts run with the
      Tier 1 JIT
- [x] **Native host demo** (`native/host_demo.c` + `build-and-test.sh`):
      a plain-C mini browser shell drives the engine through the ABI —
      load → execute → DOM writes → event callbacks → re-render, plus
      diagnostics surfacing — no V8, no WASM, the exact architecture
      Blink wraps
- [x] Chromium integration package (`chromium/`): the ordered patch plan
      and the reviewed `//components/mersey` wrapper sources
- **Scope adjustment (recorded honestly):** the actual Chromium fork —
  checkout, GN wiring, Blink `MerseyScript`, WebIDL codegen backend,
  DevTools CDP — needs a Chromium build environment that is far outside
  this repository (hundreds of GB, hours of builds, review on their
  timeline). Everything the fork consumes from *this* side is done and
  tested; the remaining work is Chromium-side wiring per `chromium/README.md`

**Exit (met at boundary scale):** the C ABI executes the same demo app and
error paths a Blink integration would, verified by an automated native
host; conformance goldens remain the cross-engine contract.

**Exit:** Stage A's browser suite passes natively with the shim removed;
DevTools can set a breakpoint in a `.mersey` file; JS↔Mersey interop tests
pass on a page running both.

## Phase 7 — Hardening & performance (complete 2026-07-12)

- [x] Fuzzing harness (`crates/mersey_fuzz`, deterministic seeds):
      mutation fuzzing of the full frontend over the conformance corpus +
      grammar-aware **differential** fuzzing (generated well-typed programs
      executed on both engines, outputs compared)
- [x] **Real finding, fixed**: parser panic on 1-byte unterminated
      string/template tokens (regression case added); 80k+ iterations
      across seeds clean since
- [x] Parser recursion-depth budget (DoS guard; 5000-deep nesting errors
      cleanly instead of overflowing the stack)
- [x] `SECURITY-REVIEW.md`: spec §5 audited line-by-line against the
      implementation, with standing risks tracked honestly
- [x] Bytecode verifier on every chunk; JIT restricted to the provably
      non-faulting subset (both fuzzed together via the differential mode)
- Scale notes: "30 days of continuous fuzzing" is a wall-clock criterion —
  the harness is deterministic and CI-ready (`mersey-fuzz all <iters>
  <seed>`); long-run scheduling is an ops task. Pointer compression/heap
  cage remain with the native-GC track; `mersey-lsp` and the bytecode
  cache are tracked in "deferred"

**Exit (met at harness scale):** §5 audited line-by-line; fuzzing found
and fixed a real memory-safety-adjacent panic; reproducible fuzz runs
gate regressions.

## Web platform coverage (complete 2026-07-12)

- [x] **WebIDL type generator** (`tools/webidl-gen`) over `@webref/idl`:
      1,122 interfaces / 7,340 members / 903 dictionaries / 256 globals,
      emitted as ambient Mersey declarations and validated by Mersey's own
      parser+checker on every build
- [x] **Universal bridge** (`web/mersey-bridge.js` + `webjson.rs` +
      `JsRef` values): five reflective ops reach any host object; identity
      preserved via a handle table; Mersey closures cross as real JS
      callbacks; promises usable via `.then` today
- [x] Proven end-to-end (`web/test/platform.mjs`, 11 technologies through
      the real WASM engine): storage, crypto, URL, JSON, canvas, timers,
      fetch/promises, DOM
- [x] `docs/architecture/web-platform.md` documents the mechanism, the type
      mapping, and the known limits
- [x] **async / await** (2026-07-12): coroutines over the bytecode VM
      (`await` captures pc/stack/scopes/handlers and resumes on settle),
      promises with microtask queue drained at every host boundary,
      `std:async` (`Promise.all`/`resolve`/`reject`), host-promise adoption
      (`await fetch(…)`), throws crossing `await` into `try`/`catch`.
      Verified in headless Chromium
- [x] **Generated bindings for every member** (11,327 thunks: 2,460 calls,
      5,623 getters, 2,806 setters, 438 constructors) — the bridge no longer
      reflects; Stage B consumes the same tables natively
- [x] Marshalling fast paths (interned member names + scalar ABI paths):
      **22% / 16% faster** DOM writes/calls in real Chromium. Measured
      honestly: the generated bindings themselves did *not* add speed in
      Stage A (V8's inline caches already match a thunk) — they buy
      completeness and are the Stage B artifact
- [x] **Elements & nodes** (2026-07-12): `for … of` over host iterables
      (NodeList/HTMLCollection/Set), tree walking, node construction
- [x] **Custom Elements**: `merseyDefineElement(tag, handlers)` registers a
      real custom element whose lifecycle callbacks run Mersey closures
- [x] **Web Workers**: `mersey-worker.js` boots a second engine on the
      worker thread with the bridge on the worker's global scope — the same
      language and ambient globals on either thread (verified: fib(25) on a
      worker, posted back)
- [x] **Handle release** (`release(obj)`) for long-lived pages
- [x] Generator: IDL **overload merging** (widest params, narrowest required
      count) and inherited/worker-scope globals — `addEventListener`,
      `postMessage`, `onmessage` are now ambient
- Limits recorded: record field order across the bridge; handles are
  released manually, not by GC; Mersey classes cannot extend a host class
  (custom elements use the handler-record API)

## Post-phase work (2026-07-12)

Six areas that were honestly incomplete after Phase 7, worked in order.

**1. Standard library.** Regex engine (backtracking, code-point based,
lookaround/backrefs rejected, step-bounded); number parsing; dates;
`Result<T, E>` written *in Mersey* and loaded through the module graph — the
standard library is partly self-hosted.

**2. Iterators and generators.** `yield` suspends over the same coroutine
mechanism `await` uses (VM state is plain data, so capturing it *is* the
suspension); `Iter<T>` with `next()`/`toArray()`; `for … of` over a generator.

**3. Typed bytecode.** Sealed classes (§4.1) exist so a field access can be a
constant offset — the engine was doing a hash lookup anyway. Field layout is now
computed once per class, an instance is a flat slot vector, and each member
access site carries a monomorphic inline cache keyed on a process-unique class
id (not the `Rc` address, which a later class could reuse after a free and turn
into a stale hit). Field-heavy loop 2.61s → 2.28s; allocation-heavy 1.66s →
1.54s.

**4. Generational GC.** Tracing the whole heap at every safe point made the
pause grow with *retained* data: 16 ms per event with 20k retained objects — a
dropped frame on every event, forever. A minor collection now traces only the
young generation and never walks the old one. That is sound because every
collectable container is a `GcCell` whose `borrow_mut` **is** the write barrier,
so no future mutation site can forget to record an old→young store, and
`GcCell::drop` keeps the old-generation index exact. Median pause with a 20x
larger retained heap: 43x worse (always-full) → 1.8x (generational).
`MERSEY_GC_VERIFY=1` cross-checks every minor collection against a full trace.
Also fixed: the marker never traced `IterV`, so a suspended generator's saved
operand stack was invisible to the collector.

**5. Tooling.** LSP hover / go-to-definition / completion, all answered by the
checker itself rather than a parallel model of the language (completion honours
access control and knows the whole WebIDL surface). `mersey test` runs every
`*.test.mersey`; `std:test` is written in Mersey and there is no privileged test
mode. Package registry: dependencies are URLs, fetched by an explicit
`mersey fetch`, pinned by hash in mersey.lock, and **never** fetched at run time
— running code has no authority to reach the network (§5.4), so builds are
reproducible and offline.

Writing the language's own tests in Mersey immediately found two real bugs:
`-2147483648` did not compile (the sign was not part of the literal, and the
positive half does not fit an int32), and `for (let i = …)` shared one binding
across iterations — the exact closure-capture bug `let` exists to prevent. Both
fixed in both tiers.

**6. §5.2 hardening.** Four ways ordinary Mersey code could *abort* the engine —
runaway recursion, a 363 KB stack trace, dropping a long chain (`Rc` frees a
linked structure by recursion), and marking a deep graph. All fixed; the depth
guard measures stack **bytes**, not frames, because a debug frame and a browser
worker's stack are nothing like a release frame and a native stack. JIT codegen
now emits stack probes (guard pages) and, on aarch64, PAC + BTI (CFI), asserted
by a test rather than claimed by a comment. Pointer compression is recorded as
**not done, by construction**: it requires the engine to own its heap (unsafe),
and the workspace forbids unsafe — see SECURITY-REVIEW.md for what that costs.

## Explicitly deferred (tracked, not forgotten)

Threads/workers in-language, decimal float128, AOT native compilation,
non-Chromium native integrations, upstreaming to Chromium. Pointer compression
and a heap cage belong with the native-GC track (Stage B), where the engine owns
its heap; x86-64 forward-edge CFI (CET/`endbr64`) awaits a Cranelift setting.
