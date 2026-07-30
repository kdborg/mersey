# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 89.39 | 96.3 | 95.52 | 87.68 |
| calls | 119.81 | 30.87 | 28.01 | 21.83 |
| fcompute | 102.12 | 102.43 | 103.25 | 101.03 |
| mathk | 13.24 | 13.88 | 12.98 | 15.53 |
| url | 8.5 | 7.22 | 10.89 | 20.62 |
| encoding | 4.35 | 1.57 | 4.07 | 1.52 |
| crypto | 18.8 | 0.46 | 1.51 | 0.58 |
| json | 3.81 | 1.24 | 1.42 | 0.75 |

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
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.8 | 30.7 | 36.8 | 5.9 |
| url | 43 | 38.2 | 41.8 | 7 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
