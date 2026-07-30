# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 89.23 | 97.18 | 96.19 | 90.17 |
| calls | 123.34 | 31.04 | 28.37 | 22.24 |
| fcompute | 104.21 | 102.86 | 104.52 | 103 |
| mathk | 13.86 | 14.07 | 13.64 | 15.89 |
| url | 8.86 | 7.78 | 11.35 | 21.15 |
| encoding | 4.6 | 1.68 | 4.31 | 2.66 |
| crypto | 19.76 | 0.48 | 1.49 | 1.01 |
| json | 4.1 | 1.31 | 1.51 | 1.08 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 50 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.8 | 29.1 | 34.9 | 5.9 |
| calls | 38.9 | 31.2 | 34.9 | 5.8 |
| fcompute | 40.7 | 29 | 36.8 | 5.8 |
| mathk | 40 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 6.9 |
| encoding | 42.1 | 30.7 | 38.4 | 7.2 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
