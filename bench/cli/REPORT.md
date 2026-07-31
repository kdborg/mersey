# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.65 | 94.93 | 94.66 | 86.57 |
| calls | 119.63 | 30.51 | 27.74 | 21.7 |
| fcompute | 101.26 | 101.44 | 102.73 | 100.85 |
| mathk | 13.17 | 13.82 | 12.96 | 12.4 |
| url | 8.45 | 7.22 | 10.85 | 9.58 |
| encoding | 4.33 | 1.57 | 4.06 | 1.45 |
| crypto | 18.29 | 0.47 | 1.5 | 0.5 |
| json | 3.81 | 1.24 | 1.42 | 0.72 |
| strings | 25.82 | 13.62 | 14.84 | 46.54 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 20 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.5 | 29.1 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.6 | 11.9 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.1 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
| strings | 41.7 | 52 | 39.3 | 85.2 |
