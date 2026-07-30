# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.57 | 97.6 | 95.79 | 87.74 |
| calls | 120.89 | 30.6 | 28.08 | 21.8 |
| fcompute | 102.46 | 102.61 | 103.2 | 101.8 |
| mathk | 13.28 | 13.85 | 13.07 | 15.62 |
| url | 8.77 | 7.68 | 11.1 | 20.45 |
| encoding | 4.4 | 1.61 | 4.15 | 2.62 |
| crypto | 19.41 | 0.47 | 1.46 | 0.9 |
| json | 3.99 | 1.26 | 1.42 | 1.16 |

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
| compute | 38.7 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.8 |
| fcompute | 40.6 | 29 | 36.8 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 6.9 |
| encoding | 42 | 30.7 | 38.4 | 7.2 |
| crypto | 48.3 | 26.9 | 35.4 | 5.8 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
