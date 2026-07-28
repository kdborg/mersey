# Overnight benchmark run

Three repeats cannot resolve a change smaller than this machine's own drift: in
one pair of sweeps `locks` moved 33% with no code touching it, and `canvas` read
5.4 ms once and 9.0–9.3 ms every time after. Every runner now takes `REPEATS`
from the environment, and each metric's median is taken independently, so more
samples buy resolution directly.

```bash
cd bench/web
REPEATS=15 node run.mjs                 # stock Chromium + Firefox (js, poly)
REPEATS=15 node run-native.mjs          # Firefox fork (native)
REPEATS=15 node run-native-chromium.mjs # Chromium fork (native)
REPEATS=15 node run-engine.mjs          # wasm engine over the Node stub realm

cd ../cli && REPEATS=25 node run.mjs     # node/bun/deno vs the Mersey CLI
```

Then regenerate every report surface — never hand-edit the numbers:

```bash
cd bench/web
node report.mjs && node gen-report-data.mjs && node report-pertech.mjs && node report-jsnative.mjs
```

Rules that make the numbers mean something:

- **Nothing else on the machine.** No builds, no editors indexing. A browser leg
  measured beside a Chromium build is not a measurement.
- **Don't interleave runners.** Each launches browsers and measures process-tree
  footprint; two at once pollute both.
- `frameworkui` costs ~200 s of pure retry on the Chromium fork (5 attempts ×
  3 repeats, 8 s each) because it fails there for a known reason — see the
  coverage table in `report-jsnative.html`. `WL=` skips it if the run needs to
  fit a window.
- Servo and Ladybird are paused; their runners are unchanged and need not run.

## What this run is meant to settle

`bench/web` ranks every workload by native/JS ratio across the four forks. Two
questions came out of that ranking, and both need numbers this run can resolve:

1. **Chromium's fork is the worst leg on ~25 of 31 workloads** — streams 56×
   against a 3.4× median, geometry 33.8× against 11.4×, msgchannel 31.7× against
   4.2×, urlpattern 14.1× where Firefox's fork is 1.4×. The diagnosis is that
   `HostWebIntern` declines every name outside a ~29-entry enum, so those
   workloads' property and method names fall to the JSON path — an encode on the
   engine side, a parse in the host, and a parsed reply back — while Firefox
   interns whatever it is handed and carries everything on the wide tier.
   Making Chromium intern arbitrarily was tried and **reverted**: it routes calls
   the JSON path used to serve onto a wide tier that has no fall-through for
   them, and twelve workloads stopped reporting. Doing it properly means giving
   the wide tier the same reflective fall-through the JSON path already has, for
   `new`, get, set and call, on every receiver kind — not just reflective
   handles. Tonight's numbers are the before for that work.

2. **`geometry` and `frameworkui2` are slow on every fork** (medians 11.4× and
   27.3×), which makes them engine or bridge-design problems rather than one
   port's gap. Nobody has profiled them yet.

`crypto` needs nothing: it is already faster than the browser's own JS on
Chromium (0.44×) and at parity on Firefox (0.95×).
