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
import { readFile, writeFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { startServer } from "./server.mjs";
import { tagRows } from "./rows.mjs";
import { treeMemoryByCmdline } from "./host-mem.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const PAGE = "bench/web/pages";

const WORKLOADS = (await readdir(join(here, "mersey")))
  .filter((f) => f.endsWith(".mersey"))
  .map((f) => f.replace(/\.mersey$/, ""))
  .sort();

const REPEATS = 3;
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
    const m = /RESULT (\S+) ([\d.]+) (-?\d+)(?: heap=(\d+))?/.exec(msg.text());
    if (m) result = { ms: Number(m[2]), checksum: Number(m[3]), heap: Number(m[4] ?? 0) };
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
    for (const impl of ["tjs"]) {
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
      console.log(
        `  ${label} ${impl.padEnd(4)} ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(med.rss).padStart(6)} KiB`,
      );
      rows.push({ browser: label, impl, wl, ms: med.ms, rss: med.rss, heap: med.heap, checksum: med.checksum });
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
await writeFile(join(here, "results.tjs.json"), JSON.stringify(tagRows(all), null, 2));
console.log(`\nwrote ${all.length} rows to bench/web/results.tjs.json`);
