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
      refcounts fall to zero. Proven: 5,000 instance→closure→scope→instance
      cycles reclaimed, heap left quiescent
- [x] **Reference-counting cycle collection** (2026-07-12): the tracing
      collector above needs a root set, and it could only get one at a host
      boundary — the interpreter cannot enumerate the live values in its own
      Rust locals. So a program that stayed inside one loop never collected
      *at all*. Refcounting freed what was not cyclic, but every `for` body
      that makes a closure builds a cycle (the scope holds the closure, the
      closure captured the scope), and those accumulated: 2M iterations held
      **1.7 GB**, and a long enough loop was killed by the OOM killer.
      The fix needs no roots. Every object is behind an `Rc`, so a reference
      from a Rust local is *already counted*; liveness is derived from the
      counts instead — `external(X) = strong_count(X) − references held by
      other heap objects`, and anything with `external > 0` is held by
      something the heap cannot see, so it is live. Trace from those; what is
      left is a cycle nobody can reach. Being root-free is what makes it legal
      to run **inside a loop**, at the back edge. Same program: **22 MB, flat
      at any loop length**
- [x] The young list itself was a second, quieter leak: every allocation left
      a `Weak` behind and a `Weak` pins its allocation, so the *bookkeeping*
      grew forever even when the objects did not. Dead entries are now pruned,
      which needs no roots either (a zero strong count is proof). A 3M-iteration
      loop over two integers: **389 MB → 18 MB**
- [x] Soundness, cross-checked: the two collectors decide liveness by
      completely different means, so every object the tracer reaches from the
      real roots must be one the reference-count analysis calls live —
      asserted over every conformance program (`verify_cycles`). Overcounting
      one internal edge would sweep a live object, so this is the property that
      matters
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
- [x] **Loop back-edge tiering + OSR** (2026-07-12): a call counter never
      sees a loop inside a function that is called once — its count reaches 1
      and stops — so the VM counts back edges too (threshold 5000) and
      *re-enters the function at the loop header*, carrying the live locals.
      A 200M-iteration loop in a once-called function: **killed by the OOM
      killer after 88s interpreted → 0.28s compiled**, matching V8's 0.29s
- [x] **Calls, including recursion and mutual recursion** (2026-07-12): the
      interpreter hands the JIT the root *and every global function reachable
      from it*, declared together in one module so they call each other
      directly. `fib(32)`: **4.69s → 0.05s (94×)**, against V8's 0.03s.
      Compiling one function at a time would have been pointless — the two
      transitions in and out of the interpreter cost more than the call
- [x] Recursion is bounded by compiled code's own depth counter, not by the
      hardware: it hands the call back at `MAX_CALL_DEPTH` and the interpreter
      raises the `RangeError` with a stack trace, rather than running the
      native stack into its guard page
- [x] Compiled code is invalidated if a global function it calls is
      *reassigned* (`f = g`): a function declaration is an ordinary binding,
      and direct calls are only correct while it still means what it meant
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
- Scale notes: kernels are still homogeneous (all-int or all-float) — mixed
  kernels need the typed-bytecode work — and still scalar: no arrays, strings,
  objects or field access, so a hot loop that touches the heap stays in Tier 0

**Exit (met, revised with data):** 7.5× on the original kernel benchmark;
94× on call-heavy code, which the subset could not touch at all before;
no deopt paths exist. Within 1.4–3× of V8 on the scalar benchmarks;
engine.md's ±1.5×-of-C target stays open until the typed-register tier lands.

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

## Typed bytecode (2026-07-12)

- [x] **Numeric conversions are in the bytecode** (`Op::Convert`). The checker
      accepted C-style widening (§3.3) and typed the expression accordingly; the
      engine then erased the type, stored whatever the value happened to be, and
      dispatched arithmetic on *that*. `let x: float64 = 7; x / 2` was **3**. An
      `int64` wrapped at **32 bits**. A `uint32` literal that did not fit an
      int32 was a runtime range error for a value that fits the type it was
      given. Every tier agreed, which is why nothing caught it — they were all
      wrong the same way
- [x] Conversions at every binding site: parameters, `let`/`const`, assignment,
      `return`, array elements, record fields (including the `{ x }` shorthand),
      class fields, and the operands of every arithmetic operator
- [x] Literals are **built** at their declared type, not converted into it —
      `let b: uint32 = 4294967295` has no int32 to convert *from*
- [x] §3.3 rule 6 (compound assignment converts its result back, with wrapping)
      — `int16 a; a += 1` held 32768 before, a number an int16 cannot represent
- [x] The conversions live in the **checker**, keyed by AST node, and checking a
      program is what makes them available. An engine cannot forget to install
      them, because there is nothing to install — and forgetting would mean
      silently wrong arithmetic, which is the bug this removes
- [x] `check` takes `&'static Module`, which makes the one hazard of keying by
      node address unrepresentable: an AST that is checked and then *freed*
      would leave entries describing addresses the allocator can hand to the
      next program. **The differential fuzzer caught exactly that**, in the
      fuzzer's own harness — it checked one AST and ran a copy
