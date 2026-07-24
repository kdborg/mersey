// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Service Worker written in Mersey, running in a real browser: it intercepts
// a fetch and serves the response itself.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";
import { chromium } from "playwright";
const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html":"text/html", ".js":"text/javascript", ".mjs":"text/javascript",
               ".wasm":"application/wasm", ".mersey":"text/plain" };
const server = createServer(async (req, res) => {
  const path = req.url.split("?")[0];
  if (path === "/hello-from-mersey") {   // the SW must beat the network here
    res.writeHead(200, { "content-type": "text/plain" });
    return res.end("SERVED BY THE NETWORK (service worker did not intercept)");
  }
  try {
    const f = join(webRoot, path);
    res.writeHead(200, { "content-type": MIME[extname(f)] ?? "text/plain" });
    res.end(await readFile(f));
  } catch { res.writeHead(404).end("nf"); }
});
await new Promise(r => server.listen(0, r));
const base = `http://localhost:${server.address().port}`;
const browser = await chromium.launch({ args: ["--no-sandbox"] });
const ctx = await browser.newContext();
const page = await ctx.newPage();
const logs = [], errs = [];
page.on("console", m => logs.push(m.text()));
page.on("pageerror", e => errs.push(String(e)));
// service-worker console output arrives on the worker, not the page
ctx.on("serviceworker", (w) => {
  w.on("console", (m) => logs.push("[sw] " + m.text()));
});
// First load registers the worker. A page is only *controlled* by a service
// worker once it has one at navigation time, so reload before asserting —
// this is standard SW behaviour, not a Mersey quirk.
await page.goto(`${base}/sw.html`, { waitUntil: "networkidle" });
await page.waitForTimeout(2000);
await page.reload({ waitUntil: "networkidle" });
await page.waitForTimeout(2000);
const out = await page.$eval("#out", e => e.textContent);
await browser.close(); server.close();

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
check("sw: no page errors", errs.length === 0, errs.join("; "));
check("REAL BROWSER · Mersey service worker registered",
      logs.some(l => /^sw: registered$/.test(l)), logs.join(" | "));
check("REAL BROWSER · service worker intercepted the fetch",
      out === "served by a Mersey service worker 🌊", out);
if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nMersey service worker: passed");
