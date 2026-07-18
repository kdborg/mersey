# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 13179.4 | 49.3 | — | — | 237.6 | — | — | — | — | — | — | 24.8 | — | — | 23.3 |
| canvas | 2.8 | 77.6 | 3.1 | 4.9 | 193.8 | 6.3 | 2.8 | 72.2 | 4.5 | 11.2 | 2585.8 | 27.8 | 4.5 | 2.2 | 10.7 | 10.6 |
| compute | 98.3 | 18843.7 | 89.0 | 110.8 | — | 592.6 | 101.1 | 18928.1 | 131.9 | 1748.6 | — | — | 100.0 | 95.6 | 91.2 | 91.3 |
| crypto | 9.7 | 112.1 | 8.8 | 8.3 | 252.7 | 10.1 | 2.5 | 79.2 | 4.4 | 5.1 | 3044.0 | 17.3 | 9.5 | 18.0 | 26.5 | 15.7 |
| cssom | 5.2 | 160.3 | 10.7 | 9.3 | 431.3 | 14.3 | 11.3 | 153.2 | 17.4 | 28.3 | 4877.3 | 63.6 | 30.8 | 27.8 | 52.3 | 78.5 |
| dom | 3.3 | 103.9 | 7.0 | 6.1 | 224.6 | 8.8 | 12.5 | 85.4 | 17.1 | 20.5 | 2536.8 | 43.0 | 10.0 | 19.3 | 28.6 | 22.2 |
| encoding | 10.3 | 96.7 | 10.9 | 2.7 | 228.4 | 3.9 | 4.5 | 78.5 | 6.9 | 17.4 | 2741.4 | 34.2 | 29.8 | 21.6 | 36.8 | 27.1 |
| events | 5.0 | 111.3 | 5.2 | 11.3 | 243.4 | 12.3 | 8.1 | 91.4 | 9.5 | 6.9 | 2806.6 | 16.1 | 40.0 | 25.4 | 39.6 | 29.8 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | 114.9 | — | — | 113.7 |
| fetch | 117.5 | 154.0 | 113.9 | 131.8 | 142.3 | 132.0 | 33.1 | 42.1 | 36.7 | — | — | — | 458.6 | 41.4 | — | — |
| json | 1.9 | 27.0 | 1.8 | 3.7 | 118.1 | 5.0 | 3.7 | 26.1 | 5.0 | 21.4 | 1728.8 | 32.3 | 3.3 | 1.0 | 1.4 | 6.1 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | 34.5 | — | — | 30.7 |
| query | 4.7 | 30.5 | 5.2 | 6.2 | 45.8 | 6.5 | 9.3 | 25.9 | 11.3 | 6.7 | 500.8 | 10.9 | 8.2 | 12.2 | 10.0 | 11.0 |
| storage | 65.5 | 256.7 | 80.4 | 32.9 | 394.9 | 35.2 | 1958.8 | 2339.1 | 3372.9 | 4965.0 | 10796.3 | 14910.3 | 59.7 | 1926.5 | 9810.8 | 225.9 |
| timers | 12.2 | 147.9 | 12.2 | 24.5 | 394.4 | 24.0 | 9.9 | 107.9 | 9.3 | 9.6 | 3846.1 | 10.0 | 116.4 | 51.3 | 145.1 | 180.6 |
| url | 20.0 | 187.8 | 18.5 | 26.8 | 466.6 | 27.8 | 11.5 | 137.3 | 13.2 | 56.9 | 5270.6 | 64.1 | 37.4 | 24.4 | 53.4 | 56.3 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 47.9 | 56.1 | — | — | 99.8 | — | — | — | — | — | — | 0.2 | — | — | 21.3 |
| canvas | 18.5 | 72.3 | 62.5 | 44.7 | 99.8 | 82.6 | 20.0 | 68.5 | 70.8 | 0.0 | 402.2 | 332.7 | 6.4 | 33.1 | — | 22.6 |
| compute | 17.7 | 1.6 | 55.9 | 46.9 | — | 85.5 | 3.7 | 57.1 | 57.1 | 3.7 | — | — | 10.0 | 18.4 | — | 20.3 |
| crypto | 97.8 | 68.5 | 58.7 | 46.7 | 90.7 | 86.9 | 4.0 | 59.5 | 58.0 | 0.0 | 478.9 | 345.5 | 9.3 | 20.7 | — | 18.3 |
| cssom | 16.3 | 82.5 | 59.1 | 47.4 | 0.7 | 93.7 | 5.2 | 59.4 | 59.6 | 11.8 | 461.6 | 387.9 | 6.4 | 19.1 | — | 27.9 |
| dom | 57.0 | 105.1 | 98.9 | 63.0 | 103.4 | 105.5 | 29.0 | 100.3 | 108.9 | 175.8 | 446.4 | 381.3 | 26.8 | 76.1 | — | 84.2 |
| encoding | 25.5 | 83.4 | 67.4 | 47.1 | 93.2 | 91.2 | 4.5 | 68.5 | 57.0 | 0.0 | 459.0 | 351.9 | 11.1 | 22.1 | — | 21.2 |
| events | 20.2 | 74.4 | 63.2 | 31.6 | 86.7 | 95.4 | 8.3 | 63.0 | 61.1 | 0.0 | 402.9 | 476.3 | 11.7 | 24.7 | — | 25.0 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | 7.6 | — | — | 18.9 |
| fetch | 16.4 | 52.0 | 57.3 | 52.6 | 94.1 | 87.8 | 7.9 | 61.8 | 57.5 | — | — | — | 14.7 | 22.8 | — | — |
| json | 16.9 | 48.5 | 59.7 | 46.6 | 92.0 | 91.3 | 3.8 | 57.7 | 59.5 | 0.0 | 358.4 | 362.8 | 5.9 | 18.4 | — | 17.6 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | 12.0 | — | — | 18.5 |
| query | 27.1 | 71.9 | 63.7 | 52.2 | 93.1 | 94.8 | 22.3 | 78.6 | 72.5 | 0.0 | 421.0 | 450.6 | 15.4 | 33.9 | — | 26.3 |
| storage | 32.8 | 88.9 | 68.9 | 57.9 | 90.0 | 87.2 | 4.8 | 55.4 | 56.5 | 17.1 | 342.5 | 419.6 | 17.1 | 19.2 | — | 24.8 |
| timers | 27.7 | 86.1 | 65.5 | 61.2 | 112.7 | 100.4 | 7.5 | 62.7 | 61.2 | 2.8 | 462.7 | 452.2 | 13.6 | 21.1 | — | 24.9 |
| url | 37.4 | 99.7 | 74.0 | 57.1 | 101.3 | 98.4 | 9.0 | 60.8 | 62.2 | 20.0 | 441.9 | 423.0 | 26.0 | 29.5 | — | 28.3 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Servo polyfill | Ladybird polyfill | Firefox fork native | Servo fork native | Ladybird fork native |
|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | — |
| canvas | 27.4× | 68.3× | 25.5× | 912.1× | 1.6× | 0.8× | 3.8× |
| compute | 191.7× | — | 192.5× | — | 1.0× | 1.0× | 0.9× |
| crypto | 11.6× | 26.2× | 8.2× | 315.1× | 1.0× | 1.9× | 2.7× |
| cssom | 31.0× | 83.5× | 29.7× | 944.3× | 6.0× | 5.4× | 10.1× |
| dom | 31.8× | 68.7× | 26.1× | 775.8× | 3.1× | 5.9× | 8.7× |
| encoding | 9.4× | 22.2× | 7.6× | 266.0× | 2.9× | 2.1× | 3.6× |
| events | 22.2× | 48.5× | 18.2× | 559.1× | 8.0× | 5.1× | 7.9× |
| fcompute | — | — | — | — | — | — | — |
| fetch | 1.3× | 1.2× | 0.4× | — | 3.9× | 0.4× | — |
| json | 14.4× | 63.0× | 13.9× | 922.0× | 1.8× | 0.5× | 0.7× |
| mathk | — | — | — | — | — | — | — |
| query | 6.5× | 9.7× | 5.5× | 106.4× | 1.7× | 2.6× | 2.1× |
| storage | 3.9× | 6.0× | 35.7× | 164.8× | 0.9× | 29.4× | 149.7× |
| timers | 12.1× | 32.3× | 8.8× | 314.9× | 9.5× | 4.2× | 11.9× |
| url | 9.4× | 23.3× | 6.9× | 263.4× | 1.9× | 1.2× | 2.7× |