- [x] JIT: a float64 kernel written with ordinary integer literals (`2 * x * y`,
      `> 4`) now compiles. Those were int32 constants in a float function, so the
      kernel was rejected as mixed and the whole thing interpreted
- Scale notes: kernels are still **homogeneous**. A float computation with an
  `int32` loop counter is still mixed, and still interpreted — per-value types in
  the JIT (typed registers) are the next step, and the bytecode now carries what
  they need

## Slot-resolved locals (2026-07-12)

- [x] **Locals are frame slots** (`Op::LoadSlot`/`StoreSlot`). Every local access
      used to be a `String` hashed into a `HashMap`, and then the same again in
      the parent scope, and the one above that. A loop touching three locals paid
      five hash lookups an iteration — and that, not the arithmetic, was most of
      what Tier 0 cost. The compiler already knew where every local lived; now the
      bytecode says so
- [x] The line that matters: **a local a closure can see stays in the
      environment**, because the closure outlives the frame. Capture is analysed
      per function, by name (conservative: a closure with its own `x` also pins an
      outer `x`, which costs a hash lookup, never a wrong answer)
- [x] Module-level bindings are *globals*, not locals — a function reaches them by
      name through the global scope and an export hands them to another module by
      name. Neither can see a slot, so they stay where they are looked for
- [x] **Scopes are only allocated when something lives in one.** A loop body used
      to build a `Scope` — a `GcCell` and a `HashMap` — every iteration whether or
      not it held anything
- [x] Compiler temps (the desugaring of `a.b += c`, `for…of`, `switch`) are slots
      by construction: they have no name in the source, so nothing can capture them
- [x] The JIT got *simpler*: the locals are already slots, and they are the **same
      slots the interpreter uses**, so on-stack replacement is a transfer of the
      frame rather than a search for each local by name

**Measured (Tier 0, interpreter):**

| workload | before | after | |
|---|---|---|---|
| array-heavy loop | 27.1s | **2.4s** | 11× |
| float loop, int counter | 27.9s | 15.3s | 1.8× |
| `bench/hotloop` | 7.0s | 4.2s | 1.7× |
| Mandelbrot | 9.0s | 5.0s | 1.8× |

Against V8 the array-heavy loop went from 301× slower to **27×**. Tier 1 got
faster too (hotloop 0.34s → 0.22s), the frame being what OSR now hands over.

- Scale notes: what is left in Tier 0 is the **boxed `Value` enum** and the generic
  operator dispatch that goes with it — removing the per-op position write saves
  2%, so the remaining cost is the values themselves. Typed registers (unboxed
  slots with a static type per slot) are the next step, and they are also what
  mixed int/float kernels and unboxed `int32[]` need

## Call overhead (2026-07-12)

A call cost **313ns**, and that — not the arithmetic — was what made
object-oriented code 237× slower than V8. A single call allocated five times.

- [x] **A body with nothing in the environment needs no environment.** No
      `Scope`, no `Rc`, no `GcCell`, no `HashMap`, and nothing handed to the
      collector — and the arguments move straight into the frame slots the
      compiler gave them, instead of being inserted into a map by name and then
      hashed back *out* to fill the frame. Names that are not locals still
      resolve: the chain the call runs against is the closure's own environment,
      whose root is the globals
- [x] **`this` is a frame slot**, not a name in an environment. Putting it in the
      environment is what forced every *method* to allocate one. `super` reads it
      from the frame too — and a constructor whose only use of `this` is its
      `super` call still has to reserve the slot, which is the one case that
      caught me
- [x] The JIT's call counter lives on the chunk (a `Cell`), not in a map keyed by
      its address: hashing a pointer on every call, forever, to decide whether to
      compile something once is a strange thing to pay for
- [x] Stack-trace frames hold `Rc<str>`, not two freshly allocated `String`s per
      call
- [x] **Literal field initializers are folded at class definition.** `new` used to
      clone the entire field list, and then allocate an environment *per
      initialized field* to hold the one binding they all wanted — `this` — which
      a literal does not even read

**Measured (Tier 0, per operation):**

| | before | after |
|---|---|---|
| function call | 313ns | **98ns** |
| `new` + constructor | 770ns | **305ns** |
| field access | 37ns | 37ns (already fine) |

| workload | before | after | vs node |
|---|---|---|---|
| class/method heavy | 11.8s | **5.3s** | 105× (was 237×) |

- Tried and rejected: **pooling the `Vec<Value>`s** (operand stack, frame,
  argument list). It was *slower* — 146ns/call against 98ns. The allocator wins;
  the pool only added branches and a drop loop. Recorded because the next person
  to look at the two remaining allocations will have the same idea

## Allocation, interface accessors, and the JsAny question (2026-07-12)

