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
and this tier does not. That is a feature, not a patch, and it is the only piece
of `bench/cli` with a `vm::exec` share worth spending it on.

## The rest of the profile

`Value::clone` and `drop_glue<Value>` together are 16%, which is the 16-byte
`Value` being moved in and out of containers — the same cost
`where-the-string-time-goes.md` reaches from the other direction, and not
specific to this workload. `IndexMap::shift_remove` at 3% is `nodes.remove()`
and is O(n) on purpose: a `Map` iterates in insertion order (§6.5), so the
cheap `swap_remove` would be observably wrong.
