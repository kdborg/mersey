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

## Shapes that leave the tier, found by asking rather than by histogram

The refusal histogram over `std:` ranks what the *standard library* does, and the
standard library is not a representative program. Writing a probe of ordinary
language constructs instead — inheritance, closures, `Map`, arrays of objects,
`try`/`catch`, `switch`, template strings, nested loops — found three common
shapes that leave Tier 1 and were invisible to that histogram. Each costs about
51x on anything hot, because a function that leaves the tier leaves it whole.

**`try`/`catch` (`PushHandler`).** Any function with error handling is
interpreted entire.

Counted before assuming: `try` appears **zero times in any benchmark** and twice
in the whole standard library, both in `std/test.mersey` — the test harness,
which is cold by construction. (The first count said otherwise because `try {`
is a substring of `Entry {`.) So there is no evidence today that it sits on a hot
path, and that is the argument against doing it now rather than any argument
about difficulty.

The difficulty is real too. The trap model bails and lets the interpreter re-run
the call, which is sound only while nothing has been written — and a `try` block
exists to contain code that writes. Doing it properly means **deoptimisation**:
bailing out of compiled code mid-function with the slot values handed back and a
resume pc, so the interpreter can continue with the handler stack intact. The
machinery is half-present — the entry wrapper already marshals slots *in* for
OSR, and this is that in reverse — but a bail today discards compiled state
rather than exporting it, so the export is the new part.

Worth revisiting when a real workload shows a `try` in a hot function, and not
before: it is a large piece of work whose benefit nothing here can currently
measure.

**`super.method()` (`CallSuperMethod`).** A super call is *statically* bound, so
it wants the direct-call path the tier already has, not virtual dispatch. The
catch, worked out and recorded here because it is the whole difficulty: `super`
resolves from the class that **declares** the running method, not from the
receiver's class. For a method inherited by a further subclass those differ —
compiling `Derived.score`'s chunk with a `Grand` receiver must still reach
`Base.score`. `JitFn` carries the receiver (`this`) and not the declaring class,
so this needs the declaring class threaded through `direct_method`, which already
computes it internally as `defining`.

**An array literal of objects (`ArrayPush1`).** `[new Base(1), new Base(2)]`.
Done — references cross by arena handle now.

### A second probe, and six more

The same trick again, on shapes the first probe missed: getters and setters,
statics, optional chaining, nullish coalescing, recursion, default parameters,
`Set`, iterating a `Map`, `break`/`continue`. Eight of fourteen functions
compile. The six that do not:

| shape | stops at |
|---|---|
| ~~a **setter** (`g.value = x`)~~ | ~~`SetMember`~~ — **done**, below |
| a **static field** read (`G.made`) | `GetMember` |
| **optional chaining** (`o?.value`) | `OnNullJump` |
| **nullish coalescing** (`a ?? b`) | `NotNullJump` |
| a **default parameter** | the callee has no describable signature |

#### The setter — done

A getter compiled and its setter did not, which looked like an omission by
symmetry rather than a difficulty, and was. `o.p` is a field read's syntax over a
method call's body; `o.p = v` is the same instruction the other way round. There
is now a `setter` hook beside `getter`, a `ClassDef::lookup_setter` beside
`lookup_getter`, and an arm in `Op::SetMember` that resolves the body, checks the
one parameter, and lets the ordinary call path take the stack — which is already
the right shape, receiver then value.

Two things are *not* symmetric with the getter:

- **The value has to outlive the call.** `o.p = v` evaluates to `v`, and the call
  path releases every argument once the callee returns. So the setter is handed a
  **duplicate** — a cloned arena handle for a string, an opaque or an object,
  nothing at all for a scalar. Hand it the original and the assignment's own
  value is freed memory: right length, wrong contents, the same shape as the two
  use-after-frees this tier has already shipped. `tests/jit/setter.mersey` reads
  the string case back as *contents* for that reason.
- **The call answers with nothing.** `sig.void` is required, and the placeholder
  result is dropped rather than left on the stack.

The subclass guard is the getter's, unchanged: a subclass that re-declares either
accessor takes over what `o.p` means, so the direct call is refused. The probe
covers it — a `Base`-typed receiver holding a `Sub` prints the subclass's number.

Nothing in `std/` or the benchmarks uses a setter, so this moves no existing
measurement; what it moves is any user code that does, by the usual factor,
because a refused *write* refused the whole enclosing function. The probe runs
0.15s → 0.02s.

`a ?? b` has been **attempted twice and reverted twice**. What each attempt
established is worth more than the attempts:

- The shape looks like it falls out of what is already there — the value jumps to
  the merge when it is not null, the fall-through evaluates `b`, and the two arms
  meet as `int32?` against `int32`, which `coerce_edge` settles. It does not.
- Recording the *nullable* type on the jump edge is wrong twice over: `x ?? 3` is
  an `int32`, not an `int32?`, so `const v = x ?? 3` cannot store the result into
  its own slot and the function is refused at the `StoreSlot`. The jump is taken
  *because* the value is not null, so the non-nullable form is what crosses.
  Narrowing there makes it compile.
