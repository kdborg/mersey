# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.88 | 96.48 | 96.24 | 88.19 |
| calls | 121.99 | 30.88 | 28.42 | 22.04 |
| fcompute | 102.82 | 102.62 | 103.99 | 101.98 |
| mathk | 13.35 | 14.03 | 13.34 | 12.49 |
| url | 8.57 | 7.52 | 10.89 | 9.67 |
| encoding | 4.46 | 1.62 | 4.26 | 1.51 |
| crypto | 19.02 | 0.51 | 1.54 | 0.5 |
| json | 3.92 | 1.33 | 1.52 | 0.72 |
| strings | 27.43 | 14.66 | 15.37 | 55.83 |
| reconcile | 11.36 | 5.5 | 5.94 | 70.24 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 60 |
| reconcile | 40 | 10 | 10 | 70 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.9 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.8 | 5.9 |
| mathk | 39.9 | 30.7 | 37 | 6 |
| url | 43.2 | 38.1 | 41.8 | 8.7 |
| encoding | 42 | 30.7 | 38.2 | 7.4 |
| crypto | 48.4 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 6 |
| strings | 41.8 | 52.1 | 39.3 | 8.4 |
| reconcile | 47.8 | 55.2 | 42.8 | 8.9 |