- [x] **`new` no longer looks its class up by name.** It scanned the name for a
      `.`, hashed it, and walked the scope chain — on *every* allocation. It can
      never resolve to a different class (a class declaration is not a variable
      and cannot be reassigned, E0304), so it is resolved once and cached on the
      chunk. Object allocation: **118ns → 91ns**
- [x] **Interfaces can require a getter** (`interface Sized { get size(): int32 }`).
      Classes had `get`/`set` from the start; interfaces could only require a
      field or a method, so a computed property could not be part of a contract.
      From the caller's side a getter *is* a readonly property, so that is what
      the interface asks for — and a class satisfies it with an accessor or with a
      plain readonly field, whichever it has
- [x] Found while doing it: **`readonly` was not enforced through an interface at
      all**. The check only looked at class receivers, so an interface was a way
      around the class's own rule. It applies to every `readonly` interface
      property, not just the new getters
- [x] The stale `generate.mjs` comment ("checker treats as `any`") is gone

### The 615 `JsAny`, and why they stay

`JsAny` becomes `unknown` — a *sound* top type: nothing comes out of it without a
narrowing or a checked cast. It is an ergonomics gap, not a hole.

Mapping `record<K,V>` looked like the cheap win and **is not one**. The bridge
encodes every JS object as an opaque handle (`{"__ref__": n}`), not as a record
or a map — so typing it as `Map<string, V>` would be a lie about what the value
*is*, and the cast that a lie forces is exactly what `unknown` already makes you
write. Typing it honestly needs a host-backed dictionary view, which is a feature,
not a mapping fix.

### Allocation has a floor

