// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Engine-only leg of the web-platform benchmark: the wasm engine over a
// deterministic stub realm in Node (engine-child.mjs) — no browser process,
// no renderer noise. This is the leg the perf regression tests gate on
// (perf-test.mjs); it measures the engine + bridge cost of each technology
// with the host reduced to its cheapest faithful form.
//
// Memory is the child's peak RSS (VmHWM) minus the median blank child (engine
// instantiated, nothing run), so the number is the workload's own footprint;
// `heap` is the engine's wasm linear memory after the run.
//
//   WL=storage,json node run-engine.mjs     filter workloads
import { readFile, writeFile, readdir } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { startServer } from "./server.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const REPEATS = Number(process.env.REPEATS ?? 3);

const WORKLOADS = process.env.WL
  ? process.env.WL.split(",")
  : (await readdir(join(here, "mersey")))
      .filter((f) => f.endsWith(".mersey"))
      .map((f) => f.replace(/\.mersey$/, ""))
      .sort();

export async function runChild(arg, env = {}) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [join(here, "engine-child.mjs"), arg], {
      env: { ...process.env, ...env },
    });
    let out = "";
    let err = "";
    child.stdout.on("data", (b) => (out += b.toString()));
    child.stderr.on("data", (b) => (err += b.toString()));
    child.on("exit", (code) => {
      const r = /RESULT (\S+) ([\d.]+) (\S+)/.exec(out);
      const m = /MEMSTAT vmhwm=(\d+) wasmheap=(\d+)/.exec(out);
      resolve({
        code,
        ms: r ? Number(r[2]) : null,
        checksum: r ? Number(r[3]) : null,
        vmhwm: m ? Number(m[1]) : null,
        wasmheap: m ? Number(m[2]) : null,
        err,
      });
    });
  });
}

// Blank baseline: engine + realm instantiated, nothing run. Median of 3.
export async function blankBaseline() {
  const blanks = [];
  for (let i = 0; i < 3; i++) blanks.push((await runChild("--blank")).vmhwm ?? 0);
  blanks.sort((a, b) => a - b);
  return blanks[1];
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
  const { server, port } = await startServer();
  const env = { MERSEY_ECHO_BASE: `http://localhost:${port}` };

  const baseRss = await blankBaseline();
  console.log(`engine  baseline blank vmhwm ${baseRss} KiB\n`);

  const rows = [];
  for (const wl of WORKLOADS) {
    const samples = [];
    for (let r = 0; r < REPEATS; r++) {
      const s = await runChild(wl, env);
      if (s.ms != null) samples.push(s);
      else if (r === 0) console.error(`  engine ${wl}: no result${s.err ? ` — ${s.err.split("\n")[0]}` : ""}`);
    }
    if (samples.length === 0) {
      console.log(`  engine ${wl.padEnd(8)} — no result`);
      rows.push({ browser: "engine", impl: "wasm", wl, ms: null });
      continue;
    }
    samples.sort((a, b) => a.ms - b.ms);
    const med = samples[Math.floor(samples.length / 2)];
    const rss = Math.max(0, (med.vmhwm ?? 0) - baseRss);
    console.log(
      `  engine ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(rss).padStart(6)} KiB   heap ${String(med.wasmheap).padStart(9)} B   (n=${samples.length})`,
    );
    rows.push({
      browser: "engine",
      impl: "wasm",
      wl,
      ms: med.ms,
      rss,
      heap: med.wasmheap,
      checksum: med.checksum,
    });
  }

  server.close();

  // A filtered run (WL=…) must not clobber rows it did not measure: merge into
  // the existing file, replacing only (impl, wl) pairs this run produced.
  // Platform is part of the key, so a macOS run leaves the Linux rows intact.
  const merged = await mergeRows(here, "results.engine.json", tagRows(rows));
  await writeFile(join(here, "results.engine.json"), JSON.stringify(merged, null, 2));
  console.log(`\nwrote ${rows.length} rows to bench/web/results.engine.json`);
}
