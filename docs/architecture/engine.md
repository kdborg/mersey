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
correctness fallback. Tier-up happens two ways, because one is not enough:

- **Call count.** A function compiles once it has been called 64 times.
- **Loop back edge.** A call counter cannot see the case that matters most —
  one `main`, called once, wrapped around a loop that runs for a minute. Its
  count reaches 1 and stops. So the VM also counts *back edges*, and when a
  loop gets hot it compiles the function it is in and re-enters it **at that
  loop's header**, carrying the live locals across (on-stack replacement).
  Profiling did show it mattered: without it, a hot loop in a cold function
  was never compiled at all.

Note that OSR here is not what OSR is in V8. There, it is entangled with
speculation and deopt — a way back *out* of code that guessed wrong. Here
there is no speculation and no deopt: OSR is only a way *in*, used once, at a
seam where the interpreter's state happens to be nothing but its locals.

## Tier 1 and the heap

Compiled code reaches objects: **a field at a constant offset, an array element,
and a direct method call.** Three properties of the *language* make that possible,
and none of them is available to a JS engine.

**A field is a constant offset, and needs no class check.** Shapes are sealed
(§4.1) and a subclass's layout begins with its base's — so an offset computed for
`Shape` is still the right offset on a `Circle`. Compiled code reads `s.size`
without asking what `s` is.

**A method call is a direct jump.** The class set is closed — a static module
graph, no `eval`, no prototype patching — so the engine can ask *"does anything
below this class override this method?"*. When nothing does, the call compiles to
a direct jump, with no vtable, no inline cache, no guard, and so nothing to deopt
from. When something does, the function is refused and stays in Tier 0. This is
class hierarchy analysis, and it is what deleting prototypes was for.

**Compiled code never touches a reference count.** It reads scalars out of heap
cells and writes scalars back; any object it holds is *borrowed* — never cloned,
never stored, never returned. Everything follows from that one rule:

- it cannot free anything, so nothing it holds an address of can go away;
- it cannot allocate, so the collector cannot run underneath it;
- it creates no edges, so the cycle collector's graph is exactly what it was.

**No write barrier, nothing to root, no GC interaction at all** for code that
only reads and writes scalars. Code that **allocates** — `new` is in the subset
now — works through the **arena**: every object compiled code creates is owned by
an arena the interpreter holds, and compiled code carries `(ptr, fields, handle)`
where a nonzero handle means this value owns the arena's reference. Overwriting
an object local releases its old handle (a loop that allocates three million
times holds two objects); a returned borrow is clone-promoted so the caller can
keep it; and everything else is swept by `arena.clear()` when the call ends — on
*every* exit, return or trap, the interpreter does the freeing. Compiled code
still never frees anything; it only lets go. Array, string and map allocation
(`[a, b]`, `.push`, template literals) remain interpreted — each is now an
ordinary extension of the same machinery.

The layout compiled code assumes (`Value` is `#[repr(u8)]`: a tag byte, then a
payload at *its own* alignment) is **checked against a real value before a single
heap instruction is emitted**. If it were ever false, the compiler declines the
heap rather than reading the wrong bytes. And the pointer inside a cell is never
taken by compiled code at all — an `Rc`'s word is the address of the *box*, not of
the value, so compiled code hands the **cell** to the engine and asks
(`heap::cell_obj`).

## Bytecode (MBC)

**Every value has a type, and the bytecode says what it is.** `BinNum(Add, F64)`
is a float add; `Convert(I64)` widens; every frame slot has a declared type. This
is what makes a slot a *register* — Tier 1 keeps it in a machine register of the
right width — and it is why a function that mixes `int32` and `float64` is just a
function, rather than something the compiler has to refuse.

A **Mersey-to-Mersey call runs inside the dispatch loop**: it pushes a frame and
keeps going, rather than re-entering the interpreter as a nested Rust call. The
callee's locals sit above the caller's in one vector, and so does its operand
stack. A throw walks back up those frames looking for a handler, exactly as it
would have walked back up the Rust ones.

Locals are **frame slots**, resolved at compile time: `LoadSlot(3)`, not a name
hashed into a scope chain. The exception is a local some closure can see — a
closure outlives the frame, so that binding lives in the environment and is still
reached by name. Everything else, including every compiler temp, is an index.

A scope is allocated only when something actually lives in it, so an ordinary
counted loop allocates nothing per iteration.

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

Strings: semantic model is UTF-16 code units (WTF-16), spec §3.4 — `length`
and every index position count units, `s[i]` is the whole `char` beginning at
unit `i` in O(1), and comparison is by unit, which is JS's order. Internal
representation may narrow to 1-byte storage for Latin-1-only strings — the
common case — while keeping O(1) indexing; this is invisible to programs.

The UTF-32 model this line described until the browser work is gone: the
engine hands strings to a JS or DOM host constantly, and code units are what
that host holds. Benchmark checksum parity with the JS twins depends on it.

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
