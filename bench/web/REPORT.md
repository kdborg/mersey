# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox fork native |
|---|---|---|---|---|---|---|---|
| canvas | 3.6 | 71.5 | 5.1 | 4.6 | 170.3 | 7.0 | 14.0 |
| compute | 94.0 | 19184.0 | 190.8 | 111.0 | 80326.0 | 150.3 | 102.0 |
| crypto | 9.1 | 102.0 | 8.6 | 8.1 | 227.9 | 9.7 | 32.0 |
| dom | 3.3 | 107.3 | 4.4 | 6.3 | 209.8 | 9.6 | 31.0 |
| json | 2.3 | 118.5 | 2.1 | 3.6 | 303.4 | 5.0 | 28.0 |
| storage | 65.9 | 261.2 | 56.7 | 31.0 | 353.6 | 35.6 | 79.0 |
| url | 19.8 | 191.4 | 21.9 | 25.5 | 486.1 | 27.6 | 104.0 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox fork native |
|---|---|---|---|---|---|---|---|
| canvas | 29.3 | 80.4 | 65.6 | 57.6 | 104.5 | 107.8 | 26.8 |
| compute | — | — | 61.2 | — | — | 88.9 | 3.5 |
| crypto | 21.5 | 77.7 | 58.6 | 44.6 | 90.9 | 73.4 | 11.4 |
| dom | 62.5 | 114.0 | 98.6 | 63.8 | 118.6 | 105.0 | 32.0 |
| json | 23.5 | 74.7 | 59.4 | 53.0 | 89.5 | 94.6 | 8.8 |
| storage | 33.7 | 89.2 | 69.4 | 53.6 | 88.6 | 96.9 | 27.4 |
| url | 38.6 | 100.2 | 74.2 | 58.3 | 98.2 | 102.2 | 21.1 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Firefox fork native |
|---|---|---|---|
| canvas | 20.0× | 47.6× | 3.9× |
| compute | 204.1× | 854.5× | 1.1× |
| crypto | 11.2× | 25.1× | 3.5× |
| dom | 33.0× | 64.5× | 9.5× |
| json | 51.3× | 131.3× | 12.1× |
| storage | 4.0× | 5.4× | 1.2× |
| url | 9.6× | 24.5× | 5.2× |
