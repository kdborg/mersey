# Where the interpreter's time goes

Tier 0, profiled on its own — `MERSEY_JIT=0` over `bench/cli/reconcile` at
N=3000, `sample` for 6s, 2933 thread samples. The companion to
`where-the-cli-time-goes.md`, which profiles the tier above it.

This matters even where Tier 1 is doing well: the interpreter runs everything
before it is hot, everything Tier 1 refuses, and — in `reconcile` — the one
method that dominates the workload. `vm::exec` was 27% of that profile *with*
the JIT on.

| symbol | samples | share |
|---|---:|---:|
| `vm::exec` | 1433 | 49% |
| `drop_glue<Value>` | 350 | 12% |
| `Value::clone` | 288 | 10% |
| `memmove` | 97 | 3% |
| `Interp::call_member` | 82 | 3% |
| `mi_malloc_aligned` | 51 | 2% |
| `IndexMap::shift_remove` | 45 | 2% |
| `mi_free` | 37 | 1% |

## The 49% is not dispatch overhead

`Op` is **16 bytes** — the same as `Value`, because the three variants that
carry one (`Done`, `Await`, `Yield`) pack their discriminant into `Value`'s
niche. A 230-op method is under 4 KB of code array, comfortably L1-resident, so
there is nothing to win by shrinking it: the 49% is the op *bodies*, inlined
into the match, not the fetch or the branch.

## The 22% that is refcount churn

`Value::clone` and `drop_glue<Value>` together are 22%, and they are structural
rather than accidental. A stack VM over owned values clones on every push that
does not consume its source:

    Op::Const(ci)     => stack.push(chunk.consts[ci].clone())
    Op::LoadSlot(sl)  => stack.push(frame[base + sl].clone())

For a scalar that is a 16-byte copy. For `Value::Instance`, `Value::Str`,
`Value::Array` it is an `Rc` increment now and a decrement-and-test later.
Removing it means the stack holding borrows, which is a different interpreter.

## A method call allocated, and did not need to

The inline-cache path in `Op::CallMethod` ended with

    Rc::new(Closure { data, env, this: Some(stack[at].clone()), cls })

— a heap allocation per call. This was first written up here as needing frames
to hold `this` separately from the closure, which was wrong: **`InlineFrame`
does not keep the closure at all.** It keeps the chunk, the pc and the stack
bases. The closure is read during the push — `this` into a frame slot, `cls`
onto the class stack, `env` onto the scope stack, `data` for the name and the
OSR context — and dropped four lines later. Both `inlinable` and `jit_call`
take `&Closure`, not `Rc<Closure>`.

So it is a stack value now, and the allocation and its free are gone.

**4.8% on the interpreter**: 5261–5276ms against 5478–5573ms over five warm runs
each, non-overlapping. With Tier 1 on it is not measurable for this workload —
60.5 against 61.0 median, overlapping — because most of `reconcile` is compiled
by then and interpreted method dispatch is a smaller share of what is left.

`Op::Call` above is deliberately unchanged: the closure it inlines already
exists on the stack as a `Value::Closure`, so cloning the `Rc` allocates
nothing.

The lesson worth keeping is that the reason recorded for *not* attempting this
was a guess about `InlineFrame` that thirty seconds of reading disproved.

## What was fixed

`Op::CallMethod` took a reference to the receiver's class on every call —
`b.class.clone()`, an `Rc` bump and a matching decrement — and then used it only
if the method cache missed. The cache exists precisely so that lookup does not
happen. Now the hit path reads `class.id` and nothing else.

**Below noise**: medians 5325ms against 5340ms over four warm runs each. An
`Rc` bump is a couple of nanoseconds and a method call is not *that* frequent.
Kept because it is strictly less work on the hottest path in the interpreter and
because fetching what a cache exists to avoid fetching is worth not doing, not
because the benchmark could see it.
