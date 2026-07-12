// Custom elements written as ordinary Mersey classes (subclass + override),
// upgraded and driven by the real browser.
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
page.on("console", m => logs.push(m.text()));
page.on("pageerror", e => errs.push(String(e)));
await page.goto(`http://localhost:${server.address().port}/components.html`,
                { waitUntil: "networkidle" });
await page.waitForTimeout(700);

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};
const text = (sel) => page.$eval(sel, (e) => e.textContent);

check("components: no page errors", errs.length === 0, errs.join("; "));
check("REAL BROWSER · declared elements upgraded by the Mersey class",
      (await text("#a")) === "clicks: 0", await text("#a"));
check("REAL BROWSER · attribute reached the subclass (label=votes)",
      (await text("#b")) === "votes: 0", await text("#b"));

// Per-element state: clicking #a must not touch #b.
await page.click("#a");
await page.click("#a");
await page.click("#a");
await page.click("#b");
check("REAL BROWSER · per-element instance state (3 clicks on #a)",
      (await text("#a")) === "clicks: 3", await text("#a"));
check("REAL BROWSER · sibling instance is independent (1 click on #b)",
      (await text("#b")) === "votes: 1", await text("#b"));

check("REAL BROWSER · second component class works too",
      (await text("mersey-hello")) === "hello from a Mersey component 🌊",
      await text("mersey-hello"));
check("REAL BROWSER · element created from Mersey is upgraded too",
      (await text("#host mersey-counter")) === "taps: 0",
      await text("#host mersey-counter"));

// Removal fires disconnected() on the right instance.
await page.evaluate(() => document.getElementById("a").remove());
await page.waitForTimeout(200);
// Host-backed: the instance IS the element.
check("REAL BROWSER · `this` is the element (tagName read off the host)",
      logs.some((l) => /^\[counter\] connected \(label=clicks, tag=MERSEY-COUNTER\)$/.test(l)),
      logs.join(" | "));
const attrSet = await page.$eval("mersey-hello", (e) => e.getAttribute("data-mersey"));
check("REAL BROWSER · host method called on `this` (setAttribute)", attrSet === "yes", attrSet);
const isElement = await page.$eval("#host mersey-counter",
                                   (e) => e instanceof HTMLElement && e.isConnected);
check("REAL BROWSER · instance passed as an Element really is in the DOM", isElement === true);

check("REAL BROWSER · disconnected() ran with that instance's state",
      logs.some((l) => /^\[counter\] disconnected after 3 clicks$/.test(l)),
      logs.join(" | "));

await browser.close(); server.close();
if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nCustom elements as Mersey classes: passed");
