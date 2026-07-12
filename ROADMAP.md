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
- [ ] Conformance test harness format (`tests/conformance/`, expected-output
      and expected-diagnostic files)

**Exit:** grammar complete; two people can independently answer "is this
program legal, and what does it print" from the spec alone.

## Phase 1 — Frontend

Lexer (strict UTF-8 decode/validation), parser with recovery, binder, type
checker; `mersey check` and `mersey convert` / `mersey fmt` work end-to-end.

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
