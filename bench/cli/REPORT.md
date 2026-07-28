# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 88.84 | 95.88 | 95.33 | 88.35 |
| calls | 121.74 | 30.92 | 28.01 | 22.01 |
| fcompute | 102.68 | 103.28 | 103.77 | 103.37 |
| mathk | 13.45 | 14.04 | 13.25 | 15.91 |
| url | 8.59 | 7.33 | 10.74 | 414.55 |
| encoding | 4.48 | 1.59 | 4.05 | 7.11 |
| crypto | 19.38 | 0.51 | 1.47 | 21.22 |
| json | 3.9 | 1.31 | 1.46 | 2.13 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 20 |
| url | 30 | 10 | 20 | 430 |
| encoding | 30 | 10 | 10 | 10 |
| crypto | 50 | 10 | 10 | 20 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.7 | 5.9 |
| calls | 38.9 | 31.1 | 34.7 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.6 |
| mathk | 39.8 | 30.7 | 36.9 | 5.6 |
| url | 43.2 | 38.2 | 41.6 | 6.6 |
| encoding | 41.9 | 30.7 | 38.2 | 3.8 |
| crypto | 48.2 | 26.9 | 35.3 | 3.7 |
| json | 40.4 | 29.3 | 36.7 | 5.7 |
