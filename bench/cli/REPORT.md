# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.25 | 96.11 | 95.79 | 87.75 |
| calls | 121.84 | 30.55 | 27.77 | 22.02 |
| fcompute | 102.46 | 102.59 | 103.77 | 102.23 |
| mathk | 13.26 | 13.8 | 13.19 | 15.53 |
| url | 8.52 | 7.71 | 10.76 | 20.06 |
| encoding | 4.35 | 1.59 | 4.23 | 2.55 |
| crypto | 19.02 | 0.48 | 1.47 | 0.91 |
| json | 3.97 | 1.28 | 1.4 | 1.02 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
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
| compute | 38.7 | 29 | 34.9 | 5.9 |
| calls | 38.9 | 31.2 | 34.9 | 5.8 |
| fcompute | 40.6 | 29 | 36.9 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.8 |
| url | 43.2 | 38.2 | 41.8 | 6.9 |
| encoding | 42 | 30.7 | 38.4 | 7.2 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
