# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 93.32 | 101.39 | 97.43 | 88.66 |
| calls | 123.04 | 31.16 | 28.5 | 22.13 |
| fcompute | 103.82 | 102.56 | 105.13 | 103.22 |
| mathk | 13.38 | 13.98 | 13.61 | 12.54 |
| url | 8.75 | 7.52 | 10.98 | 11.44 |
| encoding | 4.44 | 1.81 | 4.73 | 1.61 |
| crypto | 20.36 | 0.51 | 1.53 | 0.52 |
| json | 4.27 | 1.37 | 1.5 | 0.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 130 | 120 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 50 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.8 | 29 | 34.9 | 5.9 |
| calls | 38.9 | 31.2 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.9 |
| mathk | 40 | 30.7 | 36.9 | 5.9 |
| url | 43.3 | 38.2 | 41.8 | 11.9 |
| encoding | 42 | 30.8 | 38.3 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 5.9 |
| json | 40.6 | 29.3 | 36.9 | 5.9 |
