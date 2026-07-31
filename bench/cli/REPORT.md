# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.65 | 94.71 | 94.64 | 86.59 |
| calls | 119.67 | 30.44 | 27.84 | 21.71 |
| fcompute | 101.13 | 101.03 | 102.49 | 100.83 |
| mathk | 13.15 | 13.78 | 12.93 | 12.4 |
| url | 8.46 | 7.17 | 10.83 | 9.58 |
| encoding | 4.38 | 1.58 | 4.23 | 1.46 |
| crypto | 18.38 | 0.47 | 1.51 | 0.5 |
| json | 3.81 | 1.3 | 1.42 | 0.73 |

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
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.9 | 31.2 | 34.8 | 5.9 |
| fcompute | 40.6 | 29 | 37 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.1 | 38.2 | 41.7 | 11.8 |
| encoding | 42 | 30.7 | 38.3 | 7.3 |
| crypto | 48.1 | 26.9 | 35.3 | 5.9 |
| json | 40.4 | 29.3 | 36.8 | 5.9 |
