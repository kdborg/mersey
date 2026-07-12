// A runtime error in the browser must point at the Mersey line that failed.
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
const errors = [];
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
await page.goto(`http://localhost:${server.address().port}/error.html`, { waitUntil: "networkidle" });
await page.waitForTimeout(600);
await browser.close(); server.close();

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
const text = errors.join("\n");
check("REAL BROWSER · runtime error reported", /RangeError: index 99 out of bounds/.test(text), text);
check("REAL BROWSER · stack trace names the Mersey functions",
      /at pick \(demo\/error\.mersey:5:12\)/.test(text), text);
check("REAL BROWSER · code frame shows the offending source line",
      /return xs\[i\];/.test(text) && /\^/.test(text), text);
if (failures) { console.error(`\n${failures} failed`); console.error(text); process.exit(1); }
console.log("\nBrowser error reporting: passed");
