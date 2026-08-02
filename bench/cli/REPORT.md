# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.36 | 95.57 | 95.42 | 87.3 |
| calls | 120.72 | 30.78 | 28.07 | 21.89 |
| fcompute | 102.05 | 101.87 | 103.5 | 101.52 |
| mathk | 13.27 | 13.91 | 13.09 | 12.51 |
| url | 8.6 | 7.36 | 10.92 | 9.4 |
| encoding | 4.36 | 1.59 | 4.17 | 1.47 |
| crypto | 18.47 | 0.48 | 1.54 | 0.5 |
| json | 3.86 | 1.27 | 1.45 | 0.73 |
| strings | 25.51 | 13.95 | 14.63 | 55.1 |
| reconcile | 11.35 | 5.5 | 6 | 56.3 |

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
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 60 |
| reconcile | 40 | 10 | 10 | 60 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 6 |
| calls | 38.8 | 31.2 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.9 | 6 |
| mathk | 39.9 | 30.7 | 36.8 | 6 |
| url | 43.2 | 38.1 | 41.8 | 8.8 |
| encoding | 41.9 | 30.7 | 38.3 | 7.5 |
| crypto | 48.3 | 26.9 | 35.3 | 6 |
| json | 40.5 | 29.3 | 36.8 | 6 |
| strings | 41.6 | 52.1 | 39.3 | 8.6 |
| reconcile | 47.9 | 55.2 | 42.6 | 9.7 |
