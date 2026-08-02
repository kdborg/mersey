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
