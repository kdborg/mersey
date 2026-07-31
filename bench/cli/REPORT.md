# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.76 | 94.78 | 94.71 | 86.58 |
| calls | 119.79 | 30.51 | 27.75 | 21.71 |
| fcompute | 101.18 | 101.19 | 102.52 | 101.06 |
| mathk | 13.15 | 13.79 | 13.44 | 12.41 |
| url | 8.43 | 7.17 | 10.75 | 9.73 |
| encoding | 4.29 | 1.57 | 4.04 | 1.46 |
| crypto | 18.3 | 0.48 | 1.51 | 0.5 |
| json | 3.8 | 1.25 | 1.42 | 0.73 |
| strings | 25.77 | 14.18 | 14.84 | 46.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.9 | 5.9 |
| calls | 38.9 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.9 |
| mathk | 39.8 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 11.9 |
| encoding | 42 | 30.7 | 38.3 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
| strings | 41.8 | 52 | 39.3 | 85.2 |
