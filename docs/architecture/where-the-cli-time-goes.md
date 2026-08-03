# Where the CLI benchmarks' time goes

Companion to `where-the-browser-time-goes.md` and `where-the-string-time-goes.md`,
for `bench/cli` — the arena with no browser in it, where a number is a number.

Measured on macOS arm64 at `26bfdf7`, self-reported milliseconds (the `RESULT`
line), median of five warm runs. `MERSEY_JIT=0` is the same binary with Tier 1
switched off, so the ratio is what compiling buys and nothing else.

| workload   | Tier 1 | interpreted | ratio |
|------------|-------:|------------:|------:|
| crypto     |   0.51 |        2.70 | 5.3×  |
| strings    |   54.7 |       252.6 | 4.6×  |
| json       |   0.73 |        2.38 | 3.2×  |
| encoding   |   1.54 |        3.04 | 2.0×  |
| url        |   9.54 |       18.67 | 2.0×  |
| reconcile  |   55.3 |        64.2 | 1.16× |

`crypto` needs `--allow-random`; without it the program raises before it times
anything, which reads as a broken benchmark and is not one.

## What a session of this moved

Both engines built and run alternately in one window — run-by-run, not all of
one then all of the other, so drift lands on both sides. Checksums identical on
every row.

| workload | before | after | |
|---|---:|---:|---|
| strings | 355.45 | 47.24 | **7.5×** |
| crypto | 3.32 | 0.55 | **6.0×** |
| encoding | 4.22 | 1.65 | **2.6×** |
| reconcile | 149.50 | 60.53 | **2.5×** |
| url | 25.00 | 10.60 | **2.4×** |
| json | 1.07 | 0.83 | **1.3×** |

Geometric mean 3.1×. `crypto`, `encoding`, `json` and `url` are byte-identical
source across the two builds; `strings` and `reconcile` were *added* partway
through, which is the honest asterisk on the two largest numbers — the engine
was worst at exactly the work nobody had been measuring, and adding the workload
is what found it.

**Re-measured after a further dozen commits of coverage work: unchanged.** 3.15×
against 3.09×, every row within noise of where it was. Those commits took
`tools/std-hot.mersey` from 30 compiled / 10 refused to **38 / 0** and 11% of
its wall time, and moved this arena not at all.

That is worth stating plainly, because `std-hot` is a *driver* written to rank
coverage gaps, not a benchmark. Its compiled/refused count is a good map of what
Tier 1 cannot do and a poor proxy for what anything runs faster. The shapes
those commits fixed — nested arrays, arrays of instances, a method compiled
before its first call — are the shapes `std:` is written in and `bench/cli` does
not reach, except in `reconcile`, whose hot method is refused for a different
reason entirely.

Coverage is not speed. It has been said in this file twice already and is worth
a third.

**The gap that remains is `reconcile`.** `REPORT.md` puts Bun at 5.5ms against
this engine's ~56, an order of magnitude, where the same table has Mersey ahead
of all three runtimes on `calls`, `crypto`, `json` and `encoding`. Its hot
method is refused by Tier 1 for several unrelated reasons at once (see
`jit-coverage.md`), so it runs interpreted in a benchmark built to look like a
UI framework's reconciler — which is the shape of work this engine exists for.

## Coverage is not the lever any more, except in one place

Across `tools/std-hot.mersey` and all six workloads there are **five refusals
left**, and two of them are the same function twice removed. Everything else
compiles. Adding coverage to this arena has run out of road — which is worth
saying plainly, because "what does Tier 1 still refuse" was the question that
drove the last several rounds of work and it no longer has much of an answer
here.

The exception is `reconcile`, and it is the whole of the 1.16×.

## reconcile: 27% of the profile is the interpreter

`sample` over a 5000-round run, top of stack, 3737 thread samples:

| symbol | samples | share |
|---|---:|---:|
| `vm::exec` | 1013 | 27% |
| `drop_glue<Value>` | 387 | 10% |
| `Value::clone` | 237 | 6% |
| `Interp::call_member` | 124 | 3% |
| `Interp::try_jit_args` | 118 | 3% |
| `mi_malloc_aligned` | 112 | 3% |
| `memmove` | 111 | 3% |
| `IndexMap::shift_remove` | 95 | 3% |

`vm::exec` at 27% is `ListView::render`, which is refused, and `work`, which is
refused only because it calls `render`. `try_jit_args` is the toll on the other
side of the same fact: an interpreted `work` re-marshals arguments every time it
calls into something compiled.

**This corrects an inference recorded in the previous commit.** A 1.09× ratio
was read there as "the time is not in uncompiled code, so coverage is not the
lever". It is exactly backwards: the ratio is *low* because the hot method is
interpreted in both columns, so both columns pay for it and the difference
between them is only the other 73%. A ratio cannot distinguish "nothing left to
compile" from "the one thing left to compile dominates" — the profile can, and
should have been run first.

## What blocks `render` is one thing wearing two hats

    for (const [id, entry] of nodes.entries())   // IndexGet on a [int32, Entry] pair
    const entry = nodes.get(drop[i]); entry.node // GetMember on what a Map gave back

