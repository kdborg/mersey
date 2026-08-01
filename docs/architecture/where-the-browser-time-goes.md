# Where the browser time goes

Measured on 2026-07-31, macOS arm64, the Chromium fork against the same
browser's own JavaScript. Every row is the same workload written twice —
line for line, same checksum — so the ratio is the language boundary and
nothing else.

## The shape of it

| | ratio to Chromium's JS |
|---|---|
| `crypto` | **0.45x** (Mersey 2.2x faster) |
| `calls` | **0.80x** (faster) |
| `fcompute`, `mathk`, `compute` | 1.05–1.07x (parity) |
| `storage` | 1.30x |
| `encoding`, `blob`, `query`, `json` | 2.3–2.6x |
| `xhr`, `fetch`, `websocket`, `canvas`, `url` | 2.9–3.3x |
| `bchannel`, `idb`, `worker`, `timers`, `sse`, `events`, `dom` | 4.4–6.9x |
| `locks`, `cssom`, `compression`, `urlpattern` | 8.8–14.9x |
| `frameworkui2`, `msgchannel`, `geometry`, `streams` | **20–61x** |

The line is not subtle and it is not about the engine. **Compute is at
parity or ahead. Everything that crosses to the host is behind, in
proportion to how often it crosses.** Tier 1 compiles `compute` to machine
code that matches V8's; the same tier cannot help `geometry`, because
`geometry` spends its time leaving the engine.

`geometry` is the clearest case. Per iteration it does `new DOMMatrix()`,
`.translate(x, y)`, `.scale(2)`, and reads `m41`, `m42`, `a` — six
crossings. It took 79.7ms for 10,000 iterations against Chromium JS's
2.3ms, which is about **1.3µs per crossing**.

**That number has been chased, and the diagnosis below was wrong.** See
"What actually cost the 35x" at the end of this file: `geometry` is now
**23.8–26.0ms**, a 3.1x improvement on the same checksum, and it needed no
`web_bind` implementation at all.

## Why a crossing costs that

The bridge has tiers of decreasing cost (see `browser-integration.md`).
What the Chromium fork actually fills in today:

    web_global  web_get  web_set  web_call  web_new  web_release
    web_intern  web_get_u16  web_set_u16  web_call_u16  web_new_u16
    web_apply

and what it leaves NULL:

    web_bind         web_get_id      web_set_num   web_set_str
    web_call_str     web_call_scalars  web_new_scalars
    web_instanceof   web_iterate     web_bytes_read/write

So a numeric method call lands on `web_call_u16`. That is *not* the JSON
path — `msy_arg16` carries a `double`, so numbers cross as numbers and
nothing is stringified. The interned-id scalar tier (`web_call_scalars`
and friends) would therefore buy little here: it saves a name lookup the
u16 tier has already interned away.

What is left is **entering V8**. `HostWebCallU16Shim` has to take a context
scope, wrap the target handle as a `v8::Object`, convert each argument to a
`v8::Value`, do a property lookup, call through V8, and convert the reply
back. Around a microsecond is an honest price for that, and it is paid six
times per `geometry` iteration.

## What would change it

`web_bind` — the typed tier — is the one that matters, and it is the one
not implemented. It exists so that compiled code can call a *bound C++
function* directly: no context scope, no `v8::Value` conversion, no
property lookup. For `DOMMatrix.translate` that is a call into Blink's own
`DOMMatrix::translate` with two doubles.

That is per-interface work in each fork, and it is the whole remaining
browser gap. It is worth scoping deliberately rather than starting at the
edges: the interfaces to bind first are the ones this table already names —
`DOMMatrix` (geometry), `MessagePort` (msgchannel), the streams reader, and
whatever `frameworkui2`'s reconciler touches per row.

## What actually cost the 35x

