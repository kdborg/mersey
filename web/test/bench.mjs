// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// A/B benchmark: generated bindings vs reflective dispatch, in a real browser.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join, extname } from "node:path";
import { chromium } from "playwright";

const webRoot = new URL("..", import.meta.url).pathname;
const MIME = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript",
               ".wasm": "application/wasm", ".mersey": "text/plain" };
const server = createServer(async (req, res) => {
  try {
    const f = join(webRoot, req.url === "/" ? "/index.html" : req.url.split("?")[0]);
    res.writeHead(200, { "content-type": MIME[extname(f)] ?? "text/plain" });
    res.end(await readFile(f));
  } catch { res.writeHead(404).end("nf"); }
});
await new Promise((r) => server.listen(0, r));
const base = `http://localhost:${server.address().port}`;
const browser = await chromium.launch({ args: ["--no-sandbox"] });

async function run(mode) {
  const page = await browser.newPage();
  const logs = [];
  page.on("console", (m) => logs.push(m.text()));
  if (mode.noBindings) {
    await page.addInitScript(() => { window.__MERSEY_NO_BINDINGS = true; });
  }
  if (mode.noFastPath) {
    await page.addInitScript(() => { window.__MERSEY_NO_FASTPATH = true; });
  }
  await page.goto(`${base}/bench.html`, { waitUntil: "networkidle" });
  await page.waitForTimeout(2500);
  await page.close();
  const nums = logs.filter((l) => l.startsWith("BENCH"))
                   .map((l) => Number(l.match(/in (\d+)ms/)[1]));
  return nums;
}

// Three configurations, to see what actually costs time.
const full = await run({});                                   // bindings + fast marshalling
const noBind = await run({ noBindings: true });               // reflection + fast marshalling
const baseline = await run({ noBindings: true, noFastPath: true }); // both off (original)
await browser.close();
server.close();

const row = (label, i) =>
  `${label.padEnd(34)} writes ${String(full[i] ?? 0).padStart(3)}ms`;
const pct = (a, b) => `${(((b - a) / b) * 100).toFixed(0)}%`;
console.log("20,000 DOM property writes / 20,000 method calls, real Chromium:\n");
console.log(`  baseline (reflection + JSON)      ${baseline[0]}ms / ${baseline[1]}ms`);
console.log(`  + marshalling fast paths          ${noBind[0]}ms / ${noBind[1]}ms   ` +
            `(${pct(noBind[0], baseline[0])} / ${pct(noBind[1], baseline[1])} faster)`);
console.log(`  + generated bindings (both on)    ${full[0]}ms / ${full[1]}ms   ` +
            `(${pct(full[0], baseline[0])} / ${pct(full[1], baseline[1])} faster than baseline)`);
