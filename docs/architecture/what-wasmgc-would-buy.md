# What WasmGC would buy

Measured 2026-08-09, macOS arm64, against Chrome 151, Firefox 153 and Safari
26.5 — three engines, three repeats each, on a quiet machine. The probes are in
[`bench/wasmgc`](../../bench/wasmgc/); every one returns a checksum its
JavaScript twin must match, and every one matched on every engine.

**The answer is no.** The wins are small and disagree between engines; the
losses are large and universal, and they land on exactly what the browser
workloads are made of.

## The question

`bench/web` already runs Mersey three ways in a browser: the engine compiled to
wasm interpreting bytecode, the JS backend (`crates/mersey_js`, which also emits
a real-wasm compute tier via `wasmgen.rs`), and the native fork. The JS backend
is the fast one — **0.81× to 2.11× the browser's own JavaScript, median ~1.2×**,
against 1.2× to 353× for the interpreted-in-wasm leg.

So the open question was never "compile instead of interpret" — `wasmgen`
already does that for numeric kernels. It was whether a *WasmGC* backend could
close the remaining ~1.2× by giving sealed classes real struct types, static
field offsets, and no hidden-class transitions.

`where-the-cli-time-goes.md` sharpens the motive: after four allocation probes it
concludes the remaining CLI gap "is the allocation model, not any of its parts",
and that closing it is a design change rather than an optimisation. The
compiled-to-JS browser leg is evidence for the same thing from the other side —
it beats the *native* fork on allocation-heavy rows (`geometry` 1.74× against
6.91×, `streams` 1.87× against 11.73×) because it has no engine heap at all.
Handing allocation to the host GC is worth 3-6× there. WasmGC is one way to ask
for that.

## What was measured

Five primitives, chosen from what `bench/cli/reconcile` is actually built from
rather than from what WasmGC is good at.

| | Chrome 151 | Firefox 153 | Safari 26.5 |
|---|---|---|---|
| string build + handoff | **2.83× slower** ±1% | **2.01× slower** ±2% | **1.70× slower** ±9% |
| host call | **5.41× slower** ±2% | **1.78× slower** ±6% | **9.88× slower** ±12% |
| array churn | 1.39× faster ±3% | 1.34× faster ±1% | 1.11× faster ±1% |
| alloc — transient | 1.07× faster ±4% | 1.11× faster ±2% | 1.05× slower ±14% |
| alloc — all retained | 1.12× faster ±0% | 1.02× faster ±14% | 1.05× faster ±13% |
| keyed map | 1.14× slower ±8% | **1.68× faster** ±1% | 1.28× slower ±5% |
| alloc — bounded live set | **1.61× faster** ±6% | **1.66× slower** ±3% | 1.05× slower ±10% |
| array build | 1.02× faster ±5% | 1.78× faster ±10% | 1.81× faster ±8% |

± is the spread across three repeats.

### The premise is a wash

**Struct allocation — the entire reason to want WasmGC — is 1.02× to 1.12× on
every engine.** Both transient and retained. A modern JS engine allocates a
small fixed-shape object about as fast as a WasmGC struct, and Mersey's classes
are already sealed, so the JS backend was never paying for hidden-class
transitions in the first place.

This number shrank every time a measurement error was removed from it: 10.9×
(V8 eliding an allocation that never escaped) → 1.69× (escape forced, contended
machine) → **~1.1×** (quiet machine, three engines). The first two are recorded
in `bench/wasmgc/README.md` as traps, because both looked like results.

### Two losses, on every engine

**Strings, 1.70-2.83×.** WasmGC has no string type. `stringref` was withdrawn,
so a backend must build into an `(array i16)` and convert through the JS String
Builtins — which do work in all three engines, so availability is not the
constraint; cost is. Splitting it: construction alone is ~36ms of a 64ms Chrome
total, so **the handoff is roughly half**, and the handoff is the part no
cleverness removes. Mersey strings are WTF-16 and cross to the DOM constantly.

**Host calls, 1.78-9.88×.** This is the decisive one and it is structural. The
JS backend's advantage is that it has *no boundary*: it **is** JavaScript, so a
DOM call is a call. A WasmGC module reintroduces a wasm↔JS crossing on every
one. `cssom` is four crossings and two template strings per iteration; `dom`,
`streams` and `geometry` are the same shape. A WasmGC backend would make the
DOM-heavy majority of the suite *worse*.

### Three probes flip sign between engines

The keyed map is **1.68× faster on SpiderMonkey and 1.28× slower on
JavaScriptCore**, at ±1% and ±5%. Bounded-live-set allocation is 1.61× faster on
V8 and 1.66× slower on SpiderMonkey. Array build is parity on V8 and ~1.8× on
the other two.

For this project that matters more than the magnitudes. Mersey ships four
browser forks and treats bit-identical checksums across engines as the
correctness proof; a backend whose wins invert between V8 and SpiderMonkey means
every performance claim needs an engine qualifier, and tuning it means choosing
a winner.

## Conclusion

One universal win at 1.1-1.4× (array churn), two universal losses at 1.7-9.9×
(strings, host calls), the premise a wash, and half the probes engine-dependent.

**Do not build a WasmGC backend.** Do not extend `wasmgen` past its current
subset either: the only allocation shape that wins everywhere is growable-array
churn, which is not worth a lattice extension on its own — and the lattice
extensions that have twice regressed coverage invisibly are described in
`where-the-cli-time-goes.md`.

What this does *not* rule out is the underlying idea. The compiled-to-JS leg
already gets the host GC, and gets it without a boundary. If the allocation
model is the ceiling, the evidence points at the JS backend as the place that
already cleared it — not at a fourth backend.

## Method notes, which outlive the question

- **V8 elides an allocation whose object never escapes.** The first `alloc`
  probe reported 10.9× and was measuring nothing. Any allocation benchmark here
  must park the object somewhere observable.
- **Firefox and Safari clamp `performance.now()` to 1 ms.** At the original
  iteration counts this quantized every row under ~50 ms and made three rows
  appear to flip sign. Firefox's clamp is disabled in the runner's throwaway
  profile; Safari has no profile and needs the workload scaled up instead.
- **A contended machine inflates a headline.** The Chrome bounded-live-set row
  read 3.44× with a video and a game running against 1.61× quiet — and 3.44×
  was, briefly, the headline number for this whole investigation.
- **Node is not a browser proxy for anything crossing the wasm boundary.** It
  disagreed in *sign* with Chrome on three probes and underpriced the wasm→JS
  call by 3-5×. `bench/web/perf-test.mjs` gates on a Node leg; that is sound for
  checksums and regression detection, and it cannot price a boundary.
- **Playwright cannot measure wasm in Firefox.** The attached debugger forces
  SpiderMonkey to baseline-compile every module. `bench/wasmgc/run.mjs` is
  driverless for the same reason `bench/web/run-firefox-real.mjs` is.
