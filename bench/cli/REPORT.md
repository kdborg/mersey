# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.39 | 95.6 | 95.48 | 87.39 |
| calls | 120.67 | 31.49 | 28.06 | 21.92 |
| fcompute | 102.14 | 101.9 | 103.41 | 101.58 |
| mathk | 13.31 | 13.88 | 13.15 | 12.5 |
| url | 8.56 | 7.34 | 10.81 | 9.46 |
| encoding | 4.37 | 1.6 | 4.2 | 1.49 |
| crypto | 18.53 | 0.47 | 1.53 | 0.5 |
| json | 3.84 | 1.32 | 1.44 | 0.74 |
| strings | 25.38 | 13.91 | 14.61 | 42.7 |
| reconcile | 11.48 | 5.54 | 6.01 | 53.32 |
| csv | 13.62 | 10.77 | 11.03 | 66.7 |
| path | 29.03 | 15.26 | 25.06 | 142.06 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 40 |
| reconcile | 40 | 10 | 10 | 60 |
| csv | 40 | 20 | 20 | 70 |
| path | 60 | 20 | 40 | 160 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 6.1 |
| calls | 38.9 | 31.2 | 34.9 | 6.1 |
| fcompute | 40.6 | 29 | 36.9 | 6 |
| mathk | 39.9 | 30.7 | 36.9 | 6 |
| url | 43.2 | 38.2 | 41.7 | 8.9 |
| encoding | 42 | 30.7 | 38.3 | 7.5 |
| crypto | 48.2 | 26.9 | 35.3 | 6 |
| json | 40.4 | 29.3 | 36.8 | 6 |
| strings | 41.8 | 52 | 39.3 | 8.8 |
| reconcile | 47.7 | 55.3 | 42.7 | 10.3 |
| csv | 42.8 | 67.1 | 47.2 | 9.1 |
| path | 48.1 | 43.6 | 46.4 | 27.3 |
