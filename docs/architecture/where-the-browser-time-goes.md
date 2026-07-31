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
crossings. It takes 79.7ms for 10,000 iterations against Chromium JS's
2.3ms, which is about **1.3µs per crossing**.

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

## What this table is not

Read the small numbers with `bench/web/README.md`'s noise section in hand:
the async and IPC-shaped workloads move 7–32% run to run on the same
binary. `streams` at 61x and `geometry` at 35x are far outside that and are
real. A row at 1.05x is parity, not a win or a loss.
