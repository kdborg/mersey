# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.65 | 94.89 | 94.61 | 86.58 |
| calls | 119.66 | 30.52 | 27.8 | 21.71 |
| fcompute | 101.42 | 101.12 | 102.67 | 100.9 |
| mathk | 13.15 | 13.78 | 12.95 | 12.4 |
| url | 8.48 | 7.24 | 10.63 | 9.58 |
| encoding | 4.34 | 1.57 | 4.03 | 1.45 |
| crypto | 18.31 | 0.47 | 1.51 | 0.5 |
| json | 3.82 | 1.29 | 1.42 | 0.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.6 | 29 | 36.8 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 11.8 |
| encoding | 41.9 | 30.7 | 38.3 | 7.3 |
| crypto | 48.2 | 26.9 | 35.2 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
