# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 91.67 | 97.59 | 97.76 | 89.44 |
| calls | 123.31 | 31.3 | 28.91 | 22.58 |
| fcompute | 104.62 | 103.93 | 104.57 | 103.82 |
| mathk | 13.59 | 14.21 | 13.22 | 28.67 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 130 | 110 | 110 | 100 |
| calls | 150 | 40 | 40 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 30 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.8 | 29.1 | 34.8 | 5.9 |
| calls | 39 | 31.2 | 34.8 | 5.8 |
| fcompute | 40.6 | 29.1 | 36.8 | 5.5 |
| mathk | 40 | 30.8 | 36.9 | 5.5 |
