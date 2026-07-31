# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 9 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.76 | 96.6 | 97.1 | 88.87 |
| calls | 123.59 | 31.22 | 28.59 | 22.22 |
| fcompute | 104.38 | 104.4 | 104.88 | 104.11 |
| mathk | 13.57 | 14.14 | 13.14 | 12.76 |
| url | 8.55 | 7.76 | 11.05 | 20.45 |
| encoding | 4.46 | 1.63 | 4.26 | 1.54 |
| crypto | 19.26 | 0.48 | 1.43 | 0.5 |
| json | 4.03 | 1.3 | 1.44 | 0.74 |

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
| compute | 38.7 | 29 | 34.9 | 6 |
| calls | 38.9 | 31.2 | 34.9 | 6 |
| fcompute | 40.6 | 29 | 36.9 | 5.9 |
| mathk | 39.9 | 30.7 | 36.9 | 6 |
| url | 43.2 | 38.2 | 41.8 | 7 |
| encoding | 42 | 30.7 | 38.3 | 7.4 |
| crypto | 48.2 | 26.9 | 35.3 | 5.9 |
| json | 40.5 | 29.3 | 36.9 | 6 |
