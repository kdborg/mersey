# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

> **Caveat — Playwright Firefox understates wasm.** The "Firefox" columns are measured through Playwright, which drives Firefox with the JS debugger attached; SpiderMonkey runs ALL WebAssembly baseline-only while debugging (microsoft/playwright#11102), so the Firefox WASM-poly and JS-backend (wasm compute tier) columns are 5-7× slower than real Firefox. The "Firefox real" columns are the honest numbers: the system Firefox, headless, no driver attached (`run-firefox-real.mjs`). Its memory deltas use a fresh browser per sample (blank page → self-navigation to the workload in one process tree), so they run slightly higher than the Playwright columns, which reuse a warm browser.

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox real JS | Firefox real WASM poly | Firefox real JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 13179.4 | 49.3 | — | — | 237.6 | — | 13839.0 | 87.8 | — | — | — | — | — | — | 24.7 | — | — | 23.7 |
| canvas | 2.8 | 77.6 | 3.1 | 4.9 | 193.8 | 6.3 | 4.9 | 66.7 | 7.3 | 2.8 | 72.2 | 4.5 | 10.8 | 2511.3 | 26.6 | 4.7 | 2.5 | — | 10.8 |
| compute | 98.3 | 18843.7 | 89.0 | 110.8 | — | 592.6 | 112.8 | 18752.5 | 117.5 | 101.1 | 18928.1 | 131.9 | 1741.5 | — | — | 100.4 | 92.8 | — | 95.0 |
| crypto | 9.7 | 112.1 | 8.8 | 8.3 | 252.7 | 10.1 | 8.4 | 88.6 | 9.7 | 2.5 | 79.2 | 4.4 | 5.2 | 3056.7 | 16.6 | 9.0 | 18.1 | — | 16.1 |
| cssom | 5.2 | 160.3 | 10.7 | 9.3 | 431.3 | 14.3 | 12.1 | 166.9 | 16.5 | 11.3 | 153.2 | 17.4 | 29.5 | 4761.3 | 57.7 | 30.8 | 27.9 | — | 76.7 |
| dom | 3.3 | 103.9 | 7.0 | 6.1 | 224.6 | 8.8 | 7.1 | 91.7 | 9.8 | 12.5 | 85.4 | 17.1 | 21.9 | 2467.2 | 41.7 | 9.8 | 20.7 | — | 23.7 |
| encoding | 10.3 | 96.7 | 10.9 | 2.7 | 228.4 | 3.9 | 3.0 | 87.2 | 4.0 | 4.5 | 78.5 | 6.9 | 18.9 | 2659.0 | 31.9 | 28.1 | 21.8 | — | 25.4 |
| events | 5.0 | 111.3 | 5.2 | 11.3 | 243.4 | 12.3 | 12.2 | 124.5 | 12.0 | 8.1 | 91.4 | 9.5 | 8.7 | 2867.8 | 15.5 | 39.7 | 27.7 | 21.1 | 32.4 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 114.8 | — | — | 106.6 |
| fetch | 117.5 | 154.0 | 113.9 | 131.8 | 142.3 | 132.0 | 112.0 | 171.1 | 205.1 | 33.1 | 42.1 | 36.7 | — | — | — | 514.8 | 41.5 | — | — |
| json | 1.9 | 27.0 | 1.8 | 3.7 | 118.1 | 5.0 | 3.6 | 25.8 | 4.8 | 3.7 | 26.1 | 5.0 | 20.8 | 482.0 | 30.9 | 3.6 | 1.1 | — | 5.5 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 31.8 | — | — | 31.6 |
| query | 4.7 | 30.5 | 5.2 | 6.2 | 45.8 | 6.5 | 5.9 | 21.3 | 6.6 | 9.3 | 25.9 | 11.3 | 6.7 | 491.2 | 10.8 | 8.1 | 11.9 | — | 10.6 |
| storage | 65.5 | 256.7 | 80.4 | 32.9 | 394.9 | 35.2 | 32.6 | 173.1 | 40.0 | 1958.8 | 2339.1 | 3372.9 | 5110.3 | 10746.3 | 14479.6 | 59.2 | 1969.8 | — | 228.2 |
| timers | 12.2 | 147.9 | 12.2 | 24.5 | 394.4 | 24.0 | 22.6 | 147.3 | 23.9 | 9.9 | 107.9 | 9.3 | 9.9 | 3708.6 | 10.0 | 51.5 | 35.4 | 24.4 | 75.4 |
| url | 20.0 | 187.8 | 18.5 | 26.8 | 466.6 | 27.8 | 29.6 | 177.6 | 29.7 | 11.5 | 137.3 | 13.2 | 56.3 | 5343.1 | 64.1 | 38.3 | 25.1 | — | 54.8 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox real JS | Firefox real WASM poly | Firefox real JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 47.9 | 56.1 | — | — | 99.8 | — | -7.5 | 95.1 | — | — | — | — | — | — | 15.2 | — | — | 21.8 |
| canvas | 18.5 | 72.3 | 62.5 | 44.7 | 99.8 | 82.6 | 70.4 | 96.4 | 158.3 | 20.0 | 68.5 | 70.8 | 0.0 | 412.2 | 384.8 | 15.2 | 32.3 | — | 24.6 |
| compute | 17.7 | 1.6 | 55.9 | 46.9 | — | 85.5 | 75.4 | 265.9 | 111.5 | 3.7 | 57.1 | 57.1 | 7.0 | — | — | 14.0 | 18.2 | — | 19.4 |
| crypto | 97.8 | 68.5 | 58.7 | 46.7 | 90.7 | 86.9 | 66.5 | 84.8 | 80.7 | 4.0 | 59.5 | 58.0 | 2.0 | 445.5 | 366.7 | 21.7 | 21.9 | — | 18.7 |
| cssom | 16.3 | 82.5 | 59.1 | 47.4 | 0.7 | 93.7 | 61.7 | 156.0 | 108.7 | 5.2 | 59.4 | 59.6 | 14.7 | 452.0 | 459.9 | 8.3 | 18.5 | — | 29.0 |
| dom | 57.0 | 105.1 | 98.9 | 63.0 | 103.4 | 105.5 | 97.6 | 144.6 | 159.5 | 29.0 | 100.3 | 108.9 | 161.5 | 423.3 | 421.7 | 30.5 | 25.9 | — | 86.1 |
| encoding | 25.5 | 83.4 | 67.4 | 47.1 | 93.2 | 91.2 | 65.1 | 100.4 | 130.3 | 4.5 | 68.5 | 57.0 | 0.0 | 431.4 | 387.8 | 18.0 | 21.9 | — | 22.2 |
| events | 20.2 | 74.4 | 63.2 | 31.6 | 86.7 | 95.4 | 87.6 | 132.8 | 120.6 | 8.3 | 63.0 | 61.1 | 0.0 | 401.6 | 362.2 | 23.7 | 24.1 | — | 26.5 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 24.7 | — | — | 22.0 |
| fetch | 16.4 | 52.0 | 57.3 | 52.6 | 94.1 | 87.8 | 87.4 | 95.0 | 114.3 | 7.9 | 61.8 | 57.5 | — | — | — | 13.7 | 21.4 | — | — |
| json | 16.9 | 48.5 | 59.7 | 46.6 | 92.0 | 91.3 | 80.8 | 122.7 | 96.9 | 3.8 | 57.7 | 59.5 | 0.6 | 375.2 | 421.9 | 21.3 | 18.2 | — | 19.7 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 15.7 | — | — | 20.4 |
| query | 27.1 | 71.9 | 63.7 | 52.2 | 93.1 | 94.8 | 69.8 | 93.0 | 72.5 | 22.3 | 78.6 | 72.5 | 1.7 | 390.9 | 375.6 | 22.6 | 28.6 | — | 27.5 |
| storage | 32.8 | 88.9 | 68.9 | 57.9 | 90.0 | 87.2 | 87.3 | 113.9 | 99.8 | 4.8 | 55.4 | 56.5 | 19.3 | 430.4 | 452.3 | 22.1 | 18.9 | — | 25.8 |
| timers | 27.7 | 86.1 | 65.5 | 61.2 | 112.7 | 100.4 | 93.3 | 112.5 | 97.2 | 7.5 | 62.7 | 61.2 | 1.7 | 446.8 | 462.6 | 23.5 | 18.8 | — | 24.9 |
| url | 37.4 | 99.7 | 74.0 | 57.1 | 101.3 | 98.4 | 86.9 | 115.5 | 86.8 | 9.0 | 60.8 | 62.2 | 22.0 | 422.0 | 461.4 | 16.5 | 28.5 | — | 31.8 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Firefox real polyfill | Servo polyfill | Ladybird polyfill | Firefox fork native | Servo fork native | Ladybird fork native |
|---|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | — | — |
| canvas | 27.4× | 68.3× | 23.5× | 25.5× | 885.8× | 1.7× | 0.9× | — |
| compute | 191.7× | — | 190.7× | 192.5× | — | 1.0× | 0.9× | — |
| crypto | 11.6× | 26.2× | 9.2× | 8.2× | 316.4× | 0.9× | 1.9× | — |
| cssom | 31.0× | 83.5× | 32.3× | 29.7× | 921.8× | 6.0× | 5.4× | — |
| dom | 31.8× | 68.7× | 28.0× | 26.1× | 754.5× | 3.0× | 6.3× | — |
| encoding | 9.4× | 22.2× | 8.5× | 7.6× | 258.0× | 2.7× | 2.1× | — |
| events | 22.2× | 48.5× | 24.8× | 18.2× | 571.3× | 7.9× | 5.5× | 4.2× |
| fcompute | — | — | — | — | — | — | — | — |
| fetch | 1.3× | 1.2× | 1.5× | 0.4× | — | 4.4× | 0.4× | — |
| json | 14.4× | 63.0× | 13.8× | 13.9× | 257.1× | 1.9× | 0.6× | — |
| mathk | — | — | — | — | — | — | — | — |
| query | 6.5× | 9.7× | 4.5× | 5.5× | 104.4× | 1.7× | 2.5× | — |
| storage | 3.9× | 6.0× | 2.6× | 35.7× | 164.0× | 0.9× | 30.1× | — |
| timers | 12.1× | 32.3× | 12.1× | 8.8× | 303.6× | 4.2× | 2.9× | 2.0× |
| url | 9.4× | 23.3× | 8.9× | 6.9× | 267.0× | 1.9× | 1.3× | — |
