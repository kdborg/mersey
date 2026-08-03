// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Rewrite `report.html`'s baked DATA block from `results.json`.
//
// The numbers in that page used to be maintained by hand, and had drifted: it
// carried `strings: mersey 55.10` and `reconcile: 56.30` against measured 42.70
// and 53.32, and had no rows at all for the two workloads added after it was
// written. `bench/web` has had a generator for this since it existed; this is
// the same thing for `bench/cli`.
//
// Only the numbers are generated. The WORKLOADS map above the block is prose —
// what each workload *is* — and stays hand-written; a workload missing from it
// is reported rather than invented.
//
//   node bench/cli/run.mjs            # measure  -> results.json + REPORT.md
//   node bench/cli/gen-report-data.mjs   # then    -> report.html's DATA block
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const results = JSON.parse(readFileSync(join(here, "results.json"), "utf8"));
const htmlPath = join(here, "report.html");
let html = readFileSync(htmlPath, "utf8");

const keys = results.runtimes.map((r) => r.key);
const METRICS = [["work", "work"], ["wall", "wall"], ["rss", "rssMiB"]];

// Widest workload name, so the generated block lines up the way the hand-written
// one did — this file is read by people, and a diff of realigned columns is not.
const pad = Math.max(...results.rows.map((r) => r.wl.length)) + 2;

const block = [];
block.push("  const DATA = {");
for (const [metric, field] of METRICS) {
  block.push(`    ${metric}: {`);
  for (const row of results.rows) {
    const cells = keys
      .map((k) => `${k}: ${row.legs[k]?.[field] ?? "null"}`)
      .join(", ");
    block.push(`      ${(row.wl + ":").padEnd(pad)}{ ${cells} },`);
  }
  block.push("    },");
}
block.push("  };");

const start = html.indexOf("  const DATA = {");
if (start < 0) throw new Error("report.html: no `const DATA = {` to replace");
const end = html.indexOf("\n  };", start);
if (end < 0) throw new Error("report.html: DATA block is not terminated");
html = html.slice(0, start) + block.join("\n") + html.slice(end + "\n  };".length);

// The prose map is not generated. Say so rather than filling it in.
const described = new Set(
  [...html.matchAll(/^\s{4}(\w+):\s*\{ title:/gm)].map((m) => m[1]),
);
const missing = results.rows.map((r) => r.wl).filter((w) => !described.has(w));

writeFileSync(htmlPath, html);
process.stdout.write(
  `Wrote report.html DATA: ${results.rows.length} workloads × ${keys.length} runtimes × ${METRICS.length} metrics\n` +
    (missing.length
      ? `WARNING: no WORKLOADS description for: ${missing.join(", ")} — add one by hand\n`
      : ""),
);
