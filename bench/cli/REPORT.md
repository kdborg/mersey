# CLI runtime comparison — node vs bun vs deno vs Mersey CLI

macOS (arm64). 5 repeats per cell; work/wall = min, rss = median of peak. Checksums identical across all runtimes: yes ✓.

## Work — self-timed steady-state kernel (ms, lower is better)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 87.7 | 96.44 | 96.1 | 88.56 |
| calls | 123.97 | 31.22 | 28.11 | 21.94 |
| fcompute | 103.95 | 103.81 | 102.63 | 103.3 |
| mathk | 13.41 | 13.98 | 13.23 | 15.62 |
| url | 8.82 | 7.34 | 10.94 | 413.08 |
| encoding | 4.43 | 1.61 | 4.1 | 7.22 |
| crypto | 18.73 | 0.48 | 1.55 | 21.14 |

## Wall — whole CLI invocation incl. startup + warm-up (ms)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 120 | 110 | 110 | 100 |
| calls | 150 | 40 | 30 | 20 |
| fcompute | 130 | 110 | 110 | 100 |
| mathk | 40 | 20 | 20 | 10 |
| url | 30 | 10 | 20 | 430 |
| encoding | 30 | 10 | 10 | 10 |
| crypto | 40 | 10 | 10 | 20 |

## Peak RSS (MiB)

| workload | Node.js | Bun | Deno | Mersey CLI |
|---|---|---|---|---|
| compute | 38.6 | 29 | 34.8 | 5.9 |
| calls | 38.8 | 31.1 | 34.9 | 5.9 |
| fcompute | 40.5 | 29 | 36.8 | 5.5 |
| mathk | 39.8 | 30.7 | 36.7 | 5.6 |
| url | 43.2 | 38.1 | 41.6 | 6.7 |
| encoding | 41.8 | 30.7 | 38.2 | 3.9 |
| crypto | 48.2 | 26.9 | 35.2 | 3.8 |
