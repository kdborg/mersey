# Where the string time goes

Of the nine CLI benchmarks, Mersey wins or ties eight. It loses one, and loses it
badly. This is an account of why, written so the next attempt starts from the
measurement rather than from the same guesses.

Steady-state kernel time, `bench/cli`, five repeats, arm64 macOS:

| workload | Node | Bun | Deno | **Mersey** |
|---|---|---|---|---|
| compute | 89.0 | 97.0 | 98.4 | **88.9** |
| calls | 121.6 | 31.0 | 29.1 | **22.5** |
| json | 4.1 | 1.3 | 1.5 | **0.7** |
| encoding | 4.7 | 1.9 | 4.4 | **1.5** |
| crypto | 19.5 | 0.5 | 1.5 | **0.5** |
| **strings** | 27.8 | 15.1 | 15.7 | **58.6** |

`strings` is semver-shaped parsing — `indexOf`, `slice`, `split`, `codePointAt`
— and Mersey is **3.9x** Bun on it. Peak RSS is now the lowest of the four
(8.4 MiB against 39–52), so this is a speed question and not a memory one.

## What the profile says

`sample` over a 3M-iteration run of the same workload, self time, 2116 samples:

| | share | what |
|---|---|---|
| compiled code | ~8% | the loop itself |
| `GcCell<Vec<Value>>` drop glue | ~8% | releasing `split` results |
| `mi_malloc_aligned` + `mi_free` + `mi_bin` | ~8% | the allocator proper |
| `mach_absolute_time` | 1.7% | mimalloc's purge clock, inside `_mi_arenas_collect` |
| `_tlv_get_addr` | 1.5% | mimalloc's thread-heap TLS lookup on macOS |
| `_platform_memmove` / `memcmp` | 3% | copying and comparing units |

So roughly **a sixth of the run is allocation and the drop glue that feeds it**,
on a workload whose strings are almost all under eight code units. That is the
shape to attack, and the copying is not it: the strings are too short for the
copy to matter next to the trip through the allocator.

Two things measured and **rejected**:

- **mimalloc purge off** (`MIMALLOC_PURGE_DELAY=-1`, with and without
  `ARENA_EAGER_COMMIT`). It does remove the `clock_gettime` calls, and it is
  worth about 2% — inside the run-to-run noise. Not the lever.
- **Compiled-frame retention**, which was a real bug and is fixed (see
  `jit-coverage.md`), but was costing memory rather than time: 89 MB → 8.8 MB,
  and about 8% *slower* afterwards, because freeing 900k entries as you go is
  work that retaining them defers.

## Why the obvious fix is not available

A Mersey string is `Rc<Vec<u16>>`. That is **two** allocations — the `Rc` box
holding the `Vec` header, and the `Vec`'s own buffer — and two indirections on
every read. A one-unit string costs two trips through the allocator. Collapsing
that to one block (refcount, length, units, contiguous) would roughly halve the
allocation count on this workload.

It cannot be written here. Two constraints meet:

- **`unsafe_code = "forbid"`** in `[workspace.lints.rust]`. `forbid` is not
  something an inner `allow` can lift, and a thin refcounted variable-length
  block needs `unsafe` — there is no safe Rust spelling of it.
- **The pointer has to stay thin.** The safe one-allocation answer is
  `Rc<[u16]>`, but that is a fat pointer: it would take `Value` from 16 bytes to
  24, and `Value`'s size is the stride compiled code computes every field address
  with (16 is a shift, 24 is a multiply) and is multiplied by every field of every
  object and every element of every array. Worse, the JIT passes a string into a
  compiled frame as *one word* — `Rc::as_ptr`, from which the entry wrapper
  derives data and length — and a fat pointer does not fit that at all without
  reaching into `RcBox`'s unstable layout, which is `unsafe` again.

So the representation is fixed by a policy decision plus a codegen one. Both are
defensible; together they close this route. Anyone wanting to reopen it is asking
for `unsafe` in a confined string module, and should say so explicitly and price
it: the payoff measured here is on the order of 6–8% of this workload, not a
multiple.

## What is left

Allocate fewer strings, rather than making a string cheaper.

The idiom this workload is built from is `for (const x of s.split(sep))`, which
allocates an array *and* a `Value::Str` per part, iterates them once, and drops
the lot. Nothing escapes the loop. Lowering that pattern to an iteration over
spans — no array, no per-part value — removes the whole allocation for a shape
that is everywhere in parsing code. That is the next thing to try, and unlike the
representation change it is a Tier 1 pattern match rather than an engine-wide
rewrite.

`split` used with an index (`parts[k]`, which this workload also does) is the
harder half and would still allocate.
