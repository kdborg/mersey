# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.93 | 94.8 | 94.69 | 86.66 |
| calls | 119.69 | 30.5 | 27.78 | 21.8 |
| fcompute | 101.34 | 101.06 | 102.76 | 100.81 |
| mathk | 13.17 | 13.79 | 12.94 | 12.47 |
| url | 8.47 | 7.24 | 10.82 | 9.59 |
| encoding | 4.35 | 1.59 | 4.06 | 1.48 |
| crypto | 18.27 | 0.46 | 1.45 | 0.5 |
| json | 3.8 | 1.29 | 1.43 | 0.77 |

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

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 6 |
| fcompute | 40.5 | 29 | 36.9 | 5.9 |
| mathk | 39.8 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 11.9 |
| encoding | 41.9 | 30.7 | 38.4 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
