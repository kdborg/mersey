# What Tier 1 compiles, and how to find out

This file began as a plan for teaching Tier 1 to call a `std:` native. That work
is done, and a good deal more with it, so it is now an account of what the tier
takes and what it still refuses — written for the next person who watches a
workload run at interpreted speed and wants to know why.

## Finding out, before theorising

Two tools, in this order.

**`MERSEY_JIT=0`** says *whether* a function is compiled. Run the workload twice.
If the time barely moves, nothing in it was compiled and the whole gap is Tier 1
declining — not allocation, not dispatch. This is the first thing to try and it
settles the question in one command.

**`MERSEY_JIT_TRACE=1`** says *where* it stopped. It prints every opcode each pass
accepts, names every function COMPILED or refused, resolves name ids
(``LoadName(5) `nibble` ``), and reports a `define_function` failure with the
offending Cranelift IR.

Three things about the trace are worth knowing before reading one:

- **It prints an op *before* attempting it**, so the last line is the op that
  failed, not the last one that worked.
- **A refusal with no `analyze` lines at all is a signature failure** — `sig_of`
  declined before the body was read. That is a different bug from anything in the
  body.
- **Watch all three layers.** The analysis can pass end to end, codegen can pass
  end to end, and the function can still be refused when Cranelift rejects the
  entry wrapper. Two of the nastiest bugs so far looked exactly like an ordinary
  refusal from the outside.

Over a whole library, a histogram of "the last op each refusal printed" ranks the
work. That is how the coverage below was driven: fix the top entry, re-run, look
again — not by guessing which feature was missing.

## What it compiles

**Numbers, booleans, objects, arrays** — the original tier. Slots are registers;
an object is (address, fields, arena handle); an array is (address, elements,
length).

**Strings.** As a parameter, a return, a local, a field, an array element, and a
receiver. Three registers: data pointer, length, and the arena handle that owns
it — nonzero only for a string this value *built*. Twenty methods are emitted,
reaching the interpreter's own `call_member` by handle; `==` needs no arena at
all, being a comparison of code units. A string *field* is read as a borrow and
written as a **copy**, which is the only reason a string may be stored into a
field when an object may not: the field takes its own units rather than sharing a
reference, so there is no ownership to hand over.

**Engine primitives** — `Bytes`, `Url`, `Regex`. One arena handle. As parameters,
returns and fields; indexed (`b[i]`), measured (`.length`), and passed to natives.
An opaque *field* is **owned** where a string field is borrowed, because an arena
entry is the only representation there is: reading one makes an entry, so the
reader has to let it go.

**Containers built in compiled code** — `[]`, `new Map()`, `new Set()` — are
carried as opaques, for the same reason `Ty::Arr` cannot hold them: that shape
caches the element buffer's address and length, and a `push` moves both. `Map`
and `Set` reach `has`/`set`/`add`/`remove`/`size`, which is what a keyed
reconciler is made of. The builtin containers are recognised by their names
binding *nothing* — the same test the interpreter uses — so a program with its own
`Map` gets its own.

**A nullable number** (`int32?`) is one register: the value as an `i64` with
`i64::MIN` for null. Every `int32` fits with room to spare. Note that 0 is an
ordinary value, so the null test is against the sentinel and *not* the
"is it zero" test every other nullable here uses. Where a number is required the
checker has already narrowed it, so the value is unboxed at that point — a guard,
then a reduce.

**`std:` natives**, reached by a compile-time id rather than a name match, with
the four a compiled loop sits on going straight to their one implementation.
`random.fill` goes further and has a typed shim of its own, the way a hot web call
does.

**Statics**, **`throw new Error(msg)`** (lowered as the `NewNamed`/`Throw` pair —
the interpreter builds the error, the compiled body traps), and **free names
resolved in the scope the function was written in** rather than the scope that
happens to be running.

## What it still refuses, and why

Containers crossing a boundary in ways their *element type* matters for. An array
built in compiled code is an opaque, and an opaque's element has no shape — so an
index read off one assumes a number and bails when it is not. `split`'s result is
the exception (`Ty::StrArr`), because that one method's element type is known.
Carrying the same fact for a local declared `string[]` has been attempted twice
and reverted twice; the plumbing works and `slice` on such an array is the
unexplained trigger.

Also absent: writing a top-level binding, nested arrays, and a class instance
returned through a signature.

## Traps, each of which has bitten more than once

**Every width-assuming catch-all.** Adding a `Ty` wider than one register means
covering it in the type's `parts`/`width`, `unflatten`, `LoadSlot`, `StoreSlot`,
the **entry wrapper's slot marshalling**, the result reader, the return site, and
the call-result unpacking. Miss the wrapper and the function compiles end to end
and is then rejected by Cranelift, with nothing in the trace to say why. This has
happened for `Ty::Str`, `Ty::Val` and `Ty::StrArr` — three times, same shape.

**Result-shape dispatch must have an arm per shape.** A method path that branches
on its result type and *falls through* to the numeric branch will read an integer
off a handle. `split` did exactly this before a test caught it.

**An unconditional throw calls `trap`, not `guard`.** A guard leaves its "did not
trap" block behind and nothing follows to terminate it; the verifier reports a
branch to a block that was never finished.

**A refusal is not free.** Tier 1 refuses whole functions, so "refused" is the
normal state for most code — which means the path *to* discovering a refusal has
to be cheap. It once cost 18% of a workload that compiled nothing at all.

**Asking the wrong scope is silent.** Compiled code that cannot find its own
module's globals does not fail: the shim answers "absent" and the body bails on
every iteration, giving right answers at exactly interpreted speed. Worth 34x when
it was found, and invisible until someone counted compilations.

## Verifying

`tests/jit/*.mersey` with `.expect` goldens: each program runs on both tiers and
the outputs are compared. That is the comparison that matters here — these are
ownership questions as much as representation ones, and a mishandled handle loses
a value rather than crashing. Bless with `MERSEY_BLESS=1`.

Measure on the command-line arena (`bench/cli`), which repeats to under 1%. The
browser legs cannot resolve better than about a third, and the **wasm build has no
JIT at all** — so nothing in this file helps the browser polyfill leg. It reaches
the Chromium, Servo and Ladybird forks, which link Tier 1 by default, after a
rebuild; Firefox's development fork compiles it out.
