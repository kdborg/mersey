# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Firefox fork native | Servo fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | — | — | — | 25.9 | — |
| canvas | 3.6 | 71.5 | 3.3 | 4.6 | 170.3 | 6.8 | 2.6 | 74.5 | 4.9 | 4.4 | 27.3 |
| compute | 94.0 | 19184.0 | 91.9 | 111.0 | 80326.0 | 656.7 | 107.8 | 19404.0 | 132.8 | 102.2 | 97.5 |
| crypto | 9.1 | 102.0 | 8.7 | 8.1 | 227.9 | 9.3 | 2.4 | 82.4 | 4.6 | 10.4 | 38.7 |
| dom | 3.3 | 107.3 | 4.6 | 6.3 | 209.8 | 9.3 | 12.4 | 99.7 | 17.8 | 13.1 | 35.1 |
| fcompute | — | — | — | — | — | — | — | — | — | 118.5 | — |
| json | 2.3 | 118.5 | 1.9 | 3.6 | 303.4 | 4.9 | 3.5 | 26.2 | 5.1 | 3.2 | 1.1 |
| mathk | — | — | — | — | — | — | — | — | — | 36.7 | — |
| storage | 65.9 | 261.2 | 81.3 | 31.0 | 353.6 | 36.4 | 1981.5 | 2425.0 | 3607.4 | 55.1 | 2129.9 |
| url | 19.8 | 191.4 | 21.2 | 25.5 | 486.1 | 28.5 | 11.4 | 150.5 | 12.0 | 41.3 | 67.1 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Firefox fork native | Servo fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | — | — | — | 15.2 | — |
| canvas | 29.3 | 80.4 | 63.1 | 57.6 | 104.5 | 107.6 | 20.2 | 70.6 | 74.3 | 20.8 | 30.3 |
| compute | — | — | 56.5 | — | — | 90.7 | 3.3 | 77.0 | 58.0 | 22.4 | 16.4 |
| crypto | 21.5 | 77.7 | 59.2 | 44.6 | 90.9 | 72.5 | 3.2 | 58.3 | 57.6 | 21.8 | 20.8 |
| dom | 62.5 | 114.0 | 99.3 | 63.8 | 118.6 | 107.5 | 26.3 | 74.0 | 75.2 | 30.6 | 33.6 |
| fcompute | — | — | — | — | — | — | — | — | — | 15.7 | — |
| json | 23.5 | 74.7 | 59.9 | 53.0 | 89.5 | 92.4 | 3.6 | 56.4 | 57.3 | 19.0 | 16.3 |
| mathk | — | — | — | — | — | — | — | — | — | 16.6 | — |
| storage | 33.7 | 89.2 | 69.5 | 53.6 | 88.6 | 98.7 | 5.1 | 59.7 | 56.3 | 26.3 | 16.6 |
| url | 38.6 | 100.2 | 74.6 | 58.3 | 98.2 | 118.8 | 8.9 | 63.7 | 61.0 | 26.6 | 22.7 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Servo polyfill | Firefox fork native | Servo fork native |
|---|---|---|---|---|---|
| calls | — | — | — | — | — |
| canvas | 20.0× | 47.6× | 20.8× | 1.2× | 7.6× |
| compute | 204.1× | 854.5× | 206.4× | 1.1× | 1.0× |
| crypto | 11.2× | 25.1× | 9.1× | 1.1× | 4.3× |
| dom | 33.0× | 64.5× | 30.7× | 4.0× | 10.8× |
| fcompute | — | — | — | — | — |
| json | 51.3× | 131.3× | 11.3× | 1.4× | 0.5× |
| mathk | — | — | — | — | — |
| storage | 4.0× | 5.4× | 36.8× | 0.8× | 32.3× |
| url | 9.6× | 24.5× | 7.6× | 2.1× | 3.4× |
