// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// REAL BROWSER: the loader's Stage B contract. A Mersey-native browser sets
// `globalThis.merseyNative` before any script runs; the polyfill loader must
// then stand down entirely — no engine fetch, no execution (the native
// engine already ran the scripts; a second run would double-execute).
// The control run (no marker) proves the same page works via the polyfill.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript",
  ".wasm": "application/wasm", ".mersey": "text/plain" };
const server = createServer(async (req, res) => {
  try {
    const path = join(root, decodeURIComponent(new URL(req.url, "http://x").pathname));
    const body = await readFile(path);
    res.writeHead(200, { "content-type": MIME[extname(path)] ?? "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((r) => server.listen(0, r));
const origin = `http://localhost:${server.address().port}`;

const pw = await import(join(root, "node_modules", "playwright", "index.js"));
const { chromium } = pw.default ?? pw;
const browser = await chromium.launch({ headless: true });

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  REAL BROWSER · ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};

async function load(marker) {
  const context = await browser.newContext();
  const page = await context.newPage();
  if (marker) {
    await page.addInitScript(() => { globalThis.merseyNative = true; });
  }
  let wasmFetched = false;
  page.on("request", (r) => { if (r.url().endsWith(".wasm")) wasmFetched = true; });
  const logs = [];
  page.on("console", (m) => logs.push(m.text()));
  await page.goto(`${origin}/todo.html`);
  await page.waitForTimeout(2500);
  await context.close();
  return { wasmFetched, ready: logs.some((l) => l.includes("todo app ready")) };
}

const native = await load(true);
check("native marker: engine wasm never fetched", !native.wasmFetched);
check("native marker: loader executed nothing", !native.ready);

const poly = await load(false);
check("no marker: the same page runs via the polyfill", poly.ready);

await browser.close();
server.close();
if (failures) process.exit(1);
console.log("\nNative stand-down: passed");
