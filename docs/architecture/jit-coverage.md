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

`tools/jit-refusals.mjs` does it, so it need not be rebuilt by hand each time:

    node tools/jit-refusals.mjs app.mersey            # the histogram
    node tools/jit-refusals.mjs app.mersey --list     # every function, sorted
    node tools/jit-refusals.mjs --diff a.txt b.txt    # what changed between runs

The `--diff` is worth knowing about before you need it. Compiled and refused
*counts* can be identical while the sets differ, so counts cannot tell you
whether a change altered what gets compiled — and if the sets are identical, the
generated code is the same and a timing difference between the two builds has no
mechanism. That is how a believed 3% regression turned out to be noise, after
being measured back-to-back and still read wrong.

## Calls into another module

`top_level_fn` refused them, and so refused any function that calls into
`std:` — which is most library-using code. The guard read:

    // Nothing captured beyond the scope the caller itself resolves in.
    if !Rc::ptr_eq(&c.env, env) { return None; }

The comment is right about what it guards: a nested closure holding locals is
not a direct call, and calling one without its captures reads the wrong values.
"The caller's own scope" is a stricter thing than that, though. `run_modules`
gives every module `child_env(&self.root)`, so an *imported* function's env is
its own module's and never the caller's, and the two are never pointer-equal.

The test is now that the callee's env hangs directly off the root, which is what
"captured nothing beyond a module scope" actually means. The callee is compiled
against `c.env` either way, so its own free names still resolve where it was
written — and `tests/jit/cross-module.mersey` checks exactly that rather than
merely checking that it compiles: two modules each export a `tag` and a
`shared`, each `shared` calls its own `tag`, and crossed resolution is a wrong
answer rather than a bail.

Found by `tools/jit-refusals.mjs` over `tools/std-hot.mersey` on its first run,
ranked top with three functions stopping there.

## The cast a null check leaves behind

`x != null` narrows in the checker and not in the bytecode, so the language
*requires* the cast that follows: `(b as Bytes)`, `(s as string)`. This tier took
a host handle to a reference type and a number to `float64`, and refused the
enclosing function for every other one — which is the shape every `parse`-like
function in the library is written with, and was four of the ten still refusing
once cross-module calls landed.

Two more are no-ops, and provably rather than hopefully. `eval_cast` returns a
string cast to `string` unchanged, and reaches its `return Ok(v)` for anything
that is neither an instance nor a numeric target — which is what an opaque cast
to `Bytes` is. This tier already knows the value's shape; there is nothing left
to check, which is the same argument the host-handle case rests on.

The slot is carried through rather than replaced with a fresh `Stable` one: a
borrow that came out of a field is still a borrow after a cast, and claiming
otherwise is how two use-after-frees got in. `tests/jit/narrowing-cast.mersey`
reads contents back, because a cast that dropped ownership gives the right
length and the wrong bytes.

**`tools/std-hot.mersey`: 30 compiled / 10 refused → 33 / 7.** An `int32?` cast
to `int32` is still refused: that one is a sentinel guard rather than a
pass-through.

(An earlier version of this paragraph said two of the `Call(2)`s arrived *after*
the analysis passed, and therefore were codegen or the wrapper. They did not.
`jit-refusals.mjs` was carrying that flag across functions — a callee prints its
own "accepted every op" in the middle of its caller's analysis — so the mark
landed on whichever function refused next. Fixed there; the giveaway was going
to look for the IR those failures print and finding none.)

### An array parameter the callee grows

`Call` topped the ranking with four refusals and all four were one thing, which
the histogram could not say because every exit from that arm looked identical
from outside. They print their reasons now, and the reason was: *argument 0 is
Val, parameter wants Arr(I32)*.

An array has two shapes here. `Ty::Arr` is a borrowed pointer and a length —
fast to read, impossible to `push` to, since a push can reallocate and move
both. `Ty::Val` (and `Ty::StrArr` for string elements) is an arena opaque, which
is what `ArrayPush1` takes and the only thing it takes. A declared `int32[]`
parameter arrived as the first shape, so `void pushUtf8(out: int32[], …) {
out.push(…) }` could not be compiled — and neither could any *caller*, which
refused one op earlier and reported it against `Call` with nothing connecting
the two.

So a body that grows an array now takes its array parameters as opaques. The
decision is per function rather than per parameter: doing it properly would mean
tracing each push's receiver back to its slot, and the coarse version costs a
read-only array in a growing function its direct form — a slower compile of
something that did not compile at all. `tests/jit/grow-param.mersey` reads every
array back **in the caller** after the callee grew it, and reads contents rather
than lengths, because the hazard in a representation change is a silent copy.

