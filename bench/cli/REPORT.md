# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.65 | 96.57 | 96.56 | 87.4 |
| calls | 121.7 | 31.15 | 27.83 | 22.03 |
| fcompute | 102.9 | 102.77 | 104.26 | 102.74 |
| mathk | 13.44 | 14.32 | 13.1 | 15.73 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 90 |
| calls | 140 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 6 |
| calls | 38.9 | 31.2 | 34.8 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.5 |
| mathk | 39.8 | 30.7 | 36.9 | 5.6 |
