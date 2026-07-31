# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.8 | 94.71 | 94.87 | 86.61 |
| calls | 119.62 | 30.63 | 27.77 | 21.75 |
| fcompute | 101.66 | 101.18 | 102.8 | 100.74 |
| mathk | 13.15 | 13.87 | 13.07 | 12.4 |
| url | 8.52 | 7.15 | 11.05 | 9.7 |
| encoding | 4.36 | 1.57 | 4.1 | 1.46 |
| crypto | 18.28 | 0.46 | 1.5 | 0.5 |
| json | 3.86 | 1.26 | 1.42 | 0.73 |
| strings | 26.29 | 14.09 | 15.08 | 48.62 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29.1 | 34.9 | 5.9 |
| calls | 38.8 | 31.1 | 34.8 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.9 |
| mathk | 39.9 | 30.7 | 37 | 5.9 |
| url | 43.1 | 38.2 | 41.7 | 11.9 |
| encoding | 42 | 30.7 | 38.2 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.4 | 29.3 | 36.9 | 5.9 |
| strings | 41.7 | 52 | 39.3 | 85.2 |
