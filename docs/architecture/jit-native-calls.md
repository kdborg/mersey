# Tier-1 and native calls

Tier-1 refuses any function containing a `std:` native call. Not the call — the
*function*. One `bytes.encodeUtf8` in a loop body and every instruction around
it runs in the interpreter, which is why the three workloads the command-line
arena loses are the three that make one:

| workload | Mersey | best JS | with `MERSEY_JIT=0` |
| --- | --- | --- | --- |
| `url` | 21.7 ms | 7.13 (bun) | unchanged |
| `encoding` | 3.78 ms | 1.56 (bun) | unchanged |
| `crypto` | 2.93 ms | 0.45 (bun) | unchanged |

Unchanged with the JIT off is the whole diagnosis: none of them is compiled.
`compute` and `mathk`, which call no native, are 68x and 89x slower with the JIT
off — and those are the workloads Mersey wins.

A profile of `crypto`'s loop puts ~42% in `vm::exec`, ~15% in `Value` clone and
drop, ~10% in call dispatch, and only ~11% in ChaCha itself, which at ~1 GB/s is
not the problem. Ten interpreted opcodes per iteration is the problem.

## Why it bails

`analyze` (crates/mersey_jit/src/lib.rs) returns `None` for anything it cannot
type, and `None` aborts the whole chunk. A native call fails twice over: the
receiver is a namespace it does not recognise, and the result is a `Value` its
type lattice cannot hold.

## What is already in place

Three pieces of the answer exist and are load-bearing elsewhere.

**A namespace receiver is a marker, not a value.** `TSlot::MathNs` / `TimeNs` are
pushed by `Op::LoadName` when `JitEnv::is_math_ns` / `is_time_ns` says the global
is that namespace, and `Op::CallMethod` on the marker dispatches to a special
path (`math_at`, `time_at`). A `std:` namespace is the same shape.

**A shim can reach the interpreter.** `heap.rs`'s `extern "C"` shims take
`*mut Arena` and go through `(*arena).interp_ptr()` — the interpreter sets it
before entering compiled code and clears it after. `host_time` is four lines and
does exactly this. `call_native` is reachable the same way.

**The arena already owns arbitrary engine values.** `Arena` is
`slots: Vec<Option<Value>>` with `keep(v: Value) -> u64`. It does not need to
learn a new trick to hold a `Bytes`, a `Url`, or anything else a native returns —
this is the same mechanism `Ty::Web` uses for host handles, and `msy_release`
already frees them.

## What is missing

**An opaque value type.** `Ty::Val`, carried as a single arena handle, the way
`Ty::Web` carries a host object — except owned, so it needs the handle-nonzero
discipline `SlotV::Obj` and `SlotV::Str` already use (a handle lives in exactly
one place; copies carry 0). This touches the lattice, `JitSlot`, frame
marshalling and the release paths, and it is most of the work.

**The call shim.** `msy_native_call(arena, ns, name, args, argc, out) -> 0/1`,
marshalling scalars by value and opaques by handle, returning through the same
0-ok/1-threw convention `web_call_num` uses.

**A property read on an opaque.** `crypto`'s loop is
`random.fill(buf); sum + buf.length` — the `.length` is a read on a `Bytes`, so
the call shim alone compiles nothing. It needs a numeric-property shim beside it.

**A signature table.** The analysis has to know, per native, what its arguments
and result are, to decide whether it can be compiled and what to push. The
checker already holds this (`Ns::*` member lists and their `FnType`s); the JIT
needs the same facts in its own terms.

## Order to build it in

1. `Ty::Val` and `SlotV::ValRef`, plumbed through the lattice, slots, frames and
   release. Nothing uses it yet; the tests still pass.
2. `msy_native_call` plus the numeric-property shim, and the `StdNs` marker.
3. One native end to end: `random.fill` — one opaque argument, no result. That
   makes `crypto` the first compiled native-calling loop.
4. Opaque *returns* (`bytes.encodeUtf8`, `parse.url`), which is what `encoding`
   and `url` need.

Verify each step with `MERSEY_JIT=0` against the same run: if the two agree on
the answer and differ on the time, the tier is doing its job and doing it right.
The command-line arena is the place to measure — it repeats to under 1%, where
the browser legs cannot resolve better than about a third.
