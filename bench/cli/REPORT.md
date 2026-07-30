# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.44 | 96.59 | 96.54 | 88.2 |
| calls | 121.68 | 30.96 | 28.26 | 22.01 |
| fcompute | 103.56 | 103.46 | 105.21 | 101.98 |
| mathk | 13.47 | 13.98 | 13.26 | 15.78 |
| url | 8.64 | 7.82 | 11.08 | 20.27 |
| encoding | 4.52 | 1.6 | 4.24 | 3.74 |
| crypto | 19.54 | 0.49 | 1.45 | 0.92 |
| json | 4 | 1.27 | 1.46 | 1.05 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.7 | 5.8 |
| mathk | 40 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 42 | 7 |
| encoding | 42 | 30.7 | 38.2 | 6.1 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
