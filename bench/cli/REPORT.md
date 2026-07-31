# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 89.34 | 98.27 | 96.11 | 90.76 |
| calls | 124.25 | 31.58 | 28.72 | 22.44 |
| fcompute | 105.25 | 105.54 | 107.17 | 110.29 |
| mathk | 13.95 | 14.72 | 14.33 | 13.67 |
| url | 8.94 | 7.96 | 11.32 | 21.12 |
| encoding | 4.77 | 1.73 | 4.31 | 1.66 |
| crypto | 20.1 | 0.5 | 1.58 | 0.52 |
| json | 4.32 | 1.37 | 1.48 | 0.82 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 110 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 50 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6 |
| fcompute | 40.6 | 29.1 | 36.9 | 5.9 |
| mathk | 40 | 30.7 | 36.9 | 6 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42.1 | 30.7 | 38.3 | 7.4 |
| crypto | 48.4 | 27 | 35.3 | 5.9 |
| json | 40.6 | 29.3 | 36.9 | 5.9 |
