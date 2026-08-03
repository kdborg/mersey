# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 90.17 | 97.92 | 98.67 | 89.89 |
| calls | 125.04 | 31.92 | 28.79 | 22.39 |
| fcompute | 105.38 | 105.3 | 106.5 | 103.67 |
| mathk | 13.64 | 14.26 | 13.76 | 12.7 |
| url | 8.81 | 7.62 | 11.1 | 9.7 |
| encoding | 4.6 | 1.69 | 4.22 | 1.51 |
| crypto | 18.86 | 0.49 | 1.52 | 0.52 |
| json | 4.04 | 1.35 | 1.54 | 0.75 |
| strings | 27.25 | 14.99 | 15.3 | 43.56 |
| reconcile | 11.56 | 5.57 | 6.14 | 58.74 |
| csv | 14.5 | 11.2 | 11.42 | 68.79 |
| path | 30.54 | 16.25 | 27.03 | 136.46 |
| semver | 41.13 | 33.26 | 30.59 | 128.97 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 10 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 40 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |
| strings | 50 | 20 | 20 | 50 |
| reconcile | 40 | 10 | 10 | 70 |
| csv | 40 | 20 | 20 | 70 |
| path | 60 | 20 | 40 | 160 |
| semver | 70 | 40 | 40 | 160 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.9 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6 |
| fcompute | 40.5 | 29 | 36.8 | 5.9 |
| mathk | 40 | 30.7 | 37 | 6 |
| url | 43.2 | 38.2 | 41.9 | 8.8 |
| encoding | 42.1 | 30.7 | 38.2 | 7.4 |
| crypto | 48.3 | 26.9 | 35.3 | 6 |
| json | 40.6 | 29.3 | 36.9 | 6 |
| strings | 41.8 | 52 | 39.3 | 8.6 |
| reconcile | 47.9 | 55.2 | 42.7 | 10.3 |
| csv | 44 | 67.1 | 47.2 | 9.3 |
| path | 48.7 | 43.6 | 46.6 | 9.7 |
| semver | 49.9 | 85.7 | 44.8 | 10.8 |