91ns per object, against V8's ~5ns. What is left is the `malloc` for the object
itself and its registration with the collector. Going materially below it needs
bump allocation and a moving collector — a different heap, which the
reference-counting cycle collector (and the workspace's ban on `unsafe`) rules
out. Recorded so the next person does not re-derive it.

## Typed registers (2026-07-12)

Every value has a type, and the bytecode says what it is.

- [x] **Typed operators** (`Op::BinNum(op, ty)`). An `int32 + int32` used to walk
      a string check, a bigint check, a promotion into a common type, and *then* a
      dispatch — four matches to add two numbers whose types were known at compile
      time. The checker knew; now it says so
- [x] **Typed slots** (`Chunk.slot_types`). A frame slot with a declared type is a
      **register**
- [x] **The JIT is no longer homogeneous.** A kernel used to be all-`int32` or
      all-`float64`, because the engine could not tell one from the other without
      looking at the values — and a compiler cannot look at the values. So the most
      ordinary numeric loop there is,
      `for (let i = 0; i < n; i++) { acc = acc + 1.0 / (1.0 + acc); }`,
      was **refused**, on the grounds that its counter is an int and its
      accumulator is a float. Now `int32`, `int64`, `float64` and `bool` mix freely
      in one compiled function, and the conversions between them (§3.3) are
      instructions rather than a reason to give up

**Measured:**

| | before | after | vs node |
|---|---|---|---|
| float loop, int counter | 14.95s | **0.29s** | **1.1×** |
| array-heavy loop | 2.38s | 1.18s | 11.8× |
| class/method heavy | 5.21s | 4.09s | 51× |
| string building | 0.80s | 0.56s | 9.3× |
| `bench/hotloop` (Tier 0) | 7.0s | **1.41s** | |
| Mandelbrot (Tier 0) | 9.0s | **1.38s** | |

The interpreter got 2.5× on its own; the mixed-kernel unlock did the rest. The
float-loop benchmark went from 60× slower than V8 to **parity**.

- Not in the JIT subset, and honest about it: **the heap** (arrays, strings,
  objects, field access) and **`as` casts**. A checked cast traps on overflow, so
  it needs a range check and a trap edge — the machinery exists (division has one),
  it simply is not wired up. The heap needs a typed *heap*, which is the next thing
- Not in a register: `uint32`/`uint64` (the values that cross the boundary are
  I32/I64/F64, and an unsigned kernel would marshal through one of them and be
  wrong at the edges), and `int8`/`int16` (they promote to `int32` in arithmetic,
  but a *conversion* to one has to wrap, which is not a register move)

## Inlined calls, and what the measurements actually said (2026-07-12)

**The typed heap was not the bottleneck.** Before building it I measured, and the
typed-register work had already fixed the heap: a **field read is 17ns** and an
**array index 24ns**. Unboxing arrays and teaching the JIT to load fields would
have optimised the parts that were already fast. What cost was:

| operation | before | after |
|---|---|---|
| **function call** | 100ns | **76ns** |
| object allocation | 82ns | 84ns |
| field read | 17ns | 17ns |
| array index | 24ns | 24ns |

- [x] **A Mersey-to-Mersey call runs inside the interpreter's loop.** It used to
      re-enter `exec` — a two-thousand-line Rust function — with a fresh `Exec`
      and a fresh operand stack, for a callee that needed neither. A call now
      pushes an `InlineFrame` and keeps going round the same loop; the callee's
      locals sit above the caller's in one vector, and so does its operand stack.
      This is what CPython 3.11 did, and for the same reason
- [x] Everything a nested Rust call gave for free is now done by hand, and each
      one is a test: a throw finds the **caller's** handler, `finally` runs while
      unwinding, `super` finds the right `this`, an arrow made inside a call
      captures the right one, and the depth limit counts frames that are no longer
      Rust frames
- [x] **`chunk_yields` was a linear scan of every instruction, on every call** —
      to answer a question whose answer never changes. It is a `bool` on the chunk
      now. (Invisible in a microbenchmark whose callee has three instructions;
      real functions are not three instructions)
- [x] On-stack replacement follows the frame that is *running*. Inlining broke it
      at first: `main` calls `work(n)`, `work` is inlined, and it is **work's**
      loop that is hot — but the OSR context still pointed at `main`. The
      float-loop benchmark quietly fell from 0.29s to 4.46s until the numbers were
      looked at

**Measured:**

| workload | before | after | vs node |
|---|---|---|---|
| float loop, int counter | 14.95s | **0.26s** | **1.0×** |
| array-heavy loop | 2.38s | **1.14s** | 12.7× |
| string building | 0.80s | 0.53s | 10.6× |
| class/method heavy | 5.21s | 3.92s | 78× |

- **Allocation is what is left**, and it is what keeps object-heavy code at 78×:
  84ns against V8's ~5ns. Every object is an `Rc<GcCell<Instance>>` — a malloc —
  plus a `Vec` for its slots, plus a registration with the collector. Going below
  it needs bump allocation and a moving collector: a different heap, which the
  reference-counting cycle collector and the ban on `unsafe` both rule out. That
  is the honest ceiling of this design, and the next real decision

## The heap decision, and method dispatch (2026-07-13)

### Decision: keep the safe `Rc` heap

Allocation is 63ns against V8's ~5ns, and closing that needs bump allocation and
a **moving** collector. We are not doing it.

1. **It would cost the security pillar.** The workspace forbids `unsafe`, and this
   is an engine with a JIT that is meant to run untrusted web content. A moving
   GC is where engines get their memory-safety CVEs. Trading that for 50ns an
   allocation is the trade V8 made, and its CVE history is the receipt.
2. **It would break the cycle collector.** The reference-counting collector — the
   one that can run *inside a loop* without a root set, which is what fixed the
   OOM bug — depends on `Rc`. A moving GC brings back the bug it fixed.
3. **It would not close the gap anyway.** The gap on object code is not
   allocation: it is *interpretation*. Even a free `new` leaves the same
   bytecode dispatch.

### So: make the interpreter faster instead

- [x] **`Value` is 16 bytes, not 24.** One variant — `Native(&'static str)` — held
      a *fat* pointer, and it was the only payload in the enum wider than a word.
      That one variant cost every value in the engine 8 bytes: on every clone,
      every stack push, every frame slot, every field of every object
- [x] **`new` builds its slots from a prebuilt vector.** It used to walk the field
      list *three times* — nulls, then literals, then the initializers that were
      not literals — on every allocation, 20ns a field, to reproduce a result that
      is the same every time
- [x] **Method calls are found once per call site and then inlined.** A method call
      walked the whole of `call_member` — past iterators, promises, arrays,
      strings — and *then* searched the class chain, on every call, and then
      re-entered the interpreter. Sealed shapes (§4.1) mean a class's method set
      never changes, so the site can remember what it found: **169ns → 91ns**

| operation | start of day | now |
|---|---|---|
| method call | 169ns | **91ns** |
| function call | 100ns | **74ns** |
| `new` (no fields) | 85ns | **63ns** |
| `new` (8 fields) | 249ns | **180ns** |

| workload | start of day | now | vs node |
|---|---|---|---|
| class/method heavy | 5.21s | **3.17s** | 63× |
| DOM-shaped tree | 0.37s | **0.31s** | 15.5× |
| array-heavy loop | 2.38s | **1.05s** | 11.7× |
| string building | 0.80s | **0.53s** | 10.6× |
| float loop, int counter | 14.95s | **0.28s** | 1.1× |

### What is actually left

Numeric code is at parity with V8. Everything else is **interpreted**, and that is
the whole of the remaining gap — not allocation, not dispatch, not lookup. The JIT
refuses any function that touches the heap: a field load, an array index, a method
call. Those are 17ns, 24ns and 91ns in the interpreter and would be one or two
instructions compiled.

**The next project is a heap-aware Tier 1**: field access at a constant offset
(sealed shapes make it one, §4.1 promises it), array elements, and calls between
compiled methods. That is what takes object code from 63× to single digits — and
`mersey_jit` already permits `unsafe`, so it can hold a raw pointer to an object
that, because the heap does *not* move, stays valid.

## A heap-aware Tier 1 (2026-07-13)

Built. Tier 1 reaches the heap: **fields at a constant offset, array elements, and
direct method calls.** Object code went from 63× V8 to **2.7×**.

| | Tier 0 | Tier 1 | node |
|---|---|---|---|
| `bench/objects.mersey` (2000 bodies × 5000 steps) | 6.49s | **0.14s** | 0.051s |

Per loop iteration, which is the honest unit — at Tier 1 a single load hides under
the float-add latency and has no separately measurable cost:

| loop body | Tier 0 | Tier 1 |
|---|---|---|
| `t = t + k` (baseline) | 61.6ns | 0.91ns |
| `t = t + p.x` (field read) | 87.3ns | 0.94ns |
| `t = t + xs[i]` (array element) | 84.7ns | 0.93ns |
| `p.x = p.x + 1.0` (field write) | 95.0ns | 3.65ns |
| `t = t + p.get()` (method call) | 153.7ns | 3.69ns |

### What made it possible was the language, not the compiler

Three things, and none of them are engineering cleverness — they are things Mersey
decided years earlier and can now spend.

**Sealed shapes (§4.1) make a field a constant offset — and a base's offsets valid
on a subclass.** A class's layout is fixed at declaration and a subclass's layout
*begins with its base's*, so an offset computed for `Shape` is still the right
offset on a `Circle`. Compiled code needs **no class check at all** to read a
field. A `Shape[]` full of `Circle`s and `Square`s runs the same compiled code.

**Class hierarchy analysis makes a method call a direct call.** The module graph is
closed (§4.5), classes are sealed, and there is no `eval` — so "does anything
override `area`?" *has an answer*. When nothing does, `s.area()` compiles to a
direct jump: no vtable, no inline cache, no guard, and therefore no deopt. When
something does, the function is refused and stays in Tier 0. A JS engine cannot ask
this question, because the answer changes the moment someone assigns to a
prototype. This is the single biggest thing the "no prototypes" decision bought,
and it took until now to collect it.

**The `Rc` heap does not move** — yesterday's decision — so compiled code can hold a
raw pointer to an object and have it stay valid.

### The rule that makes it safe

**Compiled code never touches a reference count.** It reads scalars out of heap
cells and writes scalars back; any object it holds is *borrowed* — never cloned,
never stored, never returned. Everything follows from that one rule: it cannot free
anything, so nothing it holds an address of can go away; it cannot allocate, so the
collector cannot run underneath it; it creates no edges, so the cycle collector's
graph is untouched. **No write barrier, nothing to root, no GC interaction at all.**

The cost of the rule is the boundary of the subset: **allocation**. A function that
does `new`, or builds an array, or makes a string, is interpreted — that is where
ownership of a value native code created would have to be tracked along every path
out of it, and it is the next project.

So this is worth being blunt about: **it does nothing at all for object code that
allocates in its hot loop**, which is a great deal of real object code.

| workload | before | after | vs node |
|---|---|---|---|
| particles: fields, elements, methods, no allocation | 6.49s | **0.14s** | **2.7×** |
| `oop`: allocates a `Vec2` every iteration | 3.37s | 3.37s | 67× |
| `domish`: tree, `for…of`, allocates | 0.35s | 0.35s | 17× |

`oop` is refused because `new Vec2(…)` is an allocation. `domish` is refused
because `for (const c of this.children)` lowers to a *snapshot* of the array —
which is also an allocation. Two different reasons, one cause. Allocation in
compiled code is now unambiguously the next thing worth building; the `for…of`
snapshot is worth removing regardless, since it is a copy of an array nobody asked
for.

### Two bugs the machine caught, and one it would not have

`Value` is `#[repr(u8)]` now, because compiled code has to know where the tag is and
where the payload is, and Rust's default enum layout is deliberately unspecified.
The compiler checks that layout against a real value before it emits a single heap
instruction — and that check earned its keep immediately, twice:

* **A payload is not at one offset.** `repr(u8)` lays each variant out as
  `{ tag: u8, payload }` — so the payload sits at *its own* alignment: a `float64`
  at 8, an `int32` at **4**, a `bool` at **1**. Assuming one offset is the natural
  mistake and it would not have failed to compile; it would have read the wrong four
  bytes and carried on.
* **An `Rc`'s word is not `Rc::as_ptr`.** The word inside a `Value` is the address
  of the *box*, with the reference counts in front of the value. Compiled code that
  read it and handed it back would have been using a different pointer from the one
  the interpreter marshals for the same object. The fix removed the assumption
  rather than encoding it: compiled code never takes a pointer out of a cell — it
  hands the *cell* to the engine and asks (`heap::cell_obj`).

And the one no machine would have caught: **the differential fuzzer had never run
the JIT.** It compared the bytecode VM against the tree-walker, and its generated
programs called their functions a handful of times against a threshold of 64 — so
"no findings" was a statement about a compiler it never invoked. It now runs all
three tiers with the thresholds dropped to zero, and half its programs are *object*
programs.

### The bug that got through anyway: a `float64` holding `null`

Every class the fuzzer generated initialized every field. So it never wrote the one
shape that breaks compiled code:

```mersey
class C { public x: float64; }   // no initializer
new C().x                        // null — out of a field the type system calls a float64
```

**Nothing in the language requires a field to be assigned.** There is no
definite-assignment rule, so a field declared `float64` is `null` from the moment
the object exists until somebody writes to it. Compiled code believes the declared
type: it reads eight bytes of `float64`, and there is no `null` in an `f64`.

It *notices* — every cell's tag is checked. The bug was what it did next. Stopping
is normally answerable by re-running the call on the interpreter, which raises the
real error. But a call that has **already written to an object** cannot be re-run,
so it fabricated an error instead, and the tiers disagreed:

```
Tier 0:  `+` needs numeric operands, got null and float64
Tier 1:  value is not of its declared type
```

The fix is to refuse the one combination that cannot be made to behave: a group
that **writes to the heap** *and* may **read an unset field**. Either alone is
fine — a read-only group that finds a null simply bails, and the interpreter raises
the real error; a group that writes but reads only initialized fields can never see
one. Array elements are safe by construction: an element exists only because a
typed value was put there.

The fuzzer now generates uninitialized fields, and lets the throw escape to where
its *message* is printed — a `try` inside the hot function both stopped it
compiling and hid the thing being compared. With the fix reverted it finds the
divergence in under 400 programs. With it, **22,000 programs, no divergence.**

The deeper fix belongs in the checker: a non-nullable field observed as `null` is
the type system lying, which is the one thing Mersey exists not to do. Definite
assignment (as `strictPropertyInitialization` does) would make the compiler's
assumption *true* and let this restriction be dropped. Logged, not done.

### And one that would have been a wrong answer

Class hierarchy analysis says `s.area()` is a direct call because nothing overrides
`area`. `import(…)` can evaluate a module **later** — and that module can declare a
subclass that does. The class set has not changed behind the engine's back (it was
loaded and typechecked with everything else), but it has changed since the code was
compiled.

