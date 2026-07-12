// Dynamic import, top-level await and `for await` in a real browser: the loader
// fetches the whole graph (including the dynamic-import target), and the engine
// evaluates the lazy module only when it is imported.
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
await page.goto(`http://localhost:${server.address().port}/lazy.html`,
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
check("lazy: no page errors", errors.length === 0, errors.join("; "));

// Top-level await + dynamic import + for await, all on the WASM engine.
check("REAL BROWSER · dynamic import() resolved through the loader",
      text === "lazy: triple(7)=21, for-await total=60", text);

// The whole point of a dynamic import: the module is fetched and checked with
// the graph, but does not *evaluate* until it is imported.
const starting = logs.findIndex((l) => /lazy demo: starting/.test(l));
const evaluated = logs.findIndex((l) => /^lazy: evaluated$/.test(l));
check("REAL BROWSER · the imported module is fetched but not run at startup",
      starting >= 0 && evaluated > starting,
      logs.join(" | "));
check("REAL BROWSER · top-level await and for await ran",
      logs.some((l) => /^lazy demo: done total=60$/.test(l)),
      logs.join(" | "));

if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nDynamic import + async generators in a real browser: passed");
