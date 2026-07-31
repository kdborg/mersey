# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.95 | 96.03 | 95.78 | 87.6 |
| calls | 119.71 | 30.75 | 28.17 | 22.04 |
| fcompute | 102.64 | 102.66 | 103.34 | 101.63 |
| mathk | 13.33 | 13.81 | 13.38 | 12.53 |
| url | 8.75 | 7.68 | 11.17 | 20.7 |
| encoding | 4.36 | 1.57 | 4.24 | 1.56 |
| crypto | 19.4 | 0.47 | 1.53 | 0.54 |
| json | 4.03 | 1.29 | 1.43 | 0.78 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.9 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.6 | 29 | 36.9 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.8 |
| url | 43.2 | 38.1 | 41.7 | 7 |
| encoding | 42 | 30.7 | 38.2 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