Compiled code records how many classes existed when it was compiled and is
discarded when that stops being true. `tests/jit/cha-late-subclass.mersey` proves
it: with the check removed, the JIT prints **12** where every other tier prints
**1007**, because every `Sneaky` would run `Shape.area`. Not a wrong message — a
wrong *answer*, quietly.

### A trap can no longer be answered by running the call again

It used to be that compiled code hitting `x / 0` simply bailed, and the interpreter
re-ran the whole call to produce the error — free of consequence, because a pure
function run twice is a function run once.

That is not available once compiled code can write to an object: re-running it would
write to it **again**. So a trap now carries *where* — the function and the bytecode
position — and the error is built from that, with the same message and the same line
the interpreter would have given. (A group that only *reads* the heap still bails and
re-runs, which keeps the better diagnostics for everything that came before.) The
conformance test pins it with a counter that is incremented before a division by
zero: it must read 501, not 502.

### Where the remaining 2.7× is

Not dispatch, not field access, not calls. It is **allocation** (still interpreted),
the shim call the engine makes to turn a cell into an object address, and Cranelift
at `opt_level=speed` against a decade of V8's inlining. The next honest wins are
allocation in compiled code, and inlining small methods — `energy()` is three
multiplies behind a function call.

## Zero-defaults, live `for…of`, and allocation in compiled code (2026-07-13)

