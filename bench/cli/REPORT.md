# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.84 | 96.31 | 95.96 | 87.92 |
| calls | 120.87 | 30.71 | 28.14 | 22.05 |
| fcompute | 103.04 | 102.8 | 103.87 | 100.86 |
| mathk | 13.25 | 13.87 | 13.14 | 12.4 |
| url | 8.56 | 7.72 | 10.88 | 20.45 |
| encoding | 4.41 | 1.57 | 4.04 | 1.46 |
| crypto | 19 | 0.47 | 1.55 | 0.5 |
| json | 3.9 | 1.27 | 1.41 | 0.73 |

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
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.8 |
| mathk | 39.9 | 30.7 | 36.8 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 7 |
| encoding | 41.9 | 30.7 | 38.3 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
