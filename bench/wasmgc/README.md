# WasmGC probes

Five hand-written WebAssembly-GC modules, each paired with the JavaScript a
transpiler would emit for the same work, measured against each other in real
browsers. They exist to answer one question — *what would a Mersey → WasmGC
backend be worth?* — before writing one.

The answer is in
[`docs/architecture/what-wasmgc-would-buy.md`](../../docs/architecture/what-wasmgc-would-buy.md):
not enough. These are kept because the question recurs, and because the
measurement traps they hit apply to every benchmark in this repository.

## Running

```bash
npm install binaryen        # provides wasm-as
./build.sh                  # probes/*.wat -> probes/*.wasm
node run.mjs chrome         # also: firefox, safari
node run.mjs safari 4       # scale: see below
```

Driverless and headless where possible, deliberately. Playwright drives Firefox
with the JS debugger attached, and SpiderMonkey baseline-compiles *all* wasm
while debugging — 5-7× — so a driven Firefox would report fiction for a
measurement that is entirely wasm. Same reason `bench/web/run-firefox-real.mjs`
exists.

Safari has no headless mode: it is driven by AppleScript into a window of your
own Safari, and the runner closes that one window and never kills the process.

## The probes

| file | what it measures | why it is here |
|---|---|---|
| `alloc.wat` | struct allocation — transient, bounded live set, all retained | `Row`/`Entry`/`Node` in `bench/cli/reconcile` |
| `arr.wat` | a growable `i32` array, built and churned | the op buffer, `strs`, `drop`, `newRefs` |
| `map.wat` | a keyed map: set / get / iterate / delete | `Map<int32, Entry>`, the reconciler's core |
| `str.wat` | building `row <id> v<v>` and handing it to the host | the per-row template string |
| `call.wat` | a host call in a loop | four DOM crossings per `cssom` iteration |

WasmGC has no map and no growable array, so `map.wat` and `arr.wat` hand-roll
what a backend would have to emit: open addressing with linear probing and a
rehash at 0.75 load; amortised doubling with `array.copy`. It has no strings
either, so `str.wat` builds into an `(array i16)` and converts with the JS String
Builtins (`wasm:js-string`) — the practical answer since `stringref` was
withdrawn, and available in all three engines as of the measurement.

Every probe returns a checksum that its JS twin must match, the same discipline
as `bench/web` and `bench/cli`. All matched on every engine.

## Reading the numbers

`results-2026-08-09.log` is the raw log of the run the doc quotes: three repeats
per engine, on a machine with the developer's browser and game closed, gated on
a 1-minute load average below 0.9.

**Three traps this hit, all worth knowing before trusting any number here:**

- **V8 elides an allocation whose object never escapes.** The first version of
  `alloc.wat` reported 10.9×. It was measuring nothing. The probes now park each
  new struct in a mutable global or a ring buffer so the allocation must happen.
- **Firefox and Safari clamp `performance.now()` to 1 ms.** At scale 1 that
  quantized every row under ~50 ms into noise and made three rows appear to flip
  sign. Firefox's clamp is turned off in the throwaway profile; Safari needs
  scale ≥ 4.
- **A contended machine inflates a headline.** The Chrome bounded-live-set row
  read 3.44× with a video and a game running, and 1.61× on a quiet one.

And a fourth, which is why this directory does not include a Node runner: **Node
is not a browser proxy for anything crossing the wasm boundary.** It disagreed
in *sign* with Chrome on three probes and underpriced the wasm→JS call by 3-5×.
`bench/web/perf-test.mjs` gates on a Node leg, which is sound for checksums and
regression detection — but it cannot price a boundary.
