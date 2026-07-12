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

## Phase 1 — Frontend (in progress)

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
- [ ] Type checker (§3 conversions, §4 classes/access control)
- [ ] `mersey fmt`
- [ ] NFC identifier normalization (§2.4) — lands with the binder

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

**Exit:** conformance suite of ≥300 frontend tests passes; the checker
rejects every "removed from JS" construct with a good diagnostic.

## Phase 2 — Interpreter (Tier 0)

Typed register bytecode (MBC) + verifier, interpreter, precise GC v1
(non-moving to start), exceptions, classes/vtables, `string`/`Array`/`Map`/
`Set`, `mersey run`. BigInteger/BigDecimal kernels land here (they gate the
numeric-literal semantics).

**Exit:** real programs run; conformance suite green end-to-end; GC stress
tests (allocation-heavy, cycle-heavy) pass under ASAN/Miri.

## Phase 3 — Standard library + capability runtime

`std:*` modules written in Mersey per the consistent-API rules (§1.3), the
API-consistency lint, capability flags (`--allow-read` etc.), `std:caps`.

**Exit:** stdlib API review complete; `mersey audit` reports capability
surfaces; a non-trivial CLI app (e.g. a static-site builder) ships as a demo.

## Phase 4 — JIT (Tier 1)

Cranelift lowering, tiering policy, W^X code cache, moving/generational GC
v2 with exact stack maps, unboxed primitive arrays, devirtualization +
inlining. Benchmark suite (`bench/`) and CI regression gates.

**Exit:** performance targets in `engine.md` met or the targets are revised
with data; zero deopt machinery (by design — verify none crept in).

## Phase 5 — Browser Stage A (WASM shim)

Engine core compiled to WASM, JS loader (`<script type="text/mersey">`
polyfill), `browser:dom` v1 generated from a WebIDL subset (DOM core, events,
fetch, console), interop marshaling, sample apps.

**Exit:** the conformance suite plus a browser suite run in stock Chrome,
Firefox, and Safari via the shim; a demo app (TODO-MVC class) works with zero
hand-written JS besides the loader.

## Phase 6 — Browser Stage B (native Chromium)

Chromium fork with `enable_mersey` GN flag: `//components/mersey`,
`MerseyScript` in Blink's loader (CSP/SRI/CORS shared), per-Document
contexts, WebIDL binding generator backend, Blink task-runner scheduling,
CDP Debugger/Runtime domains.

**Exit:** Stage A's browser suite passes natively with the shim removed;
DevTools can set a breakpoint in a `.mersey` file; JS↔Mersey interop tests
pass on a page running both.

## Phase 7 — Hardening & performance

Fuzzing (grammar-aware fuzzer for the frontend, MBC verifier fuzzer,
embedding-API fuzzer), security review against spec §5, pointer compression
+ heap cage, DevTools profiler domain, bytecode cache, `mersey-lsp` v1.

**Exit:** 30 days of continuous fuzzing without a memory-safety finding;
security model §5 audited line-by-line against the implementation.

## Explicitly deferred (tracked, not forgotten)

Threads/workers in-language, decimal float128, AOT native compilation,
non-Chromium native integrations, package registry, upstreaming to Chromium.