Three pieces, in the order they unlock each other.

### Uninitialized declarations get their type's zero (user decision)

Numbers → 0, `string` → `""`, `char` → `'\0'`, `bool` → `false`, containers →
empty (a fresh one per binding — a shared default array would be aliasing, not
defaulting). Applies to locals, instance fields, statics; resolves through type
aliases, because the **checker** decides (a new `DEFAULTS` side table, like the
coercions) and the engine only reads the answer. `T?` and class-typed fields
still default to `null` — for `T?` that is the honest zero; for a non-nullable
class it is the one remaining type-system lie, tracked under definite assignment.

This *replaced* the `writes && reads_unset_field` refusal from yesterday: a
number-typed field now always holds a number, so the shape being refused no
longer exists. Spec §3.2; `tests/jit/unset-field.mersey`.

### `for…of` over arrays iterates live — and compiles

It used to snapshot: a full copy of the array, every loop. Now an array iterates
by index with the length re-read each pass — JS semantics (growth is seen, a
shrink ends the loop), one less allocation, and identical across all three tiers
(`tests/conformance/runtime/for-of-live.mersey`). Because the VM already lowers
`for…of` to an index loop, making `IterArray` the identity on arrays put the
whole shape inside the Tier 1 subset: `domish`'s recursive `count()` — a method
doing `for (const c of this.children)` — went **2.31s → 0.12s**.

### Allocation in compiled code: the arena

`new` compiles. The engine allocates through a shim (`initial_slots` clone, fresh
containers, GC registration), the **constructor runs as ordinary compiled code**,
and ownership is the part that took design: yesterday's rule was "compiled code
never touches a reference count", and allocation is exactly where that rule ends.

Its replacement: **every object compiled code creates is owned by an arena the
interpreter holds.** Compiled code carries `(ptr, fields, handle)` per object —
handle ≠ 0 means *this value* owns the arena's reference. The handle discipline:

