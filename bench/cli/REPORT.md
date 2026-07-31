# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.94 | 96.06 | 95.99 | 86.82 |
| calls | 121.54 | 30.83 | 28.14 | 21.76 |
| fcompute | 102.79 | 102.61 | 102.47 | 102.14 |
| mathk | 13.23 | 13.8 | 12.94 | 12.44 |
| url | 8.49 | 7.4 | 10.68 | 20.52 |
| encoding | 4.39 | 1.59 | 4.07 | 1.53 |
| crypto | 19.23 | 0.48 | 1.51 | 0.5 |
| json | 3.89 | 1.26 | 1.42 | 0.72 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 5.9 |
| calls | 38.9 | 31.1 | 34.8 | 5.9 |
| fcompute | 40.6 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.8 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 41.9 | 30.7 | 38.3 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
