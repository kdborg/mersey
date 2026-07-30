# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.64 | 94.74 | 94.59 | 86.55 |
| calls | 119.68 | 30.47 | 27.75 | 21.7 |
| fcompute | 101.13 | 100.98 | 102.51 | 100.78 |
| mathk | 13.16 | 13.76 | 12.95 | 12.4 |
| url | 8.44 | 7.23 | 10.73 | 19.58 |
| encoding | 4.29 | 1.56 | 4.04 | 1.49 |
| crypto | 18.26 | 0.45 | 1.5 | 0.5 |
| json | 3.81 | 1.26 | 1.38 | 0.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 41.9 | 30.7 | 38.4 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 37 | 5.9 |
