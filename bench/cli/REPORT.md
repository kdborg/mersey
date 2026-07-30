# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.83 | 97.17 | 96.66 | 87.98 |
| calls | 122.33 | 30.92 | 28.12 | 21.89 |
| fcompute | 102.81 | 103.67 | 104.86 | 103.14 |
| mathk | 13.52 | 14 | 13.31 | 15.62 |
| url | 8.7 | 7.67 | 11.15 | 20.82 |
| encoding | 4.49 | 1.6 | 4.13 | 2.27 |
| crypto | 19.31 | 0.47 | 1.46 | 0.92 |
| json | 4.1 | 1.29 | 1.45 | 1.07 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.8 | 29.1 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.6 | 29 | 36.8 | 5.9 |
| mathk | 40 | 30.7 | 37 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42 | 30.7 | 38.3 | 7.2 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