Both are values out of a `Map`, and a `Map` value has no static shape in this
tier: `Ty::Val` is an opaque handle, `IndexGet` on one is typed as `int32`
(right for `Bytes`, which is what that path was built for), and `GetMember` on
one has no class to look a field up in.

Neither is a wrong answer — the checker's types mean a mis-typed opaque is
always caught by the op after it, which refuses. They are refusals, and they
queue: **rewriting the destructure away moves the refusal to the `GetMember`
four lines later and buys nothing.** That was worth finding out before building
anything, and it is the argument against fixing either one on its own.

The fix they both want is a typed `Map` — the checker knows `Map<int32, Entry>`
and this tier does not.

**It would not be enough, and that was worth an hour to find out.** Rewriting
`reconcile` so that its `Map` holds `int32` throughout — which is what a typed
`Map` would amount to for this workload — walks the refusal forward rather than
removing it:

| after removing | `render` stops at |
|---|---|
| — | `IndexGet` on the `entries()` pair |
| the destructure | `GetMember` on what `get()` returned |
| the `Entry` values | `CastOp`, an opaque cast to `int32` |
| *that* (shipped) | `CallMethod` |

Four in a row, each invisible until the one before it was gone. A feature built
for the first would have bought nothing, and so would a feature built for all
three — the fourth is still there. `render` is 48 lines that touch a `Map`, a
`Set`, an array of objects, template strings and a batching object, and the
honest reading is that it is refused for *many* reasons rather than one.

So: do not build a typed `Map` expecting `reconcile` to move. The way to know
what would move it is to keep walking this table until `render` compiles, in a
scratch copy, before writing any engine code at all.

## strings: the write barrier and the drop were asking a question they knew

`strings` is 4.6x from Tier 1 and fully compiled, so its time is *in* generated
code. `sample` over a 4M-iteration run said otherwise about what that code
spends it on — allocator ~15%, arena and GC ~14%, `memmove`/`memcmp` ~9%, and
the actual searching (`str_search`, `find_units`) ~5%.

Two entries were the same fact. `GcCell::drop` was the single largest symbol at
7%, and `_tlv_get_addr` — thread-local access — was 3%. Both came from asking
`OLD` a question: the drop removed the cell from `OLD` and `REMEMBERED`, and
`borrow_mut` asked `OLD` whether to remember a write. For a young object the
answer is always no, and every `s.split(".")` in a parse loop allocates one and
lets it go.

The cell remembers instead — one bool, set at the single place anything enters
`OLD`. The asymmetry is what makes it safe: a stale `true` costs a wasted probe
and a conservative extra root; a stale `false` is a missed write barrier and an
object swept while live. So it is set on promotion and never cleared.

| workload | before | after |
|---|---:|---:|
| strings | 57.0 | **43.3** |
| reconcile | 60.2 | **54.8** |
| url | 10.06 | **9.53** |
| encoding | 1.53 | 1.52 |

Measured back to back on one machine, median of six, same checksums.
`_tlv_get_addr` leaves the profile entirely. `strings` against the Node twin
goes from 2.1× to 1.7×.

`GcCell::drop` stays at 7% of a smaller total afterwards, because what is left
in it is not the lookups. It moved its children into a fresh `Vec` and then
extended the queue with it — an allocation, a free and two copies to hand three
values to a queue that was going to take them anyway. That buffer now lives
across drops for its capacity, which is worth about **1%** on `strings`:
medians 43.6 against 43.3, every quantile lower, distributions overlapping. A
real mechanism at the edge of what this benchmark resolves, recorded as such.

Handing `take_children` the queue *directly* is the obvious version and is
wrong: it can drop a value, which re-enters and finds the queue borrowed. The
existing cycle-collector test catches that; a 300k-link chain does not, which is
why both now exist.

## reconcile, profiled again

Unchanged after everything above. 3562 samples, `N=5000`:

| symbol | share |
|---|---:|
| `vm::exec` | 27% |
| `drop_glue<Value>` | 10% |
| `Value::clone` | 6.7% |
| `Interp::call_member` | 4.4% |
| `Interp::try_jit_args` | 3.7% |
| `mi_malloc_aligned` | 2.7% |

`call_member` is the *general* method path — what a `Map` or `Set` call takes,
since the inline cache is for instances. It looked like a target: `split_args`
builds a `Vec` with `split_off` for every one of them, and `str_member_units`
already exists to avoid exactly that for strings, justified when it was written
by a measurement.

**Tried, and neutral.** A slice-based `map_set_member` for `get`/`set`/`has`/
`add`/`remove` measured 54.18ms against 54.55ms — inside the noise, on
overlapping distributions. Reverted, and not only for the absence of a gain: it
duplicates `Map::set`'s insertion-order promise into a second place that has to
keep agreeing with the first, which is a poor trade for nothing.

That is the third allocation-removal this session to come out neutral, after the
string-buffer pool and the receiver-clone move. The pattern is consistent enough
to state as a rule: **removing one small allocation per operation is not
measurable here.** mimalloc's small-size path is about as cheap as whatever
replaces it, and the changes that have shown up — the closure allocation at
4.8%, the GC flag at 24% — removed work from *every* operation rather than one
allocation from each.

