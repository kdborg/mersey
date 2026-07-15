# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

## Time (ms)

| workload | Chromium JS | Chromium polyfill | Firefox JS | Firefox polyfill | Firefox fork native |
|---|---|---|---|---|---|
| canvas | 3.6 | 71.5 | 4.6 | 170.3 | 29.0 |
| compute | 94.0 | 19184.0 | 111.0 | 80326.0 | 139.0 |
| crypto | 9.1 | 102.0 | 8.1 | 227.9 | 90.0 |
| dom | 3.3 | 107.3 | 6.3 | 209.8 | 68.0 |
| json | 2.3 | 118.5 | 3.6 | 303.4 | 121.0 |
| storage | 65.9 | 261.2 | 31.0 | 353.6 | 122.0 |
| url | 19.8 | 191.4 | 25.5 | 486.1 | 155.0 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium polyfill | Firefox JS | Firefox polyfill | Firefox fork native |
|---|---|---|---|---|---|
| canvas | 29.3 | 80.4 | 57.6 | 104.5 | 13.9 |
| compute | — | — | — | — | 16.9 |
| crypto | 21.5 | 77.7 | 44.6 | 90.9 | 14.1 |
| dom | 62.5 | 114.0 | 63.8 | 118.6 | 34.2 |
| json | 23.5 | 74.7 | 53.0 | 89.5 | 14.3 |
| storage | 33.7 | 89.2 | 53.6 | 88.6 | 18.4 |
| url | 38.6 | 100.2 | 58.3 | 98.2 | 27.0 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Firefox fork native |
|---|---|---|---|
| canvas | 20.0× | 47.6× | 8.1× |
| compute | 204.1× | 854.5× | 1.5× |
| crypto | 11.2× | 25.1× | 9.9× |
| dom | 33.0× | 64.5× | 20.9× |
| json | 51.3× | 131.3× | 52.4× |
| storage | 4.0× | 5.4× | 1.9× |
| url | 9.6× | 24.5× | 7.8× |
