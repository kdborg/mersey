# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.36 | 95.66 | 95.5 | 87.39 |
| calls | 120.63 | 30.72 | 28.03 | 21.92 |
| fcompute | 101.99 | 101.99 | 103.49 | 101.55 |
| mathk | 13.3 | 13.93 | 13.11 | 12.51 |
| url | 8.57 | 7.36 | 10.98 | 9.55 |
| encoding | 4.42 | 1.61 | 4.21 | 1.5 |
| crypto | 18.67 | 0.48 | 1.55 | 0.5 |
| json | 3.91 | 1.32 | 1.46 | 0.74 |
| strings | 27.2 | 14.61 | 14.86 | 43.8 |
| reconcile | 11.48 | 5.54 | 6.02 | 56.47 |
| csv | 13.61 | 10.94 | 11.02 | 70.64 |
| path | 29.13 | 15.71 | 25.09 | 146.54 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |
| reconcile | 40 | 10 | 10 | 60 |
| csv | 40 | 20 | 20 | 70 |
| path | 60 | 20 | 40 | 170 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6.1 |
| fcompute | 40.5 | 29 | 36.9 | 6 |
| mathk | 39.9 | 30.7 | 36.8 | 6 |
| url | 43.2 | 38.2 | 41.7 | 8.8 |
| encoding | 42 | 30.7 | 38.3 | 7.5 |
| crypto | 48.2 | 26.9 | 35.3 | 6 |
| json | 40.5 | 29.3 | 36.8 | 6 |
| strings | 41.7 | 52.1 | 39.3 | 8.7 |
| reconcile | 47.8 | 55.6 | 42.9 | 10.3 |
| csv | 43.8 | 67.1 | 47.6 | 9.3 |
| path | 48.1 | 43.6 | 46.4 | 9.8 |
