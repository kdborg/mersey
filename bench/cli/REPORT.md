# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 88.04 | 96.07 | 96.12 | 87.71 |
| calls | 121.56 | 30.87 | 27.82 | 21.77 |
| fcompute | 102.64 | 102.65 | 103.91 | 102.03 |
| mathk | 13.36 | 13.93 | 13.12 | 15.72 |
| url | 8.42 | 7.43 | 10.88 | 23.32 |
| encoding | 4.42 | 1.59 | 4.18 | 3.99 |
| crypto | 18.74 | 0.48 | 1.53 | 3.24 |
| json | 3.99 | 1.28 | 1.47 | 1.12 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.9 | 5.9 |
| calls | 38.8 | 31.2 | 34.7 | 5.9 |
| fcompute | 40.5 | 29 | 36.7 | 5.8 |
| mathk | 39.8 | 30.7 | 36.8 | 5.9 |
| url | 43.1 | 38.1 | 41.5 | 7 |
| encoding | 41.8 | 30.7 | 38.2 | 4.4 |
| crypto | 48.2 | 26.9 | 35.2 | 4.4 |
| json | 40.3 | 29.3 | 36.7 | 5.9 |