- a slot load, a `Dup`, a field read copy with handle 0 — a handle lives in one
  place (or is explicitly cloned), so a release is never a double-free;
- **overwriting an object local releases its old handle** — the loop that
  allocates three million times holds two objects, not three million (measured:
  +40KB RSS for 10× the allocations);
- everything else is swept by `arena.clear()` when the call ends — on a return,
  a bail, or a trap, the *interpreter* does the freeing, so no unwind path in
  native code can leak or double-free;
- a *returned* borrow is clone-promoted at the return site, so the caller is
  never handed something it has no way to keep;
- a borrow rooted in a re-assignable local (`Prov::FromSlot`) is clone-promoted
  when stored and refused across jump edges — the one dangling shape the static
  pass has to close.

| workload | Tier 0 | Tier 1 before | Tier 1 now | vs node |
|---|---|---|---|---|
| `bench/alloc.mersey` (3M `new` in the loop) | 3.32s | 3.32s (refused) | **0.69s** | 12× |
| `domish` (tree walk, `for…of`) | 2.31s (count) | refused | **0.12s** | — |
| `bench/objects.mersey` (no alloc) | 6.5s | 0.14s | 0.14s | 2.7× |

The remaining 12× on `alloc` is the allocation itself: `Rc` + `GcCell` + GC
tracking + arena bookkeeping per object, against V8's bump allocator. That is
the price of the no-moving-GC decision, being paid exactly where it was
predicted to be paid — and it buys the absence of an entire CVE class.

### The use-after-free the tag check caught

The first version of the assignment path was a real use-after-free: `acc =
acc.add(v)` compiles to `Dup; StoreSlot; Pop`, the Dup'd copy (handle 0) landed
in the slot, and the `Pop` released the owned original — the slot kept a pointer
to a freed object. **The tag check turned it into a clean `TypeError` instead of
memory corruption**, which is the second time that one-compare-per-load has paid
for itself. The fix: `Dup` of an owned object clones its ownership (borrows
still copy free).

The fuzzer now generates allocation in hot loops with churn between the
dangling store and the read (freed memory reads back *intact* unless something
reuses it — a lesson about what use-after-free looks like from a differential
harness). Along the way it turned out the fuzzer's own template had been keeping
every object program out of Tier 1 — an `as` cast in the generated function, and
a threshold of 0 that compiled before callees had bodies. With the Dup fix
reverted, it now finds the divergence **131 times in 300 programs**; with it,
10,000 programs clean. `tests/jit/alloc.mersey` pins the deterministic replay.

Still outside the subset: array/string/map allocation (`[a, b]`, `.push`,
template strings), `super`, getters/setters, casts. Each is now an ordinary
extension, not a design problem.

## The Chromium fork, and the ABI it needed first (2026-07-13)

**`<script type="text/mersey">` runs on the Mersey engine inside Blink.** The
fork is checked out, patched and committed (`~/chromium/src`, branch `mersey`);
`mersey_script_runner.o` and the patched `script_loader.o` compile against real
Chromium on arm64. See `chromium/README.md`.

### The ABI was a year behind the engine

`msy_host_table` had five hooks — `print`, `error`, and three fake-DOM calls
from the Phase 6 demo — while the engine's `Host` trait had grown the universal
object bridge, promises, capabilities, time and entropy. A native embedder got
silent stubs for everything interesting. **Blink would have been talking to the
demo, not the engine**, and that is a bug you discover six months into a fork.

So the ABI came first, and it is now the WASM boundary function for function:
`msy_abi_version`, the full host table, module graphs (`scan_imports` → the host
fetches → `run_graph`), callbacks with arguments, and `MSY_FLAG_NO_JIT` for a
sandbox that forbids a second JIT.

The part that matters most is not a feature: **the loader moved into
`mersey_interp::embed`**, so the WASM host and the C host now share one
implementation. Two copies of a loader is how two hosts drift apart, and the
second copy was about to be written.

`crates/mersey_capi/tests/abi.rs` drives all of it through the `extern "C"`
symbols against a mock page — a handle table of fake DOM objects, events with
JSON payloads, a promise the host settles *after* the script returned, a
capability list the engine enforces. When the Blink glue misbehaves, that file
says which side of the boundary is wrong.

One real bug it found: the capability check for `random` lived in whether the
host had *wired the hook*, not in the grant. A host that provides entropy but
grants no `random` capability has still said no — deny-by-default belongs to the
grant.

### What arm64 Linux costs

Google ships **no hermetic Chromium toolchain for linux-arm64**, and every
prebuilt in `third_party` is x86-64 — an arm64 host cannot run any of them.
Nine substitutions, each a hard failure rather than a preference:

- **clang**, plus a *shadow resource-dir tree* (Ubuntu puts `clang_rt` at
  `lib/linux/…-aarch64.a`; Chromium looks for `lib/<triple>/…`);
