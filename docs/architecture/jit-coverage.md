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

- **The last `analyze` line is the op that failed** — ops are printed after the
  decision to look at them, and one that is skipped says `(unreachable)` or
  `(folded)`. This was not always true: the trailing `ReturnNull` every function
  carries is unreachable after a `Return`, and it used to be printed anyway, so
  it appeared as the cause of refusals it had nothing to do with.
- **A refusal with no `analyze` lines at all is a signature failure** — `sig_of`
  declined before the body was read. That is a different bug from anything in the
  body.
- **`analysis accepted every op` means look elsewhere.** The analysis can pass end
  to end, codegen can pass end to end, and the function can still be refused when
  Cranelift rejects the entry wrapper. Two of the nastiest bugs so far looked
  exactly like an ordinary refusal from the outside. The three layers now say
  which one declined, and both generated-code failures print their IR.
- **A refusal at `Return` has two causes and the trace names which.** The body
  returned something with no shape, or the *signature* promised nothing because
  the declared return type resolved to nothing. The second prints
  ``return type `int32?` has no shape in this tier — the signature, not the
  body``, which is a different repair entirely.

Over a whole library, a histogram of "the last op each refusal printed" ranks the
work. That is how the coverage below was driven: fix the top entry, re-run, look
again — not by guessing which feature was missing.

## What it compiles

**Numbers, booleans, objects, arrays** — the original tier. Slots are registers;
an object is (address, fields, arena handle); an array is (address, elements,
length).

**Strings.** As a parameter, a return, a local, a field, an array element, and a
receiver. Three registers: data pointer, length, and the arena handle that owns
it — nonzero only for a string this value *built*. A string *field* is read as a
borrow and written as a **copy**, which is the only reason a string may be stored
into a field when an object may not: the field takes its own units rather than
sharing a reference, so there is no ownership to hand over.

Their **methods reach the engine in two ways**, and the difference is most of
what they cost.

Most go by arena handle to the interpreter's own `call_member`: the receiver is
cloned out of the arena as a `Value`, each argument is boxed into an arena slot,
a `Vec` is built to hold them, and the method is found by comparing its name.
That is the general path and it is what a `toUpperCase` or a `replace` needs.

The ones a loop actually sits on do not go that way at all. `indexOf`,
`lastIndexOf`, `contains`, `startsWith`, `endsWith`, `split`, `slice`,
`substring`, `charAt` and `codePointAt` are functions of the receiver's *span*
and one or two more values, so they call a shim over exactly that, keyed by an id
fixed at compile time — no arena, no `Value`, no argument vector, no name. Only
`split` and the three subrange methods touch the arena at all, and only to own a
result they had to allocate. Measured against the general path: 4.8x on
`indexOf`, 7.2x on `startsWith`, 6.2x on `codePointAt`, about 4x on `slice`.

The searching and bounds arithmetic underneath is the interpreter's own
(`find_units`, `slice_units`, and their siblings), called from both tiers, so
there is one implementation to disagree with rather than two. What the
`tests/jit/string-*.mersey` programs pin is everything *around* it — which
argument is which, an empty needle, an index past the end, one that lands inside
a surrogate pair — because that is what two routes to one rule get wrong.

`==` needs no arena either, being a comparison of code units.

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
then a reduce. It crosses as a parameter, a local, a method's result **and a
declared return type** — the last being what every `parse`-shaped function in the
standard library needed before it could be compiled at all.

**`std:` natives**, reached by a compile-time id rather than a name match, with
the four a compiled loop sits on going straight to their one implementation.
`random.fill` goes further and has a typed shim of its own, the way a hot web call
does.

**Statics**, **`throw new Error(msg)`** (lowered as the `NewNamed`/`Throw` pair —
the interpreter builds the error, the compiled body traps), and **free names
resolved in the scope the function was written in** rather than the scope that
happens to be running.

## Values that change shape at a boundary

Three of these used to be refusals, and the reason they are not is the same in
each case: the value was already what the other side wanted, and only its label
had to change.

**A merge whose arms disagree.** `x == null ? 0 : x` — the checker narrows `x` in
the else-arm but a slot's type does not follow, so one arm arrives as `int32` and
the other still carries the sentinel. The narrower side is guarded and reduced;
the wider side sign-extends, which is free.

**A borrow crossing an edge.** A borrow rooted in a re-assignable local lives only
as long as that local, and a block parameter has no provenance to carry the guard
across — so instead the borrow is given a reference of its *own* before it
crosses, and the question stops existing. Strings copy, opaques and objects take a
second arena reference. An array cannot: it has no handle to own.

