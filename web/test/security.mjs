// CSP + SRI enforcement in the polyfill (spec §5.4).
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";
import { chromium } from "playwright";
const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html":"text/html", ".js":"text/javascript", ".mjs":"text/javascript",
               ".wasm":"application/wasm", ".mersey":"text/plain" };
const server = createServer(async (req, res) => {
  try { const f = join(webRoot, req.url.split("?")[0]);
    res.writeHead(200, { "content-type": MIME[extname(f)] ?? "text/plain" });
    res.end(await readFile(f));
  } catch { res.writeHead(404).end("nf"); }
});
await new Promise(r => server.listen(0, r));
const browser = await chromium.launch({ args: ["--no-sandbox"] });
const page = await browser.newPage();
const logs = [], errs = [];
page.on("console", m => logs.push(`${m.type()}: ${m.text()}`));
page.on("pageerror", e => errs.push(String(e)));
await page.goto(`http://localhost:${server.address().port}/security.html`, { waitUntil: "networkidle" });
await page.waitForTimeout(600);
const out = await page.$eval("#out", e => e.textContent);
await browser.close(); server.close();

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
check("SRI: correct hash → module runs", out === "Clicks: 0", out);
check("SRI: wrong hash → module refused",
      logs.some(l => /integrity check failed/.test(l)), logs.join(" | "));
check("SRI: refused module did not execute",
      !logs.some(l => /modules work in the browser/.test(l)), logs.join(" | "));
if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nCSP + SRI enforcement: passed");
