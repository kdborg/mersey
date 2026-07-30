# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.65 | 98.91 | 94.6 | 86.6 |
| calls | 119.58 | 30.46 | 27.74 | 21.71 |
| fcompute | 101.13 | 101.1 | 102.48 | 100.67 |
| mathk | 13.16 | 13.77 | 12.93 | 12.4 |
| url | 8.39 | 7.25 | 10.67 | 19.93 |
| encoding | 4.3 | 1.56 | 4.02 | 1.5 |
| crypto | 18.53 | 0.46 | 1.49 | 0.5 |
| json | 3.8 | 1.24 | 1.41 | 0.73 |

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
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29.1 | 34.9 | 5.9 |
| calls | 38.8 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.9 | 30.7 | 36.8 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 7 |
| encoding | 41.9 | 30.7 | 38.3 | 7.4 |
| crypto | 48.1 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.8 | 5.9 |
