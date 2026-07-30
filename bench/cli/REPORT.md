# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 25 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.66 | 94.71 | 94.63 | 86.57 |
| calls | 119.63 | 30.45 | 27.73 | 21.71 |
| fcompute | 101.12 | 100.97 | 102.46 | 100.71 |
| mathk | 13.14 | 13.75 | 12.92 | 15.52 |
| url | 8.42 | 7.13 | 10.65 | 21.7 |
| encoding | 4.32 | 1.56 | 4.1 | 3.78 |
| crypto | 18.11 | 0.45 | 1.49 | 2.93 |
| json | 3.77 | 1.25 | 1.42 | 0.95 |

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
| calls | 38.8 | 31.1 | 34.9 | 5.8 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.8 | 30.7 | 36.8 | 5.8 |
| url | 43.2 | 38.2 | 41.7 | 7 |
| encoding | 41.9 | 30.7 | 38.3 | 4.4 |
| crypto | 48.2 | 26.9 | 35.3 | 4.4 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
