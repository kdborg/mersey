# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.65 | 96.6 | 94.82 | 87.87 |
| calls | 121.47 | 30.86 | 28.14 | 21.87 |
| fcompute | 103.12 | 103.13 | 103.69 | 101.8 |
| mathk | 13.37 | 14.09 | 13.09 | 12.63 |
| url | 8.54 | 7.61 | 10.87 | 10.12 |
| encoding | 4.4 | 1.59 | 4.11 | 1.46 |
| crypto | 18.79 | 0.49 | 1.53 | 0.51 |
| json | 3.88 | 1.28 | 1.44 | 0.75 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 6 |
| calls | 38.8 | 31.1 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.9 | 5.9 |
| mathk | 39.8 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 11.9 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 6 |
| json | 40.5 | 29.3 | 36.9 | 6 |