So `reconcile` still says what it said: 27% in `vm::exec`, all of it the one
refused method, and nothing else on the list is worth attacking while that
stands.

## What is left needs a decision, not a patch

After all of the above the profile is allocator-shaped: `mi_malloc_aligned`
~8%, `mi_free` ~5%, `memmove` ~6%. A `parse` in this workload allocates about
sixteen times — `Rc<Vec<u16>>` is two allocations per string, and `slice` and
`split` copy rather than share. V8 answers both with sliced strings, where a
substring is a view onto its parent.

Both were tried.

**`Rc<[u16]>` is blocked**, and not by `unsafe_code = "forbid"` — `mersey_jit`
has no `[lints]` section and does not inherit it. It is blocked at the argument
boundary, where each argument already occupies exactly two cells, a value and an
ownership handle, both spoken for. A slice needs three: pointer, length, owner.

**Sliced strings did not need a wider `Value` at all**, because inside a
compiled frame a string is *already* a pointer and a length. `slice`,
`substring` and `charAt` name contiguous subranges, so `heap::str_sub` now
returns a borrow — pointer arithmetic — where it used to build a `Vec`, wrap it
in an `Rc` and park that in the arena. Worth **12% on `tools/std-hot.mersey`**
(0.60s → 0.53s, non-overlapping over eight runs).

It is worth **nothing on `strings`**, which is the workload it was meant for.
Every slice there is stored straight into a local — often the same local, as in
`s = s.slice(0, plus)` — and a stored borrow must be promoted, so the
allocation moves from `str_sub` to `own_str` and the count is unchanged. The
win is in library code that *consumes* a substring rather than keeping it.

### Sharing the parent's entry instead of copying: tried, neutral

The obvious next step, and it does not pay. A stored slice has to own something
before its source slot is overwritten, and it currently owns a **copy** of the
units. It could instead own a second reference to the *source slot's* arena
entry — `clone_val` on a `Value::Str` is an `Rc` bump, not a buffer copy — which
is a refcount where the copy is an allocation and a memcpy.

Measured back to back: medians 45.8 against 46.0 over six warm runs. Nothing.

Two reasons, both worth knowing. The slices here are short (`"1"`, `"2"`,
`"rc.1"`), and mimalloc's small-allocation path is about as cheap as an arena
lookup plus a `Value` clone plus an arena push. And sharing *retains* the
parent: after `s = s.slice(0, plus)` the original full string stays alive behind
the slice, so the arena holds more for longer. Reverted.

### What is left

The remaining 1.7× against Node stands, and what would close it is unchanged: a
string value cheap enough to allocate. `how-other-engines-hold-strings.md` is
what V8 and JavaScriptCore do about that, and it reframes the question — the
largest gap is not a representation at all but the *allocator*, since both
engines bump-allocate short-lived strings in a nursery where this one pays
mimalloc plus two allocations per string. It also records that V8 refuses to
share a substring under thirteen characters, which is the same answer the
experiment above reached the hard way.

## The rest of the profile

`Value::clone` and `drop_glue<Value>` together are 16%, which is the 16-byte
`Value` being moved in and out of containers — the same cost
`where-the-string-time-goes.md` reaches from the other direction, and not
specific to this workload. `IndexMap::shift_remove` at 3% is `nodes.remove()`
and is O(n) on purpose: a `Map` iterates in insertion order (§6.5), so the
cheap `swap_remove` would be observably wrong.

## Tier 1 on: where `reconcile`'s remaining time is

The table above says `reconcile` gets **1.16×** from Tier 1 — the smallest ratio
in the arena by a wide margin. That is not a coverage problem: the arena refuses
nothing except two functions in this workload, and those are worth ~2% (see
`jit-coverage.md`). Compiling was never what `reconcile` was short of.

So, profiled with Tier 1 **on** — `sample` for 6s over a copy scaled to N=5000,
4137 thread samples, top of stack:

| symbol | samples | share |
|---|---:|---:|
| `vm::exec` | 1202 | 29% |
| `drop_glue<Value>` | 468 | 11% |
| `Value::clone` | 299 | 7% |
| `Interp::call_member` | 165 | 4.0% |
| `Interp::try_jit_args` | 145 | 3.5% |
| `memmove` | 129 | 3.1% |
| `mi_malloc_aligned` | 104 | 2.5% |
| `IndexMap` (probe) | 99 | 2.4% |
| `Arena::keep` | 79 | 1.9% |
| `mi_free` | 71 | 1.7% |
| `sip::Hasher` | 48 | 1.2% |

**47% is the interpreter and its refcount churn** — `exec`, `drop_glue`,
`clone`. That is the same 22%-of-Tier-0 structure written up in
`where-the-interpreter-time-goes.md`, which scopes the one mechanism that would
reduce it (last-use moves on `LoadSlot`) and rejects it for costing correctness
surface in two places and the debugger in a third. Tier 1 being *on* does not
change that: what it compiles leaves this behind rather than reducing it.

`sip::Hasher` is answered where it lives — see the note on `MapData` in
`lib.rs`: the whole hasher is worth 3.3% of this workload and does not buy out
collision resistance on attacker-chosen keys.

### `try_jit_args`, and a fourth data point for the allocation rule

