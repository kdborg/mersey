# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 100.02 | 109.02 | 108.9 | 98.55 |
| calls | 138.26 | 35.05 | 31.88 | 24.74 |
| fcompute | 116.05 | 116.22 | 118.34 | 116.01 |
| mathk | 14.73 | 15.21 | 14.65 | 14.22 |
| url | 9.33 | 7.81 | 11.54 | 21.83 |
| encoding | 4.82 | 1.83 | 4.37 | 1.65 |
| crypto | 20.48 | 0.53 | 1.6 | 0.56 |
| json | 4.25 | 1.41 | 1.46 | 0.82 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 140 | 130 | 130 | 110 |
| calls | 160 | 40 | 40 | 20 |
| fcompute | 140 | 120 | 130 | 110 |
| mathk | 40 | 20 | 20 | 10 |
| url | 40 | 10 | 20 | 20 |
| encoding | 30 | 10 | 10 | 0 |
| crypto | 50 | 10 | 10 | 0 |
| json | 30 | 10 | 10 | 0 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.9 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 37 | 5.9 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42.1 | 30.7 | 38.4 | 7.3 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 5.9 |
