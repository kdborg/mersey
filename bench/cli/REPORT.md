# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.77 | 94.71 | 94.76 | 86.68 |
| calls | 119.81 | 30.57 | 27.77 | 21.71 |
| fcompute | 101.27 | 101.02 | 102.51 | 100.69 |
| mathk | 13.16 | 14.1 | 12.96 | 12.4 |
| url | 8.48 | 7.19 | 10.65 | 9.62 |
| encoding | 4.35 | 1.6 | 4.09 | 1.44 |
| crypto | 18.45 | 0.47 | 1.49 | 0.5 |
| json | 3.81 | 1.27 | 1.43 | 0.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 6 |
| calls | 38.8 | 31.1 | 34.8 | 6 |
| fcompute | 40.5 | 29 | 36.8 | 5.9 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 11.8 |
| encoding | 41.9 | 30.7 | 38.3 | 7.3 |
| crypto | 48.1 | 26.9 | 35.2 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
