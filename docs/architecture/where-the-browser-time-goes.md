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
