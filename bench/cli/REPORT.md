# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.67 | 94.88 | 94.72 | 86.71 |
| calls | 119.9 | 30.57 | 27.77 | 21.75 |
| fcompute | 101.33 | 101.19 | 102.66 | 100.83 |
| mathk | 13.18 | 13.8 | 12.97 | 15.56 |
| url | 8.36 | 7.4 | 10.82 | 59.83 |
| encoding | 4.38 | 1.58 | 4.16 | 7.48 |
| crypto | 18.57 | 0.46 | 1.52 | 20.86 |
| json | 3.87 | 1.29 | 1.46 | 2.09 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 60 |
| encoding | 30 | 10 | 10 | 10 |
| crypto | 40 | 0 | 10 | 20 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.7 | 5.9 |
| calls | 38.8 | 31.1 | 34.7 | 5.8 |
| fcompute | 40.5 | 29 | 36.8 | 5.5 |
| mathk | 39.9 | 30.7 | 36.7 | 5.5 |
| url | 43 | 38.1 | 41.7 | 6.8 |
| encoding | 41.9 | 30.7 | 38.1 | 3.8 |
| crypto | 48.2 | 26.9 | 35.3 | 3.7 |
| json | 40.4 | 29.3 | 36.8 | 5.6 |
