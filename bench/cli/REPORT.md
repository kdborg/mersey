# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 25 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.95 | 94.76 | 94.65 | 86.61 |
| calls | 119.82 | 30.42 | 27.76 | 21.71 |
| fcompute | 100.3 | 101.05 | 102.58 | 100.77 |
| mathk | 13.22 | 13.77 | 12.94 | 15.52 |
| url | 8.56 | 7.17 | 10.72 | 21.77 |
| encoding | 4.32 | 1.57 | 4.11 | 3.76 |
| crypto | 20.02 | 0.46 | 1.51 | 2.93 |
| json | 2.64 | 1.25 | 1.42 | 0.95 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 43 | 29 | 34.8 | 5.9 |
| calls | 43.5 | 31.1 | 34.8 | 5.8 |
| fcompute | 45 | 29 | 36.8 | 5.8 |
| mathk | 44.4 | 30.7 | 36.6 | 5.9 |
| url | 47.8 | 38.2 | 41.6 | 7 |
| encoding | 46.4 | 30.7 | 38.2 | 4.4 |
| crypto | 53.6 | 26.9 | 35.2 | 4.4 |
| json | 44.7 | 29.3 | 36.7 | 5.9 |
