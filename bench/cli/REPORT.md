# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.7 | 94.72 | 94.59 | 86.55 |
| calls | 119.64 | 30.5 | 27.74 | 21.71 |
| fcompute | 101.17 | 101.03 | 102.55 | 100.66 |
| mathk | 13.15 | 13.78 | 12.95 | 12.4 |
| url | 8.39 | 7.2 | 10.74 | 9.55 |
| encoding | 4.35 | 1.56 | 4.09 | 1.45 |
| crypto | 18.28 | 0.46 | 1.49 | 0.5 |
| json | 3.8 | 1.25 | 1.41 | 0.72 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 30 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.9 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.6 | 29 | 36.8 | 5.9 |
| mathk | 39.9 | 30.7 | 36.8 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 11.9 |
| encoding | 42 | 30.7 | 38.4 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
