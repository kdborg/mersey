# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.98 | 96.73 | 96.15 | 88.25 |
| calls | 122.38 | 31.2 | 28.09 | 22.01 |
| fcompute | 102.15 | 103.8 | 104.69 | 102.79 |
| mathk | 13.26 | 13.93 | 13.21 | 12.77 |
| url | 8.47 | 7.55 | 10.69 | 20.74 |
| encoding | 4.5 | 1.61 | 4.18 | 1.53 |
| crypto | 19.41 | 0.46 | 1.53 | 0.5 |
| json | 3.99 | 1.29 | 1.42 | 0.75 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 0 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.8 |
| calls | 38.9 | 31.1 | 34.9 | 5.8 |
| fcompute | 40.6 | 29 | 36.8 | 5.8 |
| mathk | 39.9 | 30.7 | 36.9 | 5.8 |
| url | 43.2 | 38.2 | 41.7 | 7 |
| encoding | 42 | 30.7 | 38.3 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.8 |
| json | 40.5 | 29.3 | 36.9 | 5.8 |
