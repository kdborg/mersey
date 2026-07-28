# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 98.58 | 94.84 | 94.67 | 86.69 |
| calls | 119.68 | 30.51 | 27.84 | 21.71 |
| fcompute | 101.16 | 101.06 | 102.5 | 100.7 |
| mathk | 13.24 | 13.78 | 13.07 | 15.55 |
| url | 8.42 | 7.67 | 11.22 | 28.24 |
| encoding | 4.45 | 1.63 | 4.21 | 4.08 |
| crypto | 19.26 | 0.47 | 1.55 | 3.25 |
| json | 3.9 | 1.27 | 1.44 | 1.11 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 140 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 30 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.7 | 5.9 |
| calls | 38.8 | 31.1 | 34.7 | 5.8 |
| fcompute | 40.4 | 29 | 36.8 | 5.8 |
| mathk | 39.8 | 30.7 | 36.8 | 5.8 |
| url | 43.1 | 38.1 | 41.5 | 6.8 |
| encoding | 41.9 | 30.7 | 38.2 | 4.4 |
| crypto | 48.1 | 26.9 | 35.2 | 4.4 |
| json | 40.4 | 29.3 | 36.8 | 5.9 |
