# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 89.01 | 97.02 | 98.43 | 88.85 |
| calls | 121.6 | 30.96 | 29.06 | 22.48 |
| fcompute | 102.71 | 103.02 | 105.77 | 103.54 |
| mathk | 13.49 | 14.15 | 13.6 | 12.74 |
| url | 9.06 | 7.92 | 11.11 | 9.86 |
| encoding | 4.73 | 1.86 | 4.36 | 1.48 |
| crypto | 19.46 | 0.5 | 1.53 | 0.5 |
| json | 4.08 | 1.34 | 1.51 | 0.72 |
| strings | 27.82 | 15.1 | 15.73 | 58.58 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 40 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 20 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 50 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 60 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.9 | 6 |
| calls | 38.9 | 31.1 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.8 | 5.9 |
| mathk | 40 | 30.7 | 36.9 | 6 |
| url | 43.3 | 38.2 | 41.9 | 8.7 |
| encoding | 42 | 30.8 | 38.3 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 6 |
| strings | 41.7 | 52 | 39.3 | 8.4 |
