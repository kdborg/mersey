// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Web-platform benchmark harness.
//
// For each workload (storage, json, url, crypto, canvas, dom), in each browser
// (Chromium, Firefox), it measures two implementations that run in a STOCK
// browser:
//   - js    : the workload written in plain JavaScript
//   - poly  : the same workload written in Mersey, executed by the engine
//             compiled to WASM (the polyfill) through the universal bridge
//
// The third implementation — "native", the engine hosted inside the browser
// fork — is measured separately by run-native.mjs (it needs the fork binary,
// not a stock browser). Numbers merge in the report.
//
// Metric 1 (performance): wall-clock of the workload loop, self-timed in the
// language (performance.now / time.monotonic), excluding engine startup.
// Metric 2 (memory): RSS delta of the whole browser process tree, workload
// page vs a blank page in the same browser — a coarse "tab memory" proxy that
// is comparable across browsers and captures the WASM heap (which the JS-heap
// counter does not).
import { readFile, writeFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { startServer } from "./server.mjs";
import { treeMemoryByCmdline } from "./host-mem.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

// A workload's checksum is not always a number: `fcompute` and `mathk` self-check
// with a boolean, because float bit parity across two independent codegens is not
// guaranteed. Parsing with `Number()` turned those into NaN -> `null` in the
// results file, which quietly threw away the correctness proof for two of the
// eight compute workloads on every browser leg (and made every parity check
// compare null to null and pass). Keep numbers as numbers so the file shape does
// not change; keep anything else as the token the workload printed.
const parseChecksum = (raw) => (/^-?\d+$/.test(raw) ? Number(raw) : raw);

// Time and memory are independent measurements of the same run set, so each
// gets its own median. Reporting `samples[medianByTime].rss` — which is what
// this did — picks one arbitrary memory reading out of the repeats, and browser
// footprint swings by tens of MiB between launches; that is how a workload's
// delta against the blank baseline came out NEGATIVE often enough to matter.
const medianRss = (samples) => {
  const v = samples.map((s) => s.rss).filter((x) => x != null).sort((a, b) => a - b);
  return v.length ? v[Math.floor(v.length / 2)] : null;
};

const here = dirname(fileURLToPath(import.meta.url));
const PAGE = "bench/web/pages";

const WORKLOADS = process.env.WL
  ? process.env.WL.split(",")
  : (await readdir(join(here, "mersey")))
      .filter((f) => f.endsWith(".mersey"))
      .map((f) => f.replace(/\.mersey$/, ""))
      .sort();

const REPEATS = Number(process.env.REPEATS ?? 3);
const PAGE_SIZE = 4096;

// Sum PSS (KiB) over every process whose command line names this browser's
// executable directory — i.e. the whole browser process tree (Playwright does
// not expose the pid, so we identify the tree by its binary path). PSS, not
// RSS: shared libraries mapped into every renderer are counted once, so a
// delta that spans a new renderer process is not inflated by libxul/libchrome.
const browserRss = (match) => treeMemoryByCmdline(match);

async function runOne(browser, impl, wl, origin, rssMatch) {
  const context = await browser.newContext();

  // Baseline RSS on a blank page.
  const blank = await context.newPage();
  await blank.goto("about:blank");
  await new Promise((r) => setTimeout(r, 300));
  const rssBefore = await browserRss(rssMatch);

  const page = await context.newPage();
  let result = null;
  page.on("console", (msg) => {
    const m = /RESULT (\S+) ([\d.]+) ([^\s",]+)(?: heap=(\d+))?/.exec(msg.text());
    if (m) result = { ms: Number(m[2]), checksum: parseChecksum(m[3]), heap: Number(m[4] ?? 0) };
  });
  page.on("pageerror", (e) => console.error(`  [pageerror ${impl}/${wl}] ${e.message}`));

  await page.goto(`${origin}/${PAGE}/${impl}.html?wl=${wl}`);
  // Wait for the RESULT line (workloads self-report on completion).
  const deadline = Date.now() + 30000;
  while (!result && Date.now() < deadline) await new Promise((r) => setTimeout(r, 50));

  await new Promise((r) => setTimeout(r, 300));
  const rssAfter = await browserRss(rssMatch);

  await page.close();
  await blank.close();
  await context.close();

  if (!result) return { ms: null, checksum: null, rss: null, heap: null };
  return { ...result, rss: rssAfter - rssBefore };
}

async function measure(launcher, label, origin, rssMatch) {
  const rows = [];
  const browser = await launcher.launch({ headless: true });
  for (const wl of WORKLOADS) {
    for (const impl of ["js", "poly"]) {
      // Mersey-only workloads (calls, fcompute, mathk…) have no js/ twin;
      // don't burn the 30s result deadline ×3 discovering that.
      if (impl === "js" && !(await stat(join(here, "js", `${wl}.js`)).catch(() => null))) {
        rows.push({ browser: label, impl, wl, ms: null });
        continue;
      }
      const samples = [];
      for (let r = 0; r < REPEATS; r++) {
        const one = await runOne(browser, impl, wl, origin, rssMatch);
        if (one.ms != null) samples.push(one);
      }
      if (samples.length === 0) {
        console.log(`  ${label} ${impl.padEnd(4)} ${wl.padEnd(8)} — no result`);
        rows.push({ browser: label, impl, wl, ms: null });
        continue;
      }
      samples.sort((a, b) => a.ms - b.ms);
      const med = samples[Math.floor(samples.length / 2)];
      const medRss = medianRss(samples);
      console.log(
        `  ${label} ${impl.padEnd(4)} ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(medRss).padStart(6)} KiB`,
      );
      rows.push({ browser: label, impl, wl, ms: med.ms, rss: medRss, heap: med.heap, checksum: med.checksum });
    }
  }
  await browser.close();
  return rows;
}

const pw = await import(
  join(here, "..", "..", "web", "node_modules", "playwright", "index.js")
);
const { chromium, firefox } = pw.default ?? pw;
const { server, port } = await startServer();
const origin = `http://localhost:${port}`;
console.log(`serving ${origin}, workloads: ${WORKLOADS.join(", ")}\n`);

const all = [];
console.log("Chromium:");
all.push(...(await measure(chromium, "chromium", origin, "ms-playwright/chromium")));
console.log("Firefox:");
all.push(...(await measure(firefox, "firefox", origin, "ms-playwright/firefox")));

server.close();
// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl) pairs this run produced.

const merged = await mergeRows(here, "results.stock.json", tagRows(all));
await writeFile(join(here, "results.stock.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${all.length} rows to bench/web/results.stock.json`);
