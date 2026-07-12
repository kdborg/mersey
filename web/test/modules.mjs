// Multi-module program in a real browser: the loader fetches the import
// graph, links it, and runs it.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";
import { chromium } from "playwright";

const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript",
               ".wasm": "application/wasm", ".mersey": "text/plain" };
const server = createServer(async (req, res) => {
  try {
    const f = join(webRoot, req.url.split("?")[0]);
    res.writeHead(200, { "content-type": MIME[extname(f)] ?? "text/plain" });
    res.end(await readFile(f));
  } catch { res.writeHead(404).end("nf"); }
});
await new Promise((r) => server.listen(0, r));
const browser = await chromium.launch({ args: ["--no-sandbox"] });
const page = await browser.newPage();
const logs = [];
const errors = [];
page.on("console", (m) => logs.push(m.text()));
page.on("pageerror", (e) => errors.push(String(e)));
await page.goto(`http://localhost:${server.address().port}/modular.html`,
                { waitUntil: "networkidle" });
await page.waitForTimeout(600);
const text = await page.$eval("#out", (e) => e.textContent);
await browser.close();
server.close();

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
check("modules: no page errors", errors.length === 0, errors.join("; "));
check("REAL BROWSER · imported class from a sibling .mersey module",
      text === "modules work in the browser: counter = 3", text);
check("REAL BROWSER · module graph fetched and linked",
      logs.some((l) => /^modular: modules work in the browser, count=3$/.test(l)),
      logs.join(" | "));
if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nMulti-module Mersey in a real browser: passed");