- It still gives wrong answers, and only for `Ty::I32Opt` — **the string and
  reference cases are correct**, which is the strongest clue there is, because a
  reference needs no coercion at the edge and a nullable number does.
- The wrong value is exactly **0**, which is `i64::MIN as i32`. That is the
  sentinel being *reduced* rather than the null path being taken — so the fault
  is that the narrowing coercion is not being applied on the edge where it was
  recorded, not that the branch goes the wrong way.
- The wrongness only appears once the loop is hot enough to leave the
  interpreter: the totals are exactly the pre-OSR iterations' worth, which is why
  a small `n` looks fine. Any probe for this must be hot.

Next attempt should start by checking whether `coerce_jump`'s entry for the
`NotNullJump` pc survives to codegen — `record_block` writes the same map with
`insert` and may be replacing it.

Ranked by how much ordinary code they cover, `?.` and `??` are first — they are
how nullable values are read at all — then statics.

## The largest refusal left: an object stored into a field

`this.node = node`, where the field holds another object. Nine ops, and it
refuses the constructor. The `Op::SetMember` arm says why, and the reasoning is
sound as far as it goes:

> Storing an *object* would replace one reference-counted value with another —
> an owned reference released and an owned reference taken — and compiled code
> does not do that.

What that costs is not one store. It cascades: a refused constructor makes every
`new` of that class refuse, which makes every function that constructs one
refuse. `bench/cli/reconcile` — the keyed reconciler out of `bench/web`'s
`frameworkui2`, with its single host crossing stubbed so the engine half can be
measured — has **11 functions refused and 4 compiled**, and its 222-op `render`
stops at `NewNamed`, two ops in. `Entry { node, v }` is the whole reason.

The measurement that settles what that is worth:

| | ms |
|---|---|
| Node | 11.36 |
| Bun | 5.50 |
| Deno | 5.94 |
| **Mersey** | **70.24** |
| Mersey, `MERSEY_JIT=0` | 73.10 |

**Tier 1 buys 6% here**, which by this file's own first rule means nothing in it
is compiled. 12.8x Bun is the worst row in the CLI arena by a wide margin, and
it is not a string problem or an allocation problem — it is this one refusal,
reached through every constructor that holds a reference.

**The store itself is done** — `heap::cell_set_obj`, and `Op::SetMember` now
takes an object whose class is the field's or below it. `Entry`'s constructor
compiles. The cascade is not finished, so `reconcile` has not moved (69.3ms
against a 69.5ms baseline — neutral, measured five runs a side and discarding
the first, which is a cold relink and reads 30% high). Two refusals remain
between here and `render`:

- `this.items.push(n)` — an object pushed onto an array field.
- `this.created = applyOps(…)` — an array *of objects* stored into a field.

Both are the same question one level out, and until both are answered `render`
still stops at `NewNamed` and the reconciler still interprets.

It is also more tractable than when the comment was written. Releasing the old
reference and taking the new one is exactly the discipline the frame sweep
already needed (see "A frame that returns has to let go of what it owns"), and
`heap.rs` is where the refcount handling is allowed to live. The field's cell is
reachable, the new object's arena handle is in the third register, and the
interpreter does this store on every Tier 0 pass already.

Note what it is *not*: fields declared without an initializer compile fine, both
with and without. That was the first guess and it was wrong.

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

**A frame that returns has to let go of what it owns.** Nothing swept a callee's
frame. `jit_arena.clear()` runs when the *outermost* compiled call returns, so a
value a callee parked in a local — a `split` result, a built string — survived
until then: one arena entry per call, for as long as the outer call ran. The CLI
`strings` benchmark peaked at **89 MB where the same program interpreted took
6.3**, and it grew without bound with the work, which for a long-running server
is not a tuning question. It was invisible from the outside because nothing was
*wrong*, only retained; both tiers printed the same answer.

Two things make the fix subtle:

- **Order.** A local handed back is a *borrow* — a load carries handle 0 — so the
  return promotes it first (a string is copied, an opaque or object takes a second
  reference), and only then may the frame be swept. Sweep first and the promotion
  copies out of the entry it just freed, which is a wrong answer and not a crash.
  `tests/jit/frame-sweep.mersey` reads contents back after allocation churn for
  exactly this reason.
- **Parameters are not the callee's.** The caller hands its handle over for the
  duration and releases it the moment the call returns, so a callee that released
  it too would release it twice. Parameter slots are excluded.

The root is left alone: its frame is cleared wholesale on the way back to the
interpreter, and its slots may hold references the OSR entry parked there.

The cost is real and small: `strings` pays 8.6% (52.5 → 57.0 ms) for 10x the
memory back, `url` *gains* (10.8 → 9.7 ms, 12.5 → 9.2 MB), and the rest are
unchanged. Freeing 900k entries as you go is not free; retaining them is worse.

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
