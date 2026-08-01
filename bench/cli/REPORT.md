# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.75 | 94.73 | 94.76 | 86.6 |
| calls | 120.13 | 30.64 | 27.77 | 21.73 |
| fcompute | 101.16 | 101.16 | 102.6 | 100.73 |
| mathk | 13.15 | 13.8 | 12.93 | 12.48 |
| url | 8.53 | 7.14 | 10.71 | 9.51 |
| encoding | 4.39 | 1.58 | 4.08 | 1.44 |
| crypto | 18.39 | 0.47 | 1.52 | 0.52 |
| json | 3.81 | 1.28 | 1.42 | 0.72 |
| strings | 25.31 | 13.71 | 14.48 | 45.92 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 6 |
| calls | 38.8 | 31.1 | 34.8 | 6 |
| fcompute | 40.6 | 29 | 36.8 | 5.9 |
| mathk | 39.8 | 30.7 | 36.8 | 6 |
| url | 43 | 38.2 | 41.7 | 12 |
| encoding | 42 | 30.7 | 38.3 | 7.4 |
| crypto | 48.1 | 26.9 | 35.3 | 6 |
| json | 40.5 | 29.3 | 36.8 | 6 |
| strings | 41.7 | 52 | 39.3 | 85.2 |