**It bought no time.** `bench/cli/url` measured 9.50ms before and 9.51ms after,
same checksum, 11 warm samples each. The three hot callers still refuse — one op
later than they used to — so `decode` stays interpreted and compiling the
callee alone changes nothing. Worth recording as its own fact: a coverage number
moving is not a speed number moving, and this one was a prerequisite rather than
a win.

### `string` against `string?` at a merge — open, and it bites

The three refusals that survive the above stop at a block merge:

    the two ways into this block disagree at stack 0: Val falling in, Str jumping in

which is `return text == null ? s : text` — the reference twin of the
`x == null ? 0 : x` pair `coerce_edge` already handles for numbers. A `string?`
returned by a native is held as an opaque, so the two arms of that ternary are
`Ty::Str` and `Ty::Val`.

The obvious fix is an `EdgeFix` that relabels the opaque, the way a `Return`
already does through `heap::val_to_str`. **It was written, and it produced a
wrong answer** — so it is not in the tree, and the reproducer is worth more than
the patch was:

```mersey
const q = new URLSearchParams("a=1&b=two&c=%20sp");
acc = acc + q.toString().length;    // 17 interpreted, 15 once compiled
```

`encode(" sp")` gave back `" sp"` instead of `"%20sp"` — but only inside the hot
loop, and only after ~560 iterations. The same call made from top level
afterwards printed correctly, which is why nothing in `mersey test`, the 35 tier
programs, or 50k differential fuzz iterations caught it: all three call these
functions from outside a compiled frame. `tools/std-hot.mersey`'s own checksum
did catch it (`3661120` against `3700000`), which is the argument for that file
existing.

Enabling the rule also compiles three functions that were refused before, so the
bisect that blamed the rule does not distinguish *the conversion is wrong* from
*one of the three newly-reachable functions is wrong*. That is the next thing to
separate — likely by forcing each of the three to compile on its own.

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

**Three of them are done** — the object store, the object push, and the array
store. `reconcile` went 4 compiled / 11 refused to **8 / 8**, and **69.5ms to
63.0**, on identical checksums throughout. What still stands between here and a
compiled `render`:

| stops at | what it is |
|---|---|
| ~~`StoreName`~~ | ~~a module-level `let` written from a function~~ — **done** |
| `NewNamed` on `render` (222) and `work` (164) | `new` of a class with a computed field initializer |

`Batch`'s four methods went in two steps, and the second is the interesting one.
First `feeds_a_push` did not recognise `LoadName` as an argument, so
`this.ops.push(OP_APPEND)` kept the receiver in the shape a push cannot use —
one word, 12/4, neutral on time. Then `this.ops.push(this.str(tag))`, whose
argument is a whole call, which no list of op kinds can recognise.

The list was the wrong idea. The question is whether a later `push` finds *this*
value as its receiver, and the verifier already answers it: `vm::analyze` gives
the stack depth at every pc, and the JIT already calls it. The receiver is ours
exactly when the call's depth is the depth just after the read plus its
arguments, and any op taking the depth below that has consumed the receiver
first, so the scan stops there. **14 compiled / 2 refused, 59.30ms → 56.71**
(−4.4%, six samples a side, non-overlapping).

### Done: `new` of a class that initializes a field

`render` and `work` refuse on `new Batch(…)`, and the reason is one condition in
`class_for_new`:

    if !cls.dynamic_inits.is_empty() || cls.is_host_backed() || cls.is_builtin_error {
        return None;   // "the shim that allocates for compiled code has no evaluator"
    }

`private readonly ops: int32[] = []` is a computed initializer — a fresh array
per instance — and `Batch` has three. So does almost any class that owns a
collection, which makes this a wide refusal, not a corner.

The stated reason is no longer quite true: `heap::alloc` takes the arena, and
the arena carries `interp_ptr`, so the evaluator is reachable. What it needs is
`alloc_instance` plus the `dynamic_inits` loop the interpreter already runs at
`new` — same scope, same `this` binding.

What made it more than a copy: those initializers evaluate arbitrary Mersey
expressions *during instance construction*, which can re-enter compiled code,
and the shim has to signal a throw rather than return a half-built instance. It
follows the contract every other shim follows — stash on `jit_host_error`, hand
back a null instance, let the caller's guard bail — rather than inventing one.

