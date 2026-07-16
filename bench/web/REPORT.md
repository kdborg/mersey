# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox fork native |
|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | 25.5 |
| canvas | 3.6 | 71.5 | 3.3 | 4.6 | 170.3 | 6.8 | 4.5 |
| compute | 94.0 | 19184.0 | 91.9 | 111.0 | 80326.0 | 656.7 | 100.6 |
| crypto | 9.1 | 102.0 | 8.7 | 8.1 | 227.9 | 9.3 | 9.1 |
| dom | 3.3 | 107.3 | 4.6 | 6.3 | 209.8 | 9.3 | 16.1 |
| fcompute | — | — | — | — | — | — | 120.0 |
| json | 2.3 | 118.5 | 1.9 | 3.6 | 303.4 | 4.9 | 24.0 |
| mathk | — | — | — | — | — | — | 36.3 |
| storage | 65.9 | 261.2 | 81.3 | 31.0 | 353.6 | 36.4 | 57.9 |
| url | 19.8 | 191.4 | 21.2 | 25.5 | 486.1 | 28.5 | 48.0 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox fork native |
|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | 14.0 |
| canvas | 29.3 | 80.4 | 63.1 | 57.6 | 104.5 | 107.6 | 16.1 |
| compute | — | — | 56.5 | — | — | 90.7 | 18.2 |
| crypto | 21.5 | 77.7 | 59.2 | 44.6 | 90.9 | 72.5 | 13.7 |
| dom | 62.5 | 114.0 | 99.3 | 63.8 | 118.6 | 107.5 | 23.7 |
| fcompute | — | — | — | — | — | — | 20.4 |
| json | 23.5 | 74.7 | 59.9 | 53.0 | 89.5 | 92.4 | 23.6 |
| mathk | — | — | — | — | — | — | 16.8 |
| storage | 33.7 | 89.2 | 69.5 | 53.6 | 88.6 | 98.7 | 24.4 |
| url | 38.6 | 100.2 | 74.6 | 58.3 | 98.2 | 118.8 | 15.0 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Firefox fork native |
|---|---|---|---|
| calls | — | — | — |
| canvas | 20.0× | 47.6× | 1.3× |
| compute | 204.1× | 854.5× | 1.1× |
| crypto | 11.2× | 25.1× | 1.0× |
| dom | 33.0× | 64.5× | 4.9× |
| fcompute | — | — | — |
| json | 51.3× | 131.3× | 10.4× |
| mathk | — | — | — |
| storage | 4.0× | 5.4× | 0.9× |
| url | 9.6× | 24.5× | 2.4× |
