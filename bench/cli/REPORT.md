# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.67 | 94.81 | 94.62 | 86.57 |
| calls | 119.63 | 30.51 | 27.78 | 22.05 |
| fcompute | 101.26 | 101.38 | 102.21 | 101.15 |
| mathk | 13.16 | 13.76 | 12.98 | 12.4 |
| url | 8.47 | 7.3 | 10.85 | 9.46 |
| encoding | 4.34 | 1.57 | 4.14 | 1.43 |
| crypto | 18.37 | 0.46 | 1.5 | 0.5 |
| json | 3.8 | 1.25 | 1.43 | 0.72 |
| strings | 26.61 | 14.39 | 15.01 | 55.52 |

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
| strings | 50 | 20 | 20 | 60 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29.1 | 34.8 | 6 |
| calls | 38.8 | 31.2 | 34.9 | 6 |
| fcompute | 40.5 | 29 | 37 | 5.9 |
| mathk | 39.9 | 30.7 | 36.8 | 6 |
| url | 43.2 | 38.2 | 41.8 | 12 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 6 |
| json | 40.5 | 29.3 | 36.8 | 6 |
| strings | 41.8 | 52 | 39.3 | 96.5 |