`DOMMatrix` was not reaching the wide tier. It was not that the typed tier
was missing — it was that the fork answered `translate`, `scale`, the three
reads and the constructor from `HostWebCall`, the **JSON** path. Two doubles
became a serialised JSON array, which was immediately parsed back, for a
call into Blink's geometry code that does almost no work, and the reply went
home as a JSON string the engine parsed again. Six of those per iteration is
the 1.3µs.

The repair was to intern the six names and give them cases on the tier that
already carries doubles (`web_call_u16` / `web_get_u16` / `web_new_u16`).
Measured on the same binary, same checksum (95000):

| | ms, n=3 |
|---|---|
| before | 79.68 |
| after | 25.95, 23.81 |

`canvas` is the control and did not move (5.59 before, 5.92 / 5.68 after).

**Interning a name is a commitment, not a hint.** `read_msy_reply` always
answers `Some`, so a `FillNull` on the wide tier is the call's *value* — the
engine takes it and does not retry the JSON path. `translate` and `scale`
are canvas-context methods as well as matrix ones, so interning them without
covering that receiver would have turned `ctx.translate(x, y)` into a silent
no-op. No workload here uses canvas transforms, so nothing in this suite
would have caught it. `CallViaJson` and `GetViaJson` exist for that: a case
that has interned a name but cannot answer for *this* receiver falls to the
JSON tier rather than to null. `a` needed the same care on the read side,
being a name anything may carry.

### `break` in that switch means "handled", not "unhandled"

Applying the same treatment to `msgchannel` and `streams` needed one more thing.
Their hot names — `postMessage`, `data`, `read`, `value`, `done` — are not
interned by the fork, and an un-interned name declines at `HostWebIntern` and
cannot use the wide tier at all, so it goes out as JSON in both directions.
Neither interface has a native kind, so both are reflective `kJs`: interning the
names alone is enough to route them through V8 directly.

But interning `data` is not safe on its own, because `kEvent` *is* a native kind
with no case for it, and an unanswered case returns null that the engine takes as
the value. So the tier needs a fallback — and the obvious placement of it is
wrong. Putting it after the switch cost **13x on `canvas`**:

    case kFillRect: {
      if (…) { fill(…); }     // does the work
      break;                  // …and breaks. `break` meant "handled, no value".
    }

Several cases are written that way — `kFillRect`, `kAppendChild`, `kSetItem` —
so a fallback after the switch runs the effect **a second time**. Every rectangle
drawn twice, every storage key set twice. Measured: `canvas` 5.7 → 77.6ms, `dom`
15.9 → 76.2, `storage` 137.5 → 314.8.

**All thirty checksums stayed green through that.** These workloads do not read
back what they write, so doing it twice is invisible to the correctness proof.
Only the timings showed it, which is the argument for running the whole suite
against a snapshot rather than the workloads you meant to touch.

The fallback belongs in `default:`, where it catches an id with *no case* — which
can only be a name this fork interned deliberately — and leaves `break` meaning
what every existing case already assumed. Results after that:

| | before | after |
|---|---|---|
| `streams` | 90.78 | **66.87** (−26%) |
| `frameworkui2` | 49.90 | 43.59 (−13%, at its 12% noise floor) |
| `msgchannel` | 51.94 | 48.56 (−6%) |
| `canvas` / `dom` / `storage` | 5.68 / 15.93 / 137.50 | 5.90 / 16.16 / 138.50 |

That left one thing open, and the audit closed it. Three cases —
`kAppendChild`, `kSetItem`, `kFillRect` — did their work and then broke; every
other case already answered the way `kSetProperty` does, with `FillNull` and a
`return`. Making those three match means **`break` now says one thing
everywhere: this case could not answer for this receiver.** So the fallback
serves all of them, not just ids with no case, and `kFillRect` on a non-canvas
reaches the JSON tier instead of answering null and being believed.

All thirty checksums are unchanged by that, which is worth saying explicitly: no
receiver mismatch was silently answering null in a way any workload here could
see. The value is that one cannot start to.

### The callback direction had the same disease

