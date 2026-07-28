# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.91 | 95.87 | 96.17 | 86.74 |
| calls | 121.42 | 33.06 | 28.18 | 21.91 |
| fcompute | 102.75 | 102.85 | 103.78 | 102.4 |
| mathk | 13.33 | 13.9 | 13.09 | 15.56 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 120 | 110 | 110 | 100 |
| mathk | 30 | 20 | 20 | 10 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.2 | 34.7 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.5 |
| mathk | 39.8 | 30.7 | 36.9 | 5.6 |