- **the Rust toolchain — and it must be rustc 1.98 exactly.** A newer nightly
  mangles the allocator symbols (`__rustc::__rust_alloc`) differently from the
  std it ships, and *nothing links*;
- **`bindgen` + `rustfmt`**, needing a **real** `libclang.so` beside them, not a
  symlink (bindgen's loader will not follow one);
- **lld** for mold, **ninja** for siso (its spawn helper traps on this kernel);
- **node**, pinned to the exact version `third_party/node` checks for;
- **gperf**, which has **no linux-arm64 CIPD package at all** — DEPS carries a
  fork condition and the binary is built from source.

And two fork-side GN args, both upstream-shaped:

- **`use_tot_clang_flags`** — five flags only tip-of-tree clang understands.
- **`rust_std_in_executable_only`** — the one that took the longest to see.
  Chromium's Rust *allocator crate*, which defines the symbols every std rlib
  references, is linked into the **final executable**, not into each component
  `.so`. With the hermetic toolchain that is invisible; with a system one it
  leaves those symbols unresolved in every DSO and `-Wl,-z,defs` kills every
  component link. They resolve at **load time from the binary** — which is what
  a component build *is*, and which `no_unresolved_symbols` already exempts
  sanitizer instrumentation for, in a comment describing this exact situation.

All of it is **scripted** (`chromium/setup-arm64-host.sh`) rather than left as
tribal knowledge, and none of it is needed on x86-64 Linux, macOS or Windows —
which is what makes the rest of the matrix a configuration exercise rather than
another archaeology.

### Next

The universal bridge in Blink (the same table, filled in over real DOM objects);
external scripts through `ScriptResource`; and building the crate graph with GN's
Rust rules instead of a prebuilt staticlib — which is what turns "arm64 Linux
works" into all six platform/arch targets.

## Language completeness, engine debt, tooling (2026-07-12)

**Two bugs found while auditing.** `mersey run /abs/path.mersey` could not find its
own relative imports (the leading `/` was silently eaten, so it worked only if you
were standing in the right directory). And hover on an imported symbol returned
`<error>`, because the language server typechecked the open file *by itself* — an
editor doing that is looking at a different program than the compiler is.

**Language.** Top-level `await` (a module that awaits *is* an async function; its
importers wait for it). Dynamic `import()` — as a *closed* graph: §4.5 closes the
module graph before execution and §5.4 gives running code no authority to fetch
more, so the specifier must be a literal, the module is loaded/checked/locked with
everything else, and what is deferred is its *evaluation*. It is therefore precisely
typed (a promise of *that module's* exports, not `any`), and `import(someVar)` does
not compile. Async generators and `for await` — no new syntax needed, because a
function that yields is already a generator, and the VM already reports all three
outcomes a coroutine can have. Spread arguments, allowed exactly where they are
checkable (a callee with a rest parameter) and refused with a reason where they are
not.

**Engine debt.** `return`/`break`/`continue` through a `finally` now compile
(previously the whole function silently dropped to the AST tree-walker); likewise
`super(...xs)`. A test asserts the compiler *accepts* these, because the runtime
tests could not tell — a silent fallback still gives the right answer, just slowly.
The host hook is `dom_add_listener(id, event, cb)`: the engine had a list of which
events exist, which is not the engine's business. The JIT compiles int64 kernels
(18.05s → 0.61s on a summation loop) — the blocker was an ABI that packed
`(tag << 32) | payload`, which fits an i32 result and nothing else.

**Hardening.** x86-64 forward-edge CFI is reported as a row that is *off* rather
than omitted, with `KNOWN_GAPS` recording why (Cranelift exposes no CET setting in
0.116 or 0.123): a gap that nothing mentions is indistinguishable from a gap nobody
noticed. CI now gates on the fuzzer, the GC write-barrier verifier, the browser
suite, and stale goldens.

**Tooling.** Find-references, rename, signature help and document symbols — all from
the checker's resolution, not a text search: renaming one `value` does not touch a
different `value` in another scope. The LSP no longer leaks an AST per keystroke
(183ms → 50ms for ten hovers on one buffer).

### Not built, and why

* **Bytecode cache.** Startup is 30ms *including* parsing the 18k-line ambient WebIDL
  surface, so the cache would save single-digit milliseconds — and a chunk holds
  `&'static` pointers into the AST (patterns, types, nested function bodies), so
  caching one means first making bytecode self-contained. Wrong trade at this size.
* **Debugger.** Stepping needs either a DAP adapter (standalone) or CDP (browser),
  and the browser half is Stage B work. Errors already carry a Mersey code frame,
  stack trace, and source map.

## Explicitly deferred (tracked, not forgotten)

Threads/workers in-language, decimal float128, AOT native compilation,
non-Chromium native integrations, upstreaming to Chromium. Pointer compression
and a heap cage belong with the native-GC track (Stage B), where the engine owns
its heap; x86-64 forward-edge CFI (CET/`endbr64`) awaits a Cranelift setting.
