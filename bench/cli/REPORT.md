# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.66 | 94.77 | 94.6 | 86.55 |
| calls | 119.7 | 30.48 | 27.75 | 21.7 |
| fcompute | 101.16 | 100.96 | 102.53 | 100.73 |
| mathk | 13.15 | 13.77 | 12.94 | 12.41 |
| url | 8.4 | 7.18 | 10.62 | 9.59 |
| encoding | 4.31 | 1.57 | 4.02 | 1.46 |
| crypto | 18.27 | 0.46 | 1.48 | 0.5 |
| json | 3.8 | 1.25 | 1.42 | 0.73 |

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
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.8 |
| mathk | 39.8 | 30.7 | 36.9 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 11.9 |
| encoding | 42 | 30.7 | 38.3 | 7.3 |
| crypto | 48.3 | 26.9 | 35.3 | 5.9 |
| json | 40.4 | 29.3 | 36.8 | 5.9 |
