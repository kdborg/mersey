# Upstreaming brief: Mersey in Chromium

The case, the evidence, and the ask — written for the first conversation
with Chromium reviewers, and honest about what is and is not proven.

## What is being proposed

A second scripting engine behind `<script type="text/mersey">`: a
strictly-typed, class-based language executed by its own engine (never V8),
reaching the web platform through a typed host bridge. The integration
shape already exists in this repository's fork
(`//third_party/mersey` prebuilt engine + `blink/renderer/core/script`
runner; ordered patch plan in `chromium/README.md`), and equivalents run
today in three other engines — Gecko, Servo, and Ladybird — which is the
strongest single argument that the embedding boundary is real and not
Chromium-shaped by accident.

## Why a reviewer should take it seriously

**1. The language is specified and the spec is enforced.** A phased
specification (`docs/spec/`) with a formal grammar, one mode, sealed nominal
classes, C-style numeric semantics, and no `eval`. Conformance goldens are
the behavioral contract: five execution tiers — tree-walker, bytecode VM,
Cranelift JIT, WASM build, and a transpile-to-JS backend — must produce
byte-identical output for the whole suite, and CI gates on it.

**2. Cross-engine parity is measured, not claimed.** Twelve web technologies
run as line-for-line Mersey/JS twins in four browser forks and in stock
browsers, self-timed and checksum-verified bit-for-bit across every leg
(`bench/web/REPORT.md`). Where measurement itself was misleading, that is
documented rather than smoothed over (the Playwright-Firefox
debugger-disables-Ion confound; Ladybird's peak-PSS floor).

**3. Performance is competitive where it matters.** Native-hosted Mersey
runs at parity with the browser's own JS on compute (≈1.0×) and within
small factors on DOM-heavy workloads; the callback-heavy gap was closed
this cycle by making callbacks first-class on the typed bridge tier
(ABI v8: stable callback ids, `setTimeout` with zero JSON on any path —
Chromium fork timers went 181→75ms with checksums unchanged).

**4. The developer story is complete.** Breakpoints in `.mersey` files work
two ways today: a Debug Adapter Protocol server (`mersey dap`, end-to-end
tested; VS Code extension included) driving the engine's `DebugHook`, and —
for the browser — real source maps on the transpiled path, verified over
CDP: DevTools lists `.mersey` sources by path and a breakpoint set through
the map pauses a served page on a real click. Runtime errors render a
Mersey stack and code frame pointing at the erroring expression on every
path. (`docs/architecture/debugger.md`.)

**5. Security posture matches the platform's direction.** Capability-scoped
I/O, deny by default (§5.3); no `eval` and a closed module graph (§4.5), so
what a page can do is auditable statically (`mersey audit`); the C ABI never
unwinds across the boundary; the engine is fuzzed and CI gates on the
fuzzer and a GC write-barrier verifier. Known gaps are tracked explicitly
(`KNOWN_GAPS`: e.g. x86-64 CET awaits a Cranelift setting) — the position
is "documented gap", never "unnoticed gap".

## What exists in the fork, concretely

- `//third_party/mersey`: the engine as a prebuilt Rust staticlib behind a
  frozen C ABI (`mersey.h`, versioned; hosts check `msy_abi_version`).
- A Blink script runner implementing the host table: reflective JSON tier
  for the long tail, interned/typed tiers for hot paths, direct-C++
  dispatch for eleven technologies, timers on the task queue with typed
  callback ids.
- The ordered patch plan (`chromium/README.md`) breaking the integration
  into reviewable steps.

## Honest limits

- The fork tracks a checkout, not tip-of-tree; rebasing cost is unmeasured.
- DevTools debugging of the *engine-native* path (as opposed to the
  transpiled default path) would need a CDP agent over `DebugHook` — the
  surface exists, the agent does not.
- WebIDL coverage is generated from `@webref/idl` and validated by the
  workload suite, not by web-platform-tests; WPT triage is unscoped work.
- One benchmark absence: `fetch` has no native path in the Chromium fork.

## The ask, staged

1. **Conversation** — is a second-engine integration reviewable in
   principle, and against which anchor (intent-to-implement, an
   origin-trial-shaped experiment, or a long-lived out-of-tree fork with
   upstream-friendly seams)?
2. **If yes to any:** review of the embedding seams only (the script-type
   hook and the host-table boundary) — the smallest reviewable unit, and
   the part three other engines have already validated.
3. **Then:** the patch plan as ordered in `chromium/README.md`, with WPT
   triage scoped as its own workstream.

The fallback position is also fine: the fork continues out of tree, the
C ABI stays frozen, and the evidence keeps accumulating. Nothing in the
project depends on upstream acceptance; upstreaming is leverage, not
survival.