Neutral on `reconcile` (60.33ms against 60.59, six samples a side) because
`render` refuses one op later. The gain is the coverage, not that number — any
class owning a collection was unconstructible from compiled code before this.

`entries` followed, a one-line omission: `VAL_METHODS` listed `keys` and
`values` and not the third, which is the one a keyed reconciler iterates.

### Seven refusals deep, and what that says

`render` is 222 ops, and clearing one refusal has revealed the next every time:

    object into a field → object push → array into a field → array from a call
      → module-level write → push argument by depth → `new` with initializers
      → `entries` → `BindPattern`

Each was real, each is fixed, and each was invisible until the one before it
went. That is this file's own method working — *"a refusal names the op it
stopped on, not the reason it will stop next time"* — but it is also a fair
signal about scale: a function this size is not one fix from compiling, and the
value has been in the gaps themselves. Every one of them was general (any class
owning a collection; any module-level counter; any `xs.push(obj)`), which is why
they were worth taking one at a time rather than special-casing the workload.

`BindPattern` turned out not to be a tier gap at all — it binds into an
*environment*, and its presence sets `needs_env`, so the enclosing function was
refused before the op was ever reached. The fix is in the bytecode compiler, not
here: `bind_target` now lowers an array pattern of plain names to index-and-store,
the same two ops a named binding compiles to. Tier 0 gains the scope allocation
it no longer makes; **`reconcile` 57.10ms → 56.31**. Defaults and rest keep the
general path.

That is worth noticing as a kind: seven of these gaps were the tier refusing a
shape it could have taken, and this one was the *bytecode* handing it a shape
nothing could take. Reading a refusal as "the tier needs a case for this" would
have been wrong here.

`render` now stops at `IndexGet` on the pair — the ninth link, and the first
that is not incremental.

### The ninth link is a representation, not a case

An opaque array's element is assumed to be a *number*: `IndexGet` on a `Ty::Val`
receiver pushes `Ty::I32` (or `Ty::Str` for a `StrArr`), because those are the
two things this tier can carry out of a container it cannot see into.

`m.entries()` yields **pairs** — a container whose elements are themselves
containers — so iterating it gives values typed `Ty::I32` that are really little
arrays, and the destructuring's `IndexGet` on one has a base that is not
`Ty::Val` at all. It refuses, correctly: taking that path would read a number
out of an array.

That is not a missing case, it is a missing shape. The tier's model of an opaque
container is "holds numbers, or holds strings", and a pair needs a third answer —
an element that is itself a handle. The honest options are a new element kind
carried through `val_index_at` and its shim, or a `Ty` for a nested container;
both are a representation change rather than another arm on a `match`, and both
want designing rather than discovering.

Which is a reasonable place to stop pulling this thread. `bench/cli/reconcile`
went 4 compiled / 11 refused at 69.5ms to **14 / 2 at 56.3**, and the eight
refusals cleared on the way were all general — any class owning a collection,
any module-level counter, any `xs.push(obj)`, any array from a call, any simple
destructuring. The ninth is the first that asks for something new rather than
for something withheld.

`tests/jit/dynamic-init.mersey` builds two instances per iteration and checks
they report 2 and 1, never 3 and 3. That is the property that makes a computed
initializer computed: it runs *per instance*, which is why it cannot be folded
into `initial_slots`. A shim that ran it once and shared the container, or
skipped it and left the declared default, would give a wrong number rather than
crash.

The module-level write was the read's mirror and nothing more.
`NameKind::NumGlobal` already told the tier which register a binding holds and
`jit_global_num` read it through `env_get`; `jit_global_set_num` writes it
through `env_set`, with the kind decided at compile time because the checker
fixes a binding's type. Refusing it had cost `applyOps`, and `Batch.apply` with
it for having called one. **10 compiled / 6 refused, and 64.2ms → 59.3 measured
same-window at 5 samples a side, spreads of 0.4%.**

All three of the finished ones were the same shape underneath: take a reference,
then drop the old one, in that order, through a cell the tier already knew how
to address. `cell_set_obj` and `cell_set_arr` are twins because an instance and
an array both cross this tier as `Rc::as_ptr` of their cell, so there was never
a third representation to invent.

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

### The push: two refusals, not one

Worth recording because the design in this file predicted one of them and the
other only showed up on contact.

The predicted half was the **receiver**, and it went as written: an array field
now reads through `cell_val` — an owned handle — rather than `cell_arr`, when a
bounded lookahead sees the read feeding a `push`. `feeds_a_push` is that scan.

