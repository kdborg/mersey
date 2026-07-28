# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 89.16 | 97.34 | 96.92 | 88.83 |
| calls | 121.59 | 31.01 | 28.36 | 22.02 |
| fcompute | 103.63 | 104.35 | 105.56 | 103.37 |
| mathk | 13.69 | 14.03 | 13.42 | 15.62 |
| url | 8.82 | 7.54 | 11.19 | 427.22 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 450 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29 | 34.7 | 5.9 |
| calls | 38.8 | 31.2 | 34.7 | 5.9 |
| fcompute | 40.4 | 29 | 36.6 | 5.5 |
| mathk | 39.8 | 30.7 | 36.8 | 5.6 |
| url | 43.3 | 38.2 | 41.6 | 6.7 |