**A native's result returned as a string.** Every `std:` native's result is an
opaque handle, because the tier does not know what a native returns. Handing one
back where a string was promised re-labels it — same entry, same owner — and the
shim checks, so a value that is not a string bails rather than being read wrongly.

## What it still refuses, and why

Containers crossing a boundary in ways their *element type* matters for. An array
built in compiled code is an opaque, and an opaque's element has no shape — so an
index read off one assumes a number and bails when it is not. Two exceptions
carry the element type: `split`'s result, and a local declared `string[]` —
both `Ty::StrArr`.

`push` on a **typed** array (`Ty::Arr`), which is a representation problem rather
than a missing case: that shape caches the element buffer's address and its
length, and a push moves both. Supporting it needs a handle for an array the tier
holds only by address, and an invalidation of the cached pair afterwards.

`bool?`, which cannot borrow the nullable-number representation even though the
boundary would convert a returned `I32` back to a `Bool`: as a *field* or a
parameter it would be carried as a number, and a `bool?` field write would store
`1` where `true` belongs. It needs a type of its own, not a shortcut.

Also absent: writing a top-level binding, nested arrays, and a function with **no
declared return type** — the tier does not infer one from the body.

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

**Ownership is per *copy*, not per value.** `Dup` makes two values out of one,
and each must survive the other's release — so an owned handle has to be
duplicated, not copied. Copying it left two owners of one reference, and since
`t = expr` lowers to `Dup / StoreSlot / Pop`, the slot kept the string and `Pop`
freed it. Note the shape of the symptom: the length register was untouched, so
the string had the right length and the wrong contents, and it surfaced as a
wrong answer several calls away. A declaration stores without the `Dup`, which is
why only reassignment was affected — and why it went unnoticed.

**A borrow is only as long-lived as the slot it came from.** A `LoadSlot` yields
data and length with handle 0, and the buffer lives in the arena under the
*slot's* handle — so overwriting that slot frees what the borrow points at.
`Prov::FromSlot` marks these, and every type that carries a handle must be on
that list and be handled at the store: cloned, or declined. A type missing from
the list takes neither option, which is the third and worst one. This is the same
bug as the paragraph above by a different route, and it reached a shipped release
the same way.

**`box_str` and `own_str` are not interchangeable.** `box_str` answers from the
interpreter's small memo, which owns what it parks and releases it when it
displaces it — right for a receiver or an argument, which are finished with
before the call returns, and wrong for anything that must outlive it. Those want
`own_str`, which parks a copy the caller owns. Note that copying moves the data:
a handle that names a copy while the pointer still names the original is a
dangling read with extra steps, so `own_str` hands back both.

**A callee's bytecode may not exist yet.** A function is compiled to bytecode on
its first *call*, so a helper on a branch nothing has taken has no chunk — and a
caller that merely names it cannot be compiled, because a call this tier cannot
describe is a call it cannot make. Build it on demand. The cold branch never has
to run; it only has to exist, which is how a validator is written.

**A fall-through into a labelled block is a predecessor like any other.** It has
to agree with the jumps that reach it, and it is easy to write the merge as
"overwrite the stack with the recorded types" and never check. Where the two
disagree about machine type Cranelift will catch it for you; where they agree —
`Ty::Val` and `Ty::StrArr` are the same two registers — nothing will.

**The status word is shared by the whole group.** A callee that reports something
through it is not describing its own result, it is overwriting its caller's. Say
it in the return registers whenever the type has a way to.

**A refusal is not free.** Tier 1 refuses whole functions, so "refused" is the
normal state for most code — which means the path *to* discovering a refusal has
to be cheap. It once cost 18% of a workload that compiled nothing at all.

**Asking the wrong scope is silent.** Compiled code that cannot find its own
module's globals does not fail: the shim answers "absent" and the body bails on
every iteration, giving right answers at exactly interpreted speed. Worth 34x when
it was found, and invisible until someone counted compilations. The same mistake
came back for *declared types* — a return type or a parameter type is a name, and
a name means what the scope it was written in says, so resolving one against the
running globals read a module-defined class as nothing and the signature came out
`void`. Anything that resolves a name needs the defining scope, not the running
one.

**A signature describes registers, not types.** `param_types` says which register
a parameter is carried in, and a `bool` is carried in the `i32` an `int32` is —
so a signature says `int32` where the value says `bool`. Comparing the two for
equality refused every call to every function taking a `bool`, callers included.
The rule that works is the one `Return` uses: same machine class, both integral.
Representation is what agrees here; the checker has already settled the types.

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
