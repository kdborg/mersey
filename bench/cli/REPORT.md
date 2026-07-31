# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 90.65 | 96.01 | 96.42 | 89.23 |
| calls | 123.86 | 31.08 | 28.05 | 21.95 |
| fcompute | 105.28 | 103.68 | 104.25 | 102.76 |
| mathk | 13.44 | 13.89 | 13.14 | 12.64 |
| url | 8.55 | 7.85 | 10.99 | 10.47 |
| encoding | 4.46 | 1.58 | 4.37 | 1.58 |
| crypto | 19.56 | 0.47 | 1.55 | 0.51 |
| json | 4.11 | 1.32 | 1.44 | 0.74 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 130 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.8 | 29 | 34.9 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.9 | 6 |
| url | 43.2 | 38.2 | 41.7 | 11.9 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