Clearing it moved the refusal one place to the right, to the **argument**.
`box_arg` had no `SlotV::Obj` case, so `xs.push(row)` was declined for its
argument having nowhere to be parked — which is most of what a collection of
anything is written to do. The parking already existed: `clone_obj`, the same
one a returned borrow gets, with `boxed` as the release list. Both halves were
present and nothing joined them.

The lesson is the ordinary one for this tier: a refusal names the op it stopped
on, not the reason it will stop next time. Clearing one and re-tracing is the
whole method.

### An array from a call, and an open 3%

The fourth store landed, and the failure that blocked it first time was not the
store at all: a returned opaque was handing back a released identity register
(above). With that fixed, `Ty::Val` into an array field is the same `cell_set_val`
an opaque field gets.

The measurement is a trade, both halves taken back to back:

| | compiled | ms |
|---|---|---|
| a probe whose callee compiles | 2 → **3** methods | 0.07 → **0.06** |
| `reconcile` | 8 → 8, unchanged | 62.2 → **64.2** |

**That 3% was not real, and finding out cost less than believing it would
have.** Dumping every compiled and refused function with its opening ops and
diffing the two builds gives *identical sets* — same functions, same op
sequences, same counts. So the machine code is the same and the only difference
is an analysis path taken for a shape `reconcile` never contains: there is no
mechanism for a runtime difference.

Re-measured at 7 warm samples a side instead of 3: medians 62.89 and 63.26, a
0.6% gap, ranges overlapping (61.85–63.97 against 63.12–63.65). Noise.

Two things worth keeping from that:

- **A same-window A/B is not automatically enough.** Three samples a side with
  the cold run discarded produced two non-overlapping groups — 62.2/62.4/62.5
  against 63.5/63.8/65.2 — and still said the wrong thing. On this machine a 3%
  claim needs more than three readings.
- **When there is no mechanism, look for one before believing the number.**
  Comparing *which* functions compile, not how many, took one command and
  settled it. The counts were 8/8 either way and could not have.

### The first of those, in the parts that already exist

`push` is not missing. `jit_array_push(h, kind, bits)` takes the array **by
arena handle** and kind 2 carries anything that is not a scalar — an object, a
string, an opaque — by handle, one owner throughout. `VAL_METHODS` already lists
`("push", Ty::Val, 1, 1)`, so a push onto an *opaque* compiles today.

What refuses is the receiver's shape, and for a good reason. An array field
reads as `Ty::Arr`, which is (address, elements, length) in registers — and a
push can reallocate, moving both. `Ty::Arr` is the right shape for indexing
(`Elem::Obj` exists; object elements read fine) and the wrong one for growing.

The other reading of the same cell already exists too: `load_cell`'s `Ty::Val`
case calls `cell_val(cell, arena)` and hands back an **owned** handle. So the
missing piece is not a shim, it is a *choice* — read an array field with
`cell_val` rather than `cell_arr` when the read feeds a push, and the existing
opaque path takes it from there.

The choice has to be made in the analysis, because codegen is keyed by pc and
the field read is emitted before the call is seen. A `TSlot` carries no origin
pc, so the practical form is a bounded lookahead at the `GetMember`: scan
forward for a `CallMethod` named `push` whose intervening ops are exactly its
arguments. Conservative, and it covers `this.items.push(x)`, which is the shape
every collection class in the language is written with.

Worth knowing what *not* to do: typing every object-array field as `Ty::Val`
unconditionally. That would enable the push and take indexing with it — an
opaque's numeric index has no object path (`val_index_str` is string-only), so
reads would get slower to make writes possible.

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

**An opaque's two registers are identity and ownership, and a promoted borrow
changes both.** The `Return` arm cloned a borrowed opaque into a fresh arena
entry, then handed back the *original* handle as the identity and the clone as
the ownership. That was harmless while a returning frame leaked its locals — the
original was still alive — and became a released handle the moment the sweep
below started letting them go. A compiled function returning an array built into
a local made its caller raise "host call failed"; the interpreter answered
correctly, and the bug was live for a day.

The string arm of the same `Return` already said this, in a comment, about its
data pointer: parking a borrow makes a *new* thing and the caller must be given
that one. The opaque arm had the identical shape without the fix. Both registers
now carry the owned handle.

Note what it cost to find: neither the differential fuzzer nor the
tier-agreement programs generate a function that returns a container built into
a local *and* a caller that reads it, so nothing here would have produced it.
`tests/jit/opaque-return.mersey` does now.

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
