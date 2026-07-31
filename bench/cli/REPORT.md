# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 86.73 | 95.33 | 95.36 | 87.43 |
| calls | 120.84 | 30.75 | 28 | 21.7 |
| fcompute | 101.98 | 101.83 | 103.52 | 101.59 |
| mathk | 13.28 | 13.86 | 13.04 | 12.4 |
| url | 8.43 | 7.39 | 10.7 | 9.93 |
| encoding | 4.34 | 1.59 | 4.13 | 1.45 |
| crypto | 18.66 | 0.46 | 1.51 | 0.51 |
| json | 3.86 | 1.27 | 1.39 | 0.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.9 | 6 |
| calls | 38.8 | 31.1 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.8 | 6 |
| url | 43.2 | 38.2 | 41.8 | 11.9 |
| encoding | 42 | 30.7 | 38.3 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