3.5% is the interpreter/Tier-1 boundary: the entry guard per slot, the cache
lookup, the arena setup and teardown. Its one obvious allocation is the argument
buffer — `Vec::with_capacity(args.len() + 1)`, once per compiled call.

Replaced with a `smallvec::SmallVec<[JitArg; 8]>` so the common arity never
leaves the stack. `JitArg` is 24 bytes, so that is a 192-byte inline buffer.
Measured against the same binary alternately, four pairs per workload:
`reconcile` **54.5 against 53.9** — slightly *worse* — `strings` and `url`
neither. Retried at `[JitArg; 4]` in case 192 bytes was itself the cost:
medians 54.2 against 54.2, neither.

Reverted, and the dependency with it. This is the fourth measurement in this
session to say the same thing — **removing one small allocation per operation is
not measurable** — and the first to have it come out the wrong way, which
sharpens it: mimalloc's small-size path costs about what initialising an inline
buffer costs, so this is not a trade that has a favourable side. The wins in
this session that *did* show up removed work from every operation (a GC flag,
24%) or removed an allocation happening ~25 times per operation (the per-call
closure, 4.8%). One per call is below the floor.

What that leaves for the boundary: nothing worth taking on its own. The 3.5% is
spread across the per-slot guard, a hash lookup, and two `clear()`s, none of
which is a majority of it, and the taken path's hash lookup is the only one with
an obvious cheaper form (a memo on the chunk, the way `jit_refused` already
memoises the refusal). At 3.5% total that is a fraction of a fraction.

## Module-level numbers were read by name, per read

The `csv` twin's profile put **~32% in resolving global names** — `env_get`
13.9%, `memcmp` 7.5% comparing them, `jit_global_num` 6.2%, `heap::global_num`
4.8%. `std/csv.mersey` opens with

    const QUOTE: int32 = 34;
    const COMMA: int32 = 44;
    const LF: int32 = 10;
    const CR: int32 = 13;

and its parser reads them several times per character. Each read, *from compiled
code*, was a shim call that walked the scope chain comparing name strings.

The fix was already in the file, three times over. String globals, opaque
globals and web globals are each read **once at the entry block** and reused —
"reuse it rather than calling the shim each iteration". Numbers were the one
kind that never got it, and they are the cheapest kind to hold.

**Not unconditional, which is why it was not simply an oversight.** Unlike the
other three, a numeric global can be *written* from compiled code — `Op::StoreName`
compiles, for "a counter, a cache, an id sequence". Hoisting one of those would
answer with the value it held at entry, forever. So a name stored anywhere in
the group keeps its per-read call, and the stored set is collected across every
plan in the group, not just the function's own — a store inside an inlined
callee has to count, or the caller reading it back would see a stale value.

| | before | after | |
|---|---:|---:|---|
| `bench/cli/csv` | 113.6ms | 71.5ms | **1.59×** |

Six alternating pairs, not one overlapping, checksum `342272` throughout. The
other five workloads are unmoved, which is the expected shape: none of them
reads a module-level number in a hot loop. Against node, `csv` closes from 9.8×
to 5.5×, and Tier 1's own ratio on it goes from 2.3× to **4.4×** (331.8ms
interpreted).

The residual hazard, stated plainly: a global written by something this group
calls but did not inline would not be seen. That is the same exposure the string,
opaque and web hoists have carried since they were written, and the numeric case
narrows it further by excluding every name the group itself stores.

`tests/jit/global-nums.mersey` guards it, and asserts the **count** of compiled
functions as well as the answers — the per-read call is correct, only slow, so a
change that quietly stopped hoisting would pass every assertion about output.
That is the same reason `grow-param` exists, and the same class of regression
that cost `reconcile` 10% for four commits.

### After the hoist: what `csv` is now made of

Same measurement, 2699 samples:

| symbol | before | after |
|---|---:|---:|
| `env_get` | 13.9% | **4.1%** |
| `memcmp` | 7.5% | — |
| `jit_global_num` + `heap::global_num` | 11.0% | — |
| `vm::exec` | 12.9% | 20.7% |
| `heap::str_join` | — | 7.7% |
| `memmove` | 5.5% | 6.3% |
| `drop_glue<Value>` | 6.6% | 6.6% |
| `Value::clone` | 6.3% | 6.3% |
| `mi_malloc_aligned` | 2.7% | 4.1% |

Global resolution went from about a third of the workload to 4%, and the name
comparison under it is gone from the profile entirely. What was hiding behind it
is now visible, and it is the thing the workload was chosen for.

