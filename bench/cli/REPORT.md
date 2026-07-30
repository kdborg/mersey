# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.79 | 94.78 | 94.58 | 86.54 |
| calls | 119.67 | 30.48 | 27.74 | 21.71 |
| fcompute | 101.16 | 101.02 | 102.5 | 100.76 |
| mathk | 13.17 | 13.75 | 12.93 | 15.52 |
| url | 8.48 | 7.39 | 10.64 | 20.01 |
| encoding | 4.3 | 1.59 | 4.04 | 1.58 |
| crypto | 18.56 | 0.46 | 1.48 | 0.5 |
| json | 3.82 | 1.25 | 1.39 | 0.75 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 20 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.8 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.8 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42 | 30.7 | 38.3 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.8 |
| json | 40.4 | 29.3 | 36.9 | 5.9 |
