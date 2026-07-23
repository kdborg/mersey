# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 88.03 | 96.12 | 96.11 | 88.03 |
| calls | 121.66 | 31.06 | 27.94 | 21.71 |
| fcompute | 102.85 | 102.7 | 103.97 | 102.3 |
| mathk | 13.31 | 14.06 | 13.52 | 29.2 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 120 |
| calls | 140 | 40 | 30 | 50 |
| fcompute | 120 | 110 | 110 | 130 |
| mathk | 30 | 20 | 20 | 60 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.7 | 29.1 | 34.8 | 24.3 |
| calls | 38.8 | 31.2 | 34.8 | 24.3 |
| fcompute | 40.6 | 29.1 | 36.7 | 24.1 |
| mathk | 39.9 | 30.8 | 36.9 | 24.2 |
