# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.67 | 95.47 | 95.48 | 87.04 |
| calls | 121.41 | 30.87 | 28.04 | 21.88 |
| fcompute | 101.1 | 100.99 | 103.12 | 100.83 |
| mathk | 13.35 | 13.94 | 13.07 | 15.52 |
| url | 8.57 | 7.66 | 10.97 | 20.48 |
| encoding | 4.37 | 1.57 | 4.16 | 1.85 |
| crypto | 18.83 | 0.47 | 1.51 | 0.58 |
| json | 3.86 | 1.26 | 1.41 | 0.97 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 20 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.8 | 5.9 |
| calls | 38.9 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.8 |
| mathk | 39.8 | 30.7 | 36.8 | 5.9 |
| url | 43.2 | 38.2 | 41.7 | 7 |
| encoding | 41.9 | 30.7 | 38.4 | 7.3 |
| crypto | 48.1 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
