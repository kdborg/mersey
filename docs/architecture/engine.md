# Mersey Engine Architecture

## Implementation language: Rust (decided 2026-07-11)

**Rust core + Cranelift JIT backend**, exposing a C ABI.

- Memory safety in the engine itself directly serves the "full security"
  requirement — the majority of historical browser-engine CVEs are engine
  memory bugs, not language bugs.
- Cranelift is a production JIT backend (Wasmtime) with fast compile times,
  W^X-friendly code emission, and x86-64/AArch64/RISC-V support.
- Chromium accepts Rust components behind a C/C++ binding layer, so this does
  not block the browser plan.
- Alternative considered and rejected: C++20 — slightly more idiomatic Blink
  integration was not worth giving up the memory-safety story.

## Pipeline

```
.mersey (UTF-8) ─decode→ code points ─lex→ tokens ─parse→ AST
    ─bind→ symbols ─typecheck→ typed AST (sound)
    ─lower→ Mersey Bytecode (MBC)
                 │
        ┌────────┴─────────┐
   Tier 0: interpreter   Tier 1: optimizing JIT (Cranelift)
   (starts instantly)    (hot functions, profile-guided)
```

Key departure from JS engines: **there is no speculation tier.** V8 needs
hidden classes, inline caches, deopts, and on-stack replacement because JS
types are dynamic. Mersey's types are static and sound, and class shapes are
sealed, so:

- Property access is a compile-time-constant offset load.
- Calls are direct, or single-indirection vtable calls for virtual dispatch;
  the JIT devirtualizes when the receiver type is exact and inlines
  aggressively.
- Primitives (`int32`, `float64`, …) live unboxed in locals, arguments,
  fields, and `Array<primitive>` storage. `Array<int32>` is a flat 4-byte
  buffer, like a C array with a length check.
- No deoptimization machinery at all — Tier 1 code is valid forever, which
  removes an entire class of JIT security bugs (deopt-path type confusion).

The interpreter tier exists for startup latency and cold code, not
correctness fallback. Function-granularity tier-up at a call-count threshold;
loop-granularity OSR only if profiling shows it matters.

## Bytecode (MBC)

Register-based, typed bytecode (distinct opcodes for `int32.add`,
`float64.add`, `bigint.add`, …) so the interpreter pays no dynamic dispatch
on operand types. Verified on load (stack/register type discipline, branch
targets, access-control assertions) so a corrupted cache can't smuggle
ill-typed code — same posture as JVM/WASM verification. MBC is also the
on-disk compilation cache format (`mersey compile`).

## Memory management

Precise, generational, moving GC:

- Exact stack maps from the JIT (no conservative scanning).
- Young generation: bump-allocation + copying; old generation: mark-compact.
- Incremental marking to keep pauses inside a frame budget (browser target:
  sub-millisecond typical pause at 60–120 Hz rendering).
- `bigint`/`bigdec`/`string` payloads are GC-managed leaf objects (no
  interior pointers), simplifying the write barrier.
- Per-context heaps (see security spec §5.2) with pointer compression:
  32-bit compressed references within a 4 GiB cage.

Strings: semantic model is UTF-32 (`s[i]` = code point, O(1)). Internal
representation may narrow to 1-byte storage for Latin-1-only strings — the
common case — while keeping O(1) indexing; this is invisible to programs.

## Standard library

Written in Mersey itself wherever possible (compiled ahead-of-time and
shipped as verified MBC), dropping to native only for: BigInteger/BigDecimal
kernels, IEEE formatting/parsing, Unicode tables (normalization, casing),
and the host I/O shims. Consistent-API rules from spec §1.3 are enforced by
a lint that runs in CI over the stdlib.

BigInteger: limb-based (64-bit limbs), Karatsuba above ~32 limbs (Toom-3 and
FFT later if profiling justifies). BigDecimal: coefficient (BigInteger) +
32-bit scale, java.math semantics per spec §3.7.

## Performance targets (initial, to be validated by benchmarks)

- Startup to first statement: < 5 ms standalone for hello-world.
- Steady-state numeric code: within 1.5× of `clang -O2` C for the same
  algorithm (achievable because types are static and arrays unboxed).
- vs V8 on typed workloads: at parity or better without warmup, since no
  speculation warmup is needed.

Benchmark suite lives in `bench/` from Phase 2 onward and gates regressions
in CI.

## Components

```
crates/
  mersey_front    decode/lex/parse/typecheck → typed AST
  mersey_mbc      bytecode definition, emitter, verifier, (de)serializer
  mersey_vm       interpreter, GC, runtime object model
  mersey_jit      Cranelift lowering, tiering policy, code cache
  mersey_std      standard library (Mersey source + native kernels)
  mersey_capi     C ABI embedding layer (docs/architecture/embedding-api.md)
  mersey_cli      `mersey` binary: run/check/compile/fmt/convert/audit
```