Recomputing the ratios after the tier work put `streams` at **49x** its
JavaScript twin and `msgchannel` at **27x**, with JS twins of 1.5 and 1.9ms — so
almost all of Mersey's 75 and 52ms was overhead. But not tier overhead: their
names were interned by then. It was the *other* direction.

`msy_context_invoke_args` took a JSON string. The fork built a `JSONArray` from
a callback's arguments, serialised it, and the engine parsed it back — after
already reducing every argument to a scalar or a handle. The string carried
nothing the typed form does not, and every promise in an async workload paid for
it. So `msy_context_invoke16` (ABI v11), the typed twin, with
`Interp::invoke_callback_args` underneath both; the JSON door remains and
remains correct.

Measured as a **rebuilt A/B on one quiet machine state** — same engine binary,
only the fork's call site differing:

| | JSON | typed | |
|---|---|---|---|
| `streams` | 72.1 | **61.1** | −15.3% |
| `sse` | 39.0 | **34.1** | −12.6% |
| `msgchannel` | 50.5 | **46.0** | −8.9% |
| `timers` | 44.6 | 45.7 | +2.5%, inside its 14% spread |

Two traps, both of which produce wrong arguments rather than crashes:

- **`msy_arg16` borrows its strings.** They must outlive the call, and the
  vector holding them must have stopped reallocating before any pointer into it
  is taken.
- **A Blink `String` may be 8-bit.** Asking a Latin-1 string for
  `Characters16()` yields garbage units, not a failure. Index and widen.

And one that only cost time: the first version heap-allocated three vectors per
callback, which made *zero-argument* callbacks — every `setTimeout` — slower
than the JSON they replaced. `timers` measured +35% until those vectors got
inline capacity. A typed path is only cheaper than building JSON if it does not
allocate to say "no arguments".

The lesson generalises past `DOMMatrix`. Before writing `web_bind` for an
interface, check which tier its calls are actually landing on — the rows at
20–61x above (`msgchannel`, `streams`, `frameworkui2`) are worth that check
first, because a JSON round trip and a missing typed binding look identical
from the outside and cost very differently to fix.

## What this table is not

Read the small numbers with `bench/web/README.md`'s noise section in hand:
the async and IPC-shaped workloads move 7–32% run to run on the same
binary. `streams` at 61x and `geometry` at 35x are far outside that and are
real. A row at 1.05x is parity, not a win or a loss.


## The other gap: the polyfill has no compile tier

Everything above is about the *fork*, where Tier 1 runs. The polyfill — the
WASM engine in a stock browser, which is what anyone gets without a custom
build — has no compile tier at all, and on compute-shaped work that is the
larger number by far.

Measured the same day, `compute`:

| | ms | vs the browser's JS |
|---|---|---|
| Chromium's own JS | 86.7 | 1x |
| native, Tier 1 on | **87.3** | **1.01x — parity** |
| native, Tier 1 off | 4448 | 51x |
| WASM polyfill (no Tier 1) | 12670 | 146x |

The decomposition is the point. Of the polyfill's 146x, **51x is having no
compile tier** and only **2.85x is WASM itself**. Tier 1 is worth 51x on
this workload; the cost of running the engine as WASM rather than natively
is small beside it. A polyfill that could compile would be at roughly 250ms
— still behind V8, but fifty times closer.

`web-platform.md` gives the reason as "WASM cannot map code pages". That is
true of WASM itself and not quite the whole story: the *host* can compile
generated bytes into a `WebAssembly.Module` at run time, and a generated
module can import the engine's own `Memory`, so it would share the heap
rather than copy across it. Whether that is worth building is a genuinely
open question and this note does not answer it — the constraints are real
(browsers block synchronous compilation of modules over a few KB on the
main thread, and each module costs instantiation latency that a per-function
tier would pay constantly). It is recorded because the 51x is the largest
single performance fact in the project and the stated reason for accepting
it is narrower than it looks.

Note also what the table does *not* say. Tier 1 already reaches parity with
V8 on compute natively, and the fork inherits that (`compute` 1.07x there).
So this is a Stage A question only.