**1. Building a string one character at a time — ~18%.** `str_join` 7.7%,
`memmove` 6.3%, `mi_malloc_aligned` 4.1%. `parse` accumulates a field with
`field = ${field}${c}`, and every one of those allocates a fresh `Vec<u16>` at
exactly the needed size and copies the whole field into it. That is quadratic in
the field's length and one allocation per character. Every JS engine avoids it
with a rope (V8's `ConsString`), which is why node runs this workload in a
quarter of the time with the same algorithm.

The cheaper answer than a rope, and the one that fits this engine: **append in
place.** When a template's first part is the slot the result is being stored
back into, the existing buffer can be extended rather than replaced — and
`Vec`'s geometric growth turns the quadratic into linear.

Two hazards, both already answered by machinery that exists:

- *Another local holds the same string.* It cannot hold it unnoticed:
  `Prov::FromSlot` marks a value borrowed from a re-assignable local, and
  `clone_at` already parks a **clone** of it when it is stored, so the aliasing
  copy owns its own arena reference. `Rc::get_mut` therefore sees a count above
  one and declines — the check is sufficient, not merely a heuristic.
- *A live borrow on the operand stack that was never stored.* Restricting the
  pattern to a `TemplateJoin` immediately followed by a `StoreSlot` to the same
  slot keeps it at statement level, where the operand stack below it cannot hold
  a borrow of that slot.

So: a new shim that takes the slot's arena handle, tries `Rc::get_mut`, extends
in place when it is unique and copies when it is not.

**2. `vm::exec` at 20.7%, which is one refused function.** `parse` (208 ops),
`quoteField` (70) and `stringify` (59) all compile; the refusal is the harness
loop, on `grid.length` where `grid = parse(text)`.

`sig_of` recovers a declared return type's element only for `ArrayOf(Named)`
resolving to a class — a `Row[]` return becomes `Ty::ObjArr`. Nothing recovers
`string[]` (which `Ty::StrArr` already describes exactly, and which is a
one-line fix) or `string[][]` (which would need an opaque-with-`Elem::StrArr`
entry the lattice does not have). `std:csv`'s whole interface is the second one,
so *every* caller of `parse` hands Tier 1 a bare opaque.

Worth knowing before building it: the refused function here is the benchmark's
own harness. Consuming a `string[][]` from `parse` is what real code does, so
the gap is real — but a program that did more per row would spend proportionally
less of itself in this loop than this one does.

### Building a string a piece at a time, in place

`str_join` sizes a fresh `Vec<u16>` exactly and copies every part into it. For
`s = ${s}x` that is a full copy of `s` per append — quadratic in the string's own
length, and one allocation per piece. The fix is to extend the existing buffer,
which `Vec`'s geometric growth makes linear.

| | before | after | |
|---|---:|---:|---|
| a builder loop (400 appends × 2000) | 45.2ms | 15.0ms | **3.01×** |
| `bench/cli/csv` | 76.0ms | 71.3ms | 6.3% |

The gap between those two rows is the honest part. `csv` accumulates *fields*,
which are ten or twenty characters, so its quadratic factor was always small —
the profile's 18% was `str_join` being called constantly, not any single call
being slow. The 3× is what the change is actually worth, and it needs a string
long enough for the copying to dominate.

**Where the safety comes from.** Three things can make extending in place wrong,
and each is caught in a different place, which is why none of them is a matter of
judgement:

1. *Another local holds the buffer.* `Prov::FromSlot` plus `clone_at` already
   park a **clone** whenever a borrow of a slot is stored, so an aliasing local
   owns its own arena reference — `Rc::get_mut` sees a count above one and the
   shim copies. The count is exact here, not a proxy.
2. *The same slot appears twice in one template* — `s = ${s}${s}`. No refcount
   can catch this: it is one borrow read twice, so the count is still one while
   the second part points into the buffer the first is about to reallocate. The
   **analysis** rejects it, and that is the only place that can.
3. *Something non-empty precedes the base.* `s = <${s}>` cannot be expressed as
   an append. The **shim** checks it at run time, because whether a part is empty
   is not known until then — and it matters that this is a fallback rather than a
   refusal, since a template that opens with its interpolation renders a leading
   empty literal and would otherwise never qualify.

That last one is also why the base is not simply part 0. `` `${s}x` `` compiles to
*three* parts — an empty literal, the slot, then `"x"` — so the analysis records
the base's index and the shim skips what precedes it.

**The handle discipline.** The result comes back under a *new* handle holding a
clone of the same `Rc`, never under the base's own. The `StoreSlot` that follows
releases the slot's old handle; returning that handle would free the entry just
extended. The clone takes the count to two and the release takes it back to one,
and the buffer itself is never copied.

`strings` is unmoved, which is worth recording rather than explaining away: its
work is `charAt`, `slice` and comparison, not accumulation, so there is nothing
here for it.

## `path`: everything compiles, and it is still 7.6× off

`std:path` against `node:path` — 233.1ms against 30.5ms, checksums agreeing at
468884 on the first run across 20000 paths.

Tier 1 cannot be the answer here, and that is what makes this workload
different from the others: **9 functions compiled, 0 refused**, and the JIT is
worth only 1.23× (286.2ms interpreted). There is no refusal to fix and no
uncompiled hot function to reach. The time is in what compiled code *calls*.

The profile is flat, which is the finding rather than an obstacle to it:

| symbol | share |
|---|---:|
| `vm::exec` | 9.2% |
| `drop_glue<Value>` | 8.4% |
| `mi_malloc_aligned` | 6.6% |
| `memmove` | 6.3% |
| `GcCell` drop | 6.2% |
| `mi_free` | 3.8% |
| `Value::clone` | 3.6% |
| `Arena::keep` | 3.1% |
| `Arena::release` | 1.9% |

About a third is allocating and destroying short-lived values, with no single
hot spot. `normalize` is the shape: `p.split("/")` allocates an array, one
string per segment, and a GC cell for the array; `out.push` grows it; `out.join`
builds another string. Eight or so allocations per call, all dead by the end of
it.

### The obvious library fix was a pessimisation

`normalize` popped its stack by copying — `out = out.slice(0, out.length - 1)`,
a whole new array per `..`. `pop` exists on the engine's arrays and no std
module used it, so it looked like a free win.

**It was 6% slower**: ~250ms against ~235ms, checksum unchanged. `pop` is not in
Tier 1's subset, so `CallMethod` refused it — and took `normalize`, `join` and
`relative` with it. Coverage went from 9 compiled / 0 refused to **6 / 3**, and
three compiled functions were worth more than the array copy they were spent on.

Reverted. Two things worth keeping from it:

- **A change to `std/` is a change to compiled code.** The refusal tool
  (`tools/jit-refusals.mjs`) belongs in the loop for any edit to a standard
  library module, not only for engine work. Nothing else would have said why
  this got slower.
- Even had `pop` compiled, the saving is *one* array allocation per call, which
  four separate measurements in this session put below the floor where anything
  is visible. The library was not the problem; the eight allocations around it
  are, and they are a property of how strings and arrays are held.

That is now the third distinct workload pointing at the same thing — `strings`,
the `csv` append, and this.

## The primitives, one at a time

The per-operation breakdown that found the `string[]` return, taken one level
down: each primitive in its own function, warmed past the tier threshold, timed
against the same loop in node. Identical checksums.

**The first version of this measured nothing.** Every loop was at module top
level, which Tier 1 never sees, so it compared the Mersey *interpreter* against
node's JIT and reported figures up to 49×. A benchmark of the engine has to be
inside a function, and the number that gave it away was `"".length` at 64ns.

| primitive | mersey | node | |
|---|---:|---:|---:|
| `slice` | 1.27 | 1.97 | 0.6× |
| `lastIndexOf` | 3.62 | 4.85 | 0.7× |
| `startsWith` | 2.58 | 2.74 | 0.9× |
| `charAt` | 1.27 | 0.98 | 1.3× |
| `length` | 1.37 | 0.98 | 1.4× |
| `template` | 9.30 | 4.40 | 2.1× |
| `codePointAt` | 1.33 | 0.54 | 2.4× |
| `indexOf` | 3.14 | 1.00 | 3.1× |
| `join` | 81.30 | 22.27 | 3.7× |
| `split` | 81.70 | 14.04 | 5.8× |
| **`push`** | **55.09** | **3.79** | **14.5×** |

The shape of this is the useful part, and it is not what the workload-level gaps
suggested: **the string primitives are competitive**, three of them faster than
node's. Nothing here is 7× on its own. The workload gaps are built out of the
two rows at the bottom and out of what surrounds the primitives rather than the
primitives themselves.

`push` — allocate a small array, push twice — is 183ns against node's 12.6ns, on
**compiled** code. That is the row worth chasing, and it lines up with `path`'s
flat allocation profile.

### A module-level array refuses the function that reads it

Three rows of the table were missing because their functions were refused, all
on `LoadName`: they read a module-level `const ARR: string[]`. An array global
resolves to `NameKind::Other`, which is looked up as a *function*, is not one,
and refuses. A local array of the same contents compiles.

That is idiomatic code — a keyword list, an alphabet, a lookup table — and no
`std/` module happens to use it, which is why nothing in the arena ever noticed.

**Adding `Value::Array` to the opaque-global arm was measured and reverted.** It
does what it says: the refused function compiles (prim2 went 10/3 to 11/2, no
arena coverage moved). It is worth **nothing** — 137ms against 137ms on a loop
reading `KEYWORDS.length` two million times — because the read stays a shim
either way. Compiling the arithmetic around a shim that dominates changes
nothing, which is the fourth or fifth time this session that compiling more has
not meant going faster.

The fix that would pay is an element-*typed* array global — `Ty::StrArr` or
`Ty::ObjArr` rather than `Ty::Val`, hoisted at entry like the numeric, string and
web globals — so `.length`, `[i]` and `for…of` are native rather than shims. The
element type is available: `name_kind` already holds the `Value`, and a
`string[]` only ever holds strings, so its first element names the type. That is
a real piece of work and nothing currently measured needs it, so it is written
down rather than built.

## Array allocation, decomposed

`push` was the one primitive genuinely off, so it was taken apart. Each shape in
its own function, warmed, N=1,000,000, all five compiled (verified from the
trace, not assumed):

| | mersey | node | |
|---|---:|---:|---:|
| allocate an empty array, read `.length` | 85.9 | 3.03 | **28.3×** |
| …and push one string | 147.3 | 12.73 | 11.6× |
| …and push two | 182.4 | 12.58 | 14.5× |
| push onto an array that already exists | 100.7 | 12.06 | 8.3× |
| allocate and push two `int32` | 124.4 | 10.10 | 12.3× |

**The allocation is the cost, not the push.** An empty array is 86ns against
node's 3ns — the largest single-primitive gap measured in this arena — and the
first push adds 61ns on top.

Profiled (compiled, verified), the 86ns is not one thing:

| | share |
|---|---:|
| `Arena::keep` + `Arena::release` | 15.7% |
| thread-local access + `_tlv_get_addr` + `YOUNG` | ~17% |
| shim plumbing (`array_new`, `jit_array_new`, `new_array`) | ~14% |
| `mi_malloc` + `mi_free` + `Rc` | ~13% |
| GC tracking (`track_array`, `GcCell` drop) | ~9% |
| `val_len` — the `.length` read | 4.8% |

Nothing dominates. Allocating an array does five separate pieces of bookkeeping:
allocate the `Rc<GcCell<Vec>>`, register a `Weak` in the young list, bump the
allocation counter, park it in the arena for compiled code, and release it
again.

### Consolidating the GC's thread-locals: neutral

`register` runs on every allocation and reached three thread-locals — the young
list, the prune threshold nested inside it, the allocation counter. Folding them
into one `Nursery` struct behind a single lookup (with zero-sized key handles so
the cold call sites stayed as they were) **did exactly what it claimed and
bought nothing**: `_tlv_get_addr` disappears from the profile entirely, and the
time does not move — `alloc2` 182.4 against 181.8, and every arena workload
within noise.

Reverted. Worth keeping is why it failed, because it was not the usual "below
the floor" answer: the work really was removed, and the workload is bound
elsewhere. `Arena::keep` and `Arena::release` become the top two rows afterwards.

### What this actually says

The gap is architectural rather than a missing optimisation. V8 allocates a
young object by bumping a pointer into a nursery and registers nothing; this
engine does a refcounted heap allocation plus four pieces of bookkeeping. No
single one of them is wrong, and removing any one of them — as the thread-local
attempt showed — leaves the other four.

That is a much better-founded target than "how strings are held", which three
workload-level profiles pointed at and the primitive table then contradicted:
the string primitives are competitive, and it is the *container* allocation path
that is 28×.

## A leak in compiled code, found by the memory column

`bench/cli`'s refresh put `path` at **27 MiB peak RSS against 6–10 for every
other workload**. Memory is this engine's clearest advantage over the JS
runtimes, so a workload breaking the pattern by 3× is worth more attention than
a workload that is merely slow.

It was not overhead. RSS grew **linearly with N** — 13.8, 27.3, 83.9, 227 MiB at
N=5k, 20k, 80k, 200k — in a loop that discards everything it makes. `csv`,
`strings` and `url` are all flat at the same test, so it was specific rather
than structural, and `MERSEY_JIT=0` was flat too: **the leak was in Tier 1.**

Localised by bisecting the workload rather than reading the code — each `path`
operation alone, then each construct inside `normalize`, then each primitive.
Everything was flat in isolation (`split`, `push`, `join`, `for…of`, array
`slice`, array reassignment, a template return) until the one construct none of
those covered:

    out[out.length - 1] != ".."

**Reading an element out of a `string[]` and comparing it.** The element arrives
from the shim owning its own arena entry — deliberately, so that it survives the
container being reallocated — and the comparison then dropped both handles on
the floor:

    (SlotV::Str(ap, al, _), SlotV::Str(bp, bl, _)) => { … }

A comparison *consumes* its operands, so an owning one has to give its entry
back. Almost every string compared is a borrow whose handle is 0, which is why
this survived: the release is a no-op for all of them, and the one case that is
not is an element read.

`out.push(t[j])` is the same mistake in the same shape — `ArrayPush1` copies the
value into the array and discarded the source's handle. Both are fixed, and both
releases go *after* the copy, since the copy reads the buffer the handle names.

| | before | after |
|---|---:|---:|
| `normalize`, N=20k → 200k | 9.7 → 21.8 MiB | 8.2 → **8.2 MiB** |
| `bench/cli/path`, N=200k | 227 MiB | 71.7 MiB |

Checksums unchanged everywhere, and the times did not move: an arena slot is a
`Vec` index, so leaking one costs memory rather than speed.

### Not finished

`path` is **still linear** — 16.2 MiB at N=20k against 71.7 at 200k — so a third
site remains. It is reachable from `out.push(src[j])` where the array is a
`string[]` local, which does *not* go through `ArrayPush1`: that path is
compiled (2 functions, 0 refused) and flat under `MERSEY_JIT=0`, so it is a
third place where an owned handle is dropped.

**And there is no regression guard yet.** A tier-agreement test cannot catch
this: every answer was always correct, because a leaked arena slot is memory,
not meaning. `gc.stats().live` cannot see it either — the arena is cleared when
the outermost compiled call returns, so the growth exists only *during* the
call, as peak RSS. `bench/cli/run.mjs` already measures peak RSS per workload
and does not assert on it; that is the shape the guard should take.

### The third site, and the leak closed

`box_arg` — the path a method *argument* takes. `out.push(src[j])` on a
`string[]` local does not compile to `ArrayPush1` at all; it is an ordinary
`CallMethod`, and its arguments go through:

    SlotV::Str(ptr, len, have) => {
        let c = b.ins().call(shim.box_str, …, have);
        b.inst_results(c)[0]          // `have` is never released
    }

The comment above it was right about what `box_str` *parks* — that is either a
handle the interpreter keeps in a bounded memo, or the string's own entry handed
straight back, and neither is the caller's to free. What it missed is that the
**argument itself** is consumed here. `have` is 0 for the borrow that almost
every string argument is, so releasing it is a no-op; the one that owns is an
element read out of an array. The object arm had the same omission.

| | before | after |
|---|---:|---:|
| `bench/cli/path`, N=20k → 200k | 27.3 → 227 MiB | **9.86 → 9.86 MiB** |

Flat, and in line with every other workload (6–10 MiB). Checksums unchanged
across the arena, times unmoved.

### Why there is still no regression guard

An attempt is recorded here because it failed in an instructive way. `Arena`'s
`slots` never shortens, so its length is a high-water mark that survives the
call — exactly what is needed, since the arena is *cleared* when the outermost
compiled call returns and any count taken afterwards sees nothing. An accessor
plus a test in `crates/mersey_interp/tests/gc.rs` looked right and **passed with
the fix reverted**.

The peak was **zero**: `mersey_interp`'s own test crate has no Tier 1 wired in,
so the program ran interpreted and the arena was never touched. The same mistake
as timing a benchmark loop at module top level, and caught the same way — by
checking that the number moves when the thing under test is removed.

Both were reverted rather than kept as decoration. The guard has to live where
the JIT actually runs, which is `crates/mersey_cli/tests/`, and that means the
binary has to *report* its arena high-water mark — an env-gated line on stderr —
because those tests see a subprocess's output and nothing else.

## `join` went through UTF-8 and did not need to

`join` was 3.7× node in the primitive table, second only to `push`. Profiled on
its own, the reason was not allocation:

| symbol | share |
|---|---:|
| `utf16_to_utf8_bytes` | 10.4% |
| `str::from_utf8` | 7.7% |
| `join_generic_copy` | 5.5% |
| `utf16_to_string` | 4.8% |
| `Interp::display` | 3.1% |
| `Vec<String>` growth | 2.6% |

**Over a third of it was transcoding.** The implementation converted every
element to a Rust `String`, joined those with Rust's `[String]::join`, and
converted the answer back. The engine holds both the parts and the result as
UTF-16, so none of that was needed — and it cloned the whole `Vec<Value>` first
as well.

An all-strings fast path builds the result directly: sum the lengths, allocate
one `Vec<u16>` at exactly that size, copy the units in. It also borrows the array
rather than cloning it, which the general path cannot do — `display` can run
Mersey code (an instance's `toString`) and so can mutate the array underneath.

| | before | after | |
|---|---:|---:|---|
| `join` alone | 81.3 | 44.0 | **1.85×** |
| `bench/cli/path` | 141.3 | 130.8 | 7.4% |
| `bench/cli/csv` | 67.7 | 65.1 | 3.9% |

Four alternating pairs on each workload, none overlapping, checksums unchanged.

`split` was checked for the same mistake and does not make it — it already works
in code units. Its 5.8× is the array-allocation gap, which is where the profile
of `push` also ended up.

Both paths are pinned in `tests/mersey/collections.test.mersey`: empty, single,
empty separator, multi-unit separator, non-BMP elements (the units have to be
copied as units, not re-encoded), empty strings as elements (a string, not a
missing one), and numbers to reach the `display` path. Inverting the
separator guard in the fast path fails 13 tests, so they do discriminate.

### The allocation model, probed twice

Two independent attempts to remove one of array allocation's five costs, both
measured as ceilings before building anything:

**The GC thread-locals.** `register` runs on every allocation and reached three
of them. Folded into one lookup behind a single `Nursery` — `_tlv_get_addr`
disappeared from the profile and the time did not move at all.

**GC tracking itself.** Deleting `track_array` outright — unsound, purely to see
the ceiling — is worth **7–10%** of allocation: `alloc` 87.1 → 79.0ms, `alloc2`
184.4 → 171.1, `intpush` 123.3 → 110.8. A *sound* version (skipping registration
only for arrays whose declared element type cannot hold a reference, so cannot
be in a cycle) would get some fraction of that, on one of five costs, in a path
that is itself perhaps a quarter of the workloads that are slow. Call it 2% and
it is not worth the correctness surface.

That is now two probes agreeing with the conclusion the first profile suggested:
**the gap is the allocation model, not any of its parts.** Allocating an array
here is a refcounted heap allocation plus four pieces of bookkeeping — arena
park, arena release, GC registration, GC drop — and V8 does the equivalent by
bumping a pointer into a nursery and registering nothing. Removing any single
piece leaves the other four, which is exactly what both probes measured.

The next real win on the four container-bound workloads is a different
allocation model, not a cheaper version of this one. That is a design change
rather than an optimisation, and it is written down here rather than started.

### `toUpperCase` is not an outlier

The primitive table put it at 25.5×, which was wrong: the receiver was a module
constant and V8 hoisted the whole call out of the loop. Against a receiver built
per iteration it is **3.5×** (49.3ms against 14.0), which is the band the other
real-work methods sit in.

Left below the transcode deliberately. `str::to_uppercase` implements
context-sensitive rules — Greek final sigma, ß → SS — that a per-character
`char::to_uppercase` does not, so a units version would be a *behaviour* change
rather than the same answer faster. The rest of the survey moved because those
methods were provably equivalent; this one is not.
