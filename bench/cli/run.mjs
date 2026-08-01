// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Cross-runtime CLI benchmark: node vs bun vs deno vs the Mersey CLI.
//
// The comparable, browser-free subset of the bench/web suite — the four
// pure-compute workloads whose Mersey twin imports only `std:` (compute, calls,
// fcompute, mathk). Each JS twin (bench/cli/js/<wl>.js) is self-contained and
// line-for-line equivalent to its Mersey twin (bench/web/mersey/<wl>.mersey),
// and every leg prints `RESULT <wl> <ms> <checksum>`.
//
// Two times are reported, both meaningful for a command-line tool:
//   - work ms : self-timed steady-state kernel (after an in-process warm-up),
//               excludes process startup — apples-to-apples with the web report.
//   - wall ms : whole `/usr/bin/time` invocation — startup + JIT warm-up +
//               work, i.e. what you actually wait for at the shell.
// Memory is peak RSS of the process (macOS `time -l` "maximum resident set
// size", in bytes). Checksums are asserted identical across all four runtimes.
//
// macOS only (parses BSD `time -l`). Run from the repo root on the Mac:
//   node bench/cli/run.mjs                 # all workloads
//   WL=compute,calls node bench/cli/run.mjs
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { writeFileSync, existsSync } from "node:fs";

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, "..", "..");
const HOME = process.env.HOME;
const REPEATS = Number(process.env.REPEATS ?? 5);

const WORKLOADS = (process.env.WL ? process.env.WL.split(",") : ["compute", "calls", "fcompute", "mathk", "url", "encoding", "crypto", "json", "strings", "reconcile"]);

// Resolve each runtime; skip any that isn't installed rather than aborting.
const first = (...cands) => cands.find((p) => p && existsSync(p));
const RUNTIMES = [
  { key: "node", label: "Node.js", bin: "node", argv: (js) => [js] },
  { key: "bun", label: "Bun", bin: first(`${HOME}/.bun/bin/bun`, "/opt/homebrew/bin/bun"), argv: (js) => [js] },
  { key: "deno", label: "Deno", bin: first(`${HOME}/.deno/bin/deno`, "/opt/homebrew/bin/deno"), argv: (js) => ["run", js] },
  {
    key: "mersey", label: "Mersey CLI",
    bin: join(repo, "target/release/mersey"),
    // Prefer a CLI-specific twin (bench/cli/mersey/<wl>.mersey) — the web twins
    // import from browser:dom, which the backend has no bridge for; the CLI twin
    // uses the std-library equivalent (e.g. std:url) with the same checksum.
    // --allow-random is harmless for the workloads that don't use it and lets
    // the crypto twin reach std:random; other std caps can be added the same way.
    argv: (_js, wl) => ["run", "--allow-random", first(join(repo, "bench/cli/mersey", `${wl}.mersey`),
      join(repo, "bench/web/mersey", `${wl}.mersey`))],
    isMersey: true,
  },
].filter((r) => r.bin);

const jsPath = (wl) => join(here, "js", `${wl}.js`);

// One measured invocation: `/usr/bin/time -l <bin> <argv...>`.
function once(bin, args) {
  return new Promise((resolve) => {
    const p = spawn("/usr/bin/time", ["-l", bin, ...args], { cwd: repo });
    let out = "", err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    p.on("close", (code) => {
      const rline = out.split("\n").find((l) => l.startsWith("RESULT "));
      const result = rline ? rline.trim().split(/\s+/) : null; // RESULT wl ms checksum
      const real = err.match(/([\d.]+)\s+real/);
      const rss = err.match(/(\d+)\s+maximum resident set size/);
      resolve({
        code,
        work: result ? Number(result[2]) : null,
        checksum: result ? result[3] : null,
        wall: real ? Number(real[1]) * 1000 : null, // s -> ms
        rss: rss ? Number(rss[1]) : null, // bytes (macOS)
        err: (code === 0 && rline) ? "" : (err.split("\n").slice(-6).join("\n") || out.slice(-300)),
      });
    });
  });
}

const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s[(s.length - 1) >> 1]; };
const min = (xs) => Math.min(...xs);

const rows = [];
for (const wl of WORKLOADS) {
  const perWl = { wl, checksums: {}, legs: {} };
  for (const rt of RUNTIMES) {
    const args = rt.argv(jsPath(wl), wl);
    const runs = [];
    let failure = null;
    for (let i = 0; i < REPEATS; i++) {
      const r = await once(rt.bin, args);
      if (r.code !== 0 || r.work == null) { failure = r.err || `exit ${r.code}`; break; }
      runs.push(r);
    }
    if (failure) {
      perWl.legs[rt.key] = { error: failure };
      process.stderr.write(`  ${wl}/${rt.key} FAILED: ${failure.replace(/\n/g, " ")}\n`);
      continue;
    }
    perWl.checksums[rt.key] = runs[0].checksum;
    perWl.legs[rt.key] = {
      work: +min(runs.map((r) => r.work)).toFixed(2),
      wall: +min(runs.map((r) => r.wall)).toFixed(1),
      rssMiB: +(median(runs.map((r) => r.rss)) / (1024 * 1024)).toFixed(1),
    };
    process.stderr.write(`  ${wl}/${rt.key}: work=${perWl.legs[rt.key].work}ms wall=${perWl.legs[rt.key].wall}ms rss=${perWl.legs[rt.key].rssMiB}MiB cs=${runs[0].checksum}\n`);
  }
  // Checksum parity across runtimes that produced one.
  const cs = Object.values(perWl.checksums);
  perWl.checksumOk = cs.length > 0 && cs.every((c) => c === cs[0]);
  if (!perWl.checksumOk) process.stderr.write(`  !! ${wl} CHECKSUM MISMATCH: ${JSON.stringify(perWl.checksums)}\n`);
  rows.push(perWl);
}

// ---- render ---------------------------------------------------------------
const metric = (rows, pick) => {
  const legs = RUNTIMES.map((r) => r.key);
  const head = `| workload | ${RUNTIMES.map((r) => r.label).join(" | ")} |`;
  const sep = `|${"---|".repeat(RUNTIMES.length + 1)}`;
  const body = rows.map((row) => {
    const cells = legs.map((k) => {
      const leg = row.legs[k];
      return leg && leg[pick] != null ? String(leg[pick]) : (leg && leg.error ? "ERR" : "—");
    });
    return `| ${row.wl} | ${cells.join(" | ")} |`;
  });
  return [head, sep, ...body].join("\n");
};

const report =
  `# CLI runtime comparison — node vs bun vs deno vs Mersey CLI\n\n` +
  `macOS (${process.arch}). ${REPEATS} repeats per cell; work/wall = min, rss = median of peak. ` +
  `Checksums identical across all runtimes: ${rows.every((r) => r.checksumOk) ? "yes ✓" : "NO — see below"}.\n\n` +
  `## Work — self-timed steady-state kernel (ms, lower is better)\n\n` + metric(rows, "work") + "\n\n" +
  `## Wall — whole CLI invocation incl. startup + warm-up (ms)\n\n` + metric(rows, "wall") + "\n\n" +
  `## Peak RSS (MiB)\n\n` + metric(rows, "rssMiB") + "\n";

writeFileSync(join(here, "REPORT.md"), report);
writeFileSync(join(here, "results.json"), JSON.stringify({ arch: process.arch, repeats: REPEATS, rows, runtimes: RUNTIMES.map((r) => ({ key: r.key, label: r.label, bin: r.bin })) }, null, 2));
process.stdout.write("\n" + report + "\nWrote bench/cli/REPORT.md and bench/cli/results.json\n");
