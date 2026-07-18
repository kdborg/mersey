# Mersey web-platform benchmarks

Wall-clock of the workload loop (self-timed in-language, startup excluded), median of 3 runs. Lower is faster.

- **js** — the workload in plain JavaScript (the browser's own engine)
- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser
- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge

> **Caveat — Playwright Firefox understates wasm.** The "Firefox" columns are measured through Playwright, which drives Firefox with the JS debugger attached; SpiderMonkey runs ALL WebAssembly baseline-only while debugging (microsoft/playwright#11102), so the Firefox WASM-poly and JS-backend (wasm compute tier) columns are 5-7× slower than real Firefox. The "Firefox real" columns are the honest numbers: the system Firefox, headless, no driver attached (`run-firefox-real.mjs`). Its memory deltas use a fresh browser per sample (blank page → self-navigation to the workload in one process tree), so they run slightly higher than the Playwright columns, which reuse a warm browser.

## Time (ms)

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox real JS | Firefox real WASM poly | Firefox real JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 14007.7 | 48.2 | — | — | 256.0 | — | 13852.8 | 88.4 | — | — | — | — | — | — | 24.7 | — | — | 23.7 |
| canvas | 3.2 | 86.6 | 3.2 | 5.3 | 254.0 | 6.8 | 4.7 | 66.8 | 6.4 | 2.8 | 72.2 | 4.5 | 10.8 | 2511.3 | 26.6 | 4.7 | 2.5 | 9.6 | 10.8 |
| compute | 95.8 | 20242.9 | 93.6 | 112.9 | — | 670.3 | 113.8 | 18537.1 | 120.5 | 101.1 | 18928.1 | 131.9 | 1741.5 | — | — | 100.4 | 92.8 | 96.8 | 95.0 |
| crypto | 10.0 | 118.1 | 8.7 | 8.0 | 300.1 | 9.2 | 8.8 | 85.0 | 9.4 | 2.5 | 79.2 | 4.4 | 5.2 | 3056.7 | 16.6 | 9.0 | 18.1 | 12.5 | 16.1 |
| cssom | 5.4 | 180.2 | 9.3 | 9.6 | 479.9 | 14.3 | 10.7 | 141.3 | 15.8 | 11.3 | 153.2 | 17.4 | 29.5 | 4761.3 | 57.7 | 30.8 | 27.9 | 41.1 | 76.7 |
| dom | 3.3 | 115.1 | 4.7 | 6.3 | 248.1 | 9.4 | 6.7 | 97.7 | 9.0 | 12.5 | 85.4 | 17.1 | 21.9 | 2467.2 | 41.7 | 9.8 | 20.7 | 23.1 | 23.7 |
| encoding | 11.5 | 118.0 | 12.1 | 2.9 | 252.1 | 4.3 | 3.3 | 81.5 | 3.8 | 4.5 | 78.5 | 6.9 | 18.9 | 2659.0 | 31.9 | 28.1 | 21.8 | 37.4 | 25.4 |
| events | 5.1 | 132.8 | 5.7 | 11.4 | 294.8 | 11.5 | 13.6 | 97.2 | 11.3 | 8.1 | 91.4 | 9.5 | 8.7 | 2867.8 | 15.5 | 39.7 | 27.7 | 20.2 | 32.4 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 114.8 | — | — | 106.6 |
| fetch | 130.0 | 194.5 | 123.8 | 142.4 | 155.2 | 131.2 | 102.6 | 108.7 | 141.2 | 33.1 | 42.1 | 36.7 | 63.4 | 181.5 | 73.3 | 514.8 | 41.5 | 75.2 | — |
| json | 2.0 | 9.2 | 1.9 | 3.6 | 37.1 | 5.1 | 4.3 | 8.4 | 5.2 | 3.7 | 26.1 | 5.0 | 20.8 | 482.0 | 30.9 | 3.6 | 1.1 | 1.4 | 5.5 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 31.8 | — | — | 31.6 |
| query | 5.0 | 34.7 | 5.5 | 6.3 | 49.8 | 6.5 | 5.9 | 21.9 | 6.7 | 9.3 | 25.9 | 11.3 | 6.7 | 491.2 | 10.8 | 8.1 | 11.9 | 8.5 | 10.6 |
| storage | 84.3 | 314.5 | 73.3 | 33.1 | 434.1 | 35.9 | 32.9 | 144.6 | 37.6 | 1958.8 | 2339.1 | 3372.9 | 5110.3 | 10746.3 | 14479.6 | 59.2 | 1969.8 | 5323.8 | 228.2 |
| timers | 12.5 | 162.3 | 11.6 | 25.5 | 457.1 | 24.7 | 23.9 | 137.4 | 21.5 | 9.9 | 107.9 | 9.3 | 9.9 | 3708.6 | 10.0 | 51.5 | 35.4 | 24.0 | 75.4 |
| url | 23.3 | 226.8 | 23.1 | 28.2 | 536.2 | 27.1 | 28.7 | 184.7 | 30.3 | 11.5 | 137.3 | 13.2 | 56.3 | 5343.1 | 64.1 | 38.3 | 25.1 | 42.2 | 54.8 |

## Memory — PSS delta vs blank page (MiB)

Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta). The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native engine is compiled into the browser binary, so its delta is workload allocation only.

| workload | Chromium JS | Chromium WASM poly | Chromium JS-backend | Firefox JS | Firefox WASM poly | Firefox JS-backend | Firefox real JS | Firefox real WASM poly | Firefox real JS-backend | Servo JS | Servo WASM poly | Servo JS-backend | Ladybird JS | Ladybird WASM poly | Ladybird JS-backend | Firefox fork native | Servo fork native | Ladybird fork native | Chromium fork native |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| calls | — | 56.8 | 55.6 | — | — | 109.3 | — | -5.9 | 92.4 | — | — | — | — | — | — | 15.2 | — | — | 21.8 |
| canvas | 24.1 | 82.2 | 62.5 | 50.2 | 91.8 | 103.7 | 71.5 | 61.3 | 57.9 | 20.0 | 68.5 | 70.8 | 0.0 | 412.2 | 384.8 | 15.2 | 32.3 | 26.0 | 24.6 |
| compute | 23.8 | 57.0 | 55.8 | 48.9 | — | 88.8 | 59.4 | 151.6 | 89.4 | 3.7 | 57.1 | 57.1 | 7.0 | — | — | 14.0 | 18.2 | — | 19.4 |
| crypto | 21.1 | 78.4 | 59.5 | 45.1 | 89.8 | 86.9 | 69.6 | 96.9 | 85.7 | 4.0 | 59.5 | 58.0 | 2.0 | 445.5 | 366.7 | 21.7 | 21.9 | 23.1 | 18.7 |
| cssom | 22.2 | 92.5 | 59.4 | 51.0 | 88.7 | 92.4 | 112.4 | 84.9 | 101.3 | 5.2 | 59.4 | 59.6 | 14.7 | 452.0 | 459.9 | 8.3 | 18.5 | 21.9 | 29.0 |
| dom | 63.2 | 114.0 | 97.6 | 67.4 | 108.1 | 108.4 | 106.9 | 94.2 | 97.0 | 29.0 | 100.3 | 108.9 | 161.5 | 423.3 | 421.7 | 30.5 | 25.9 | 222.8 | 86.1 |
| encoding | 31.6 | 93.0 | 67.9 | 48.8 | 99.2 | 93.0 | 60.2 | 102.1 | 110.4 | 4.5 | 68.5 | 57.0 | 0.0 | 431.4 | 387.8 | 18.0 | 21.9 | 27.2 | 22.2 |
| events | 26.3 | 82.8 | 63.7 | 49.9 | 93.6 | 94.4 | 57.0 | 111.0 | 95.3 | 8.3 | 63.0 | 61.1 | 0.0 | 401.6 | 362.2 | 23.7 | 24.1 | 25.0 | 26.5 |
| fcompute | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 24.7 | — | — | 22.0 |
| fetch | 19.5 | 61.6 | 57.3 | 50.5 | 95.8 | 89.8 | 58.4 | 87.3 | 120.3 | 7.9 | 61.8 | 57.5 | 40.0 | 483.9 | 469.1 | 13.7 | 21.4 | 33.5 | — |
| json | 23.1 | 56.9 | 60.3 | 47.1 | 95.6 | 91.7 | 125.2 | 104.1 | 95.5 | 3.8 | 57.7 | 59.5 | 0.6 | 375.2 | 421.9 | 21.3 | 18.2 | 17.9 | 19.7 |
| mathk | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — | 15.7 | — | — | 20.4 |
| query | 26.9 | 71.4 | 64.6 | 51.9 | 97.5 | 95.4 | 103.6 | 102.0 | 101.2 | 22.3 | 78.6 | 72.5 | 1.7 | 390.9 | 375.6 | 22.6 | 28.6 | 25.1 | 27.5 |
| storage | 32.9 | 88.6 | 69.4 | 51.6 | 91.6 | 97.9 | 59.6 | 83.3 | 127.2 | 4.8 | 55.4 | 56.5 | 19.3 | 430.4 | 452.3 | 22.1 | 18.9 | 19.2 | 25.8 |
| timers | 28.3 | 84.5 | 66.5 | 54.1 | 98.9 | 98.8 | 67.0 | 130.5 | 95.3 | 7.5 | 62.7 | 61.2 | 1.7 | 446.8 | 462.6 | 23.5 | 18.8 | 26.8 | 24.9 |
| url | 37.6 | 100.6 | 73.9 | 57.1 | 98.1 | 101.3 | 116.0 | 118.6 | 117.9 | 9.0 | 60.8 | 62.2 | 22.0 | 422.0 | 461.4 | 16.5 | 28.5 | 23.2 | 31.8 |

## Slowdown vs plain JS (Chromium JS = 1×)

| workload | Chromium polyfill | Firefox polyfill | Firefox real polyfill | Servo polyfill | Ladybird polyfill | Firefox fork native | Servo fork native | Ladybird fork native |
|---|---|---|---|---|---|---|---|---|
| calls | — | — | — | — | — | — | — | — |
| canvas | 27.0× | 79.2× | 20.9× | 22.5× | 783.6× | 1.5× | 0.8× | 3.0× |
| compute | 211.3× | — | 193.5× | 197.6× | — | 1.0× | 1.0× | 1.0× |
| crypto | 11.7× | 29.9× | 8.5× | 7.9× | 304.1× | 0.9× | 1.8× | 1.2× |
| cssom | 33.2× | 88.5× | 26.1× | 28.2× | 877.7× | 5.7× | 5.1× | 7.6× |
| dom | 35.3× | 76.1× | 30.0× | 26.2× | 756.8× | 3.0× | 6.4× | 7.1× |
| encoding | 10.3× | 21.9× | 7.1× | 6.8× | 231.1× | 2.4× | 1.9× | 3.2× |
| events | 26.3× | 58.4× | 19.2× | 18.1× | 567.9× | 7.9× | 5.5× | 4.0× |
| fcompute | — | — | — | — | — | — | — | — |
| fetch | 1.5× | 1.2× | 0.8× | 0.3× | 1.4× | 4.0× | 0.3× | 0.6× |
| json | 4.6× | 18.5× | 4.2× | 13.0× | 240.4× | 1.8× | 0.5× | 0.7× |
| mathk | — | — | — | — | — | — | — | — |
| query | 7.0× | 10.0× | 4.4× | 5.2× | 98.5× | 1.6× | 2.4× | 1.7× |
| storage | 3.7× | 5.1× | 1.7× | 27.7× | 127.5× | 0.7× | 23.4× | 63.2× |
| timers | 13.0× | 36.5× | 11.0× | 8.6× | 296.2× | 4.1× | 2.8× | 1.9× |
| url | 9.7× | 23.0× | 7.9× | 5.9× | 229.3× | 1.6× | 1.1× | 1.8× |
