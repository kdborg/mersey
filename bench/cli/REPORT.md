# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.52 | 97.12 | 95.6 | 87.83 |
| calls | 122.44 | 31.25 | 28.06 | 21.98 |
| fcompute | 103.03 | 102.57 | 104.13 | 102.52 |
| mathk | 13.34 | 13.98 | 13.27 | 15.67 |
| url | 8.65 | 7.69 | 10.94 | 23.05 |
| encoding | 4.37 | 1.61 | 4.29 | 3.99 |
| crypto | 18.73 | 0.46 | 1.41 | 3.04 |
| json | 3.93 | 1.28 | 1.43 | 0.95 |

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
| compute | 38.7 | 29 | 34.7 | 5.9 |
| calls | 38.8 | 31.1 | 34.7 | 5.8 |
| fcompute | 40.4 | 29 | 36.7 | 5.8 |
| mathk | 39.8 | 30.7 | 36.6 | 5.9 |
| url | 43.1 | 38.1 | 41.5 | 7 |
| encoding | 41.9 | 30.7 | 38.1 | 4.4 |
| crypto | 48.2 | 26.9 | 35.3 | 4.4 |
| json | 40.5 | 29.3 | 36.7 | 5.9 |
