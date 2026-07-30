# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.65 | 94.72 | 94.61 | 86.56 |
| calls | 119.64 | 30.46 | 27.73 | 21.7 |
| fcompute | 101.13 | 100.98 | 102.48 | 100.7 |
| mathk | 13.16 | 13.78 | 12.94 | 12.4 |
| url | 8.39 | 7.25 | 10.69 | 19.92 |
| encoding | 4.28 | 1.58 | 4.03 | 1.56 |
| crypto | 18.33 | 0.46 | 1.51 | 0.5 |
| json | 3.81 | 1.26 | 1.43 | 0.75 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 20 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.6 | 29 | 37 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42 | 30.7 | 38.4 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
