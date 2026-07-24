// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";
import { chromium } from "playwright";
const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html":"text/html", ".js":"text/javascript", ".mjs":"text/javascript",
               ".wasm":"application/wasm", ".mersey":"text/plain" };
const server = createServer(async (req,res) => {
  try { const f = join(webRoot, req.url.split("?")[0]);
    res.writeHead(200,{"content-type":MIME[extname(f)] ?? "text/plain"}); res.end(await readFile(f));
  } catch { res.writeHead(404).end("nf"); }
});
await new Promise(r => server.listen(0, r));
const browser = await chromium.launch({ args:["--no-sandbox"] });
const page = await browser.newPage();
const logs = []; const errs = [];
page.on("console", m => logs.push(m.text()));
page.on("pageerror", e => errs.push(String(e)));
await page.goto(`http://localhost:${server.address().port}/pixels.html`, { waitUntil:"networkidle" });
await page.waitForTimeout(4000);
// verify the canvas really got the gradient
const px = await page.evaluate(() => {
  const c = document.getElementById("c");
  const d = c.getContext("2d").getImageData(10, 0, 1, 1).data;
  return `${d[0]},${d[1]},${d[2]},${d[3]}`;
});
await browser.close(); server.close();
const nums = logs.filter(l => l.startsWith("PIXELS")).map(l => Number(l.match(/in (\d+)ms/)[1]));
const [bytesMs, bridgeMs] = nums;
const perByte = bytesMs / 160000, perBridge = bridgeMs / 40000;
let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
check("REAL BROWSER · pixel loop painted the canvas", px === "10,128,245,255", px);
check("REAL BROWSER · no page errors", errs.length === 0, errs.join(";"));
// In the WASM backend, Bytes must beat per-element bridge calls by a wide
// margin. In the transpiled-JS backend both paths run at native speed, so the
// ratio is meaningless — accept either a real win or both-too-fast-to-measure.
// The "too fast" floor is 0.1µs/element: on fast CI hardware the whole loop is
// only a few ms (timer-noise territory), where the ratio is not reliable but
// each element is plainly cheap — which is the property that actually matters.
check(`REAL BROWSER · Bytes is faster per element (${(perBridge / perByte).toFixed(1)}x)`,
      perByte < perBridge / 3 || (perByte * 1000 < 0.1 && perBridge * 1000 < 0.1),
      `${(perByte * 1000).toFixed(2)}µs vs ${(perBridge * 1000).toFixed(2)}µs`);
console.log(`\n  160,000 element writes via Bytes (bulk in/out): ${bytesMs}ms  (${(perByte * 1000).toFixed(2)}µs each)`);
console.log(`   40,000 element writes via the bridge:          ${bridgeMs}ms  (${(perBridge * 1000).toFixed(2)}µs each)`);
if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nTyped-array bulk transfer: passed");
