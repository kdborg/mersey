# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.8 | 96.53 | 96.14 | 88.42 |
| calls | 121.37 | 30.87 | 28.31 | 21.77 |
| fcompute | 103.05 | 103.11 | 104.15 | 102.45 |
| mathk | 13.49 | 13.89 | 13.46 | 12.54 |
| url | 8.6 | 7.74 | 11.06 | 20.8 |
| encoding | 4.42 | 1.6 | 4.19 | 1.54 |
| crypto | 19.66 | 0.46 | 1.56 | 0.5 |
| json | 4 | 1.35 | 1.46 | 0.75 |

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
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.1 | 38.2 | 41.8 | 7 |
| encoding | 42 | 30.7 | 38.4 | 7.3 |
| crypto | 48.3 | 26.9 | 35.2 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
