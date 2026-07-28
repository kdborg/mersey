# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.81 | 95.08 | 94.92 | 87.08 |
| calls | 120 | 30.49 | 27.84 | 21.72 |
| fcompute | 101.4 | 101.21 | 102.74 | 100.91 |
| mathk | 13.34 | 13.82 | 13.2 | 15.6 |
| url | 8.63 | 7.64 | 11.06 | 45.68 |
| encoding | 4.55 | 1.61 | 4.12 | 7.73 |
| crypto | 19.39 | 0.47 | 1.55 | 21.84 |
| json | 4.01 | 1.31 | 1.46 | 2.19 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 50 |
| encoding | 30 | 10 | 10 | 10 |
| crypto | 40 | 10 | 10 | 20 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.7 | 5.6 |
| mathk | 39.8 | 30.7 | 36.7 | 5.6 |
| url | 43 | 38.2 | 41.6 | 6.4 |
| encoding | 41.8 | 30.7 | 38.2 | 3.9 |
| crypto | 48.2 | 26.9 | 35.2 | 3.7 |
| json | 40.4 | 29.3 | 36.8 | 5.7 |
