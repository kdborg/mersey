// REAL BROWSER: the console REPL (`globalThis.mersey`) on a polyfill page —
// one growing typechecked module against the page's live engine.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm", ".mersey": "text/plain" };
const server = createServer(async (req, res) => {
  try {
    const path = join(root, decodeURIComponent(new URL(req.url, "http://x").pathname));
    res.writeHead(200, { "content-type": MIME[extname(path)] ?? "application/octet-stream" });
    res.end(await readFile(path));
  } catch { res.writeHead(404).end(); }
});
await new Promise((r) => server.listen(0, r));
const origin = `http://localhost:${server.address().port}`;

const pw = await import(join(root, "node_modules", "playwright", "index.js"));
const { chromium } = pw.default ?? pw;
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
await page.goto(`${origin}/todo.html`);
await page.waitForFunction(() => typeof globalThis.mersey === "function", { timeout: 15000 });

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  REAL BROWSER · ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};

const r1 = await page.evaluate(() => mersey("let answer = 21"));
check("statement echoes nothing", r1 === undefined, String(r1));
const r2 = await page.evaluate(() => mersey("answer * 2"));
check("expression echoes across turns", r2 === "42", String(r2));
const r3 = await page.evaluate(() => mersey`answer + ${100}`);
check("tagged template with interpolation", r3 === "121", String(r3));
const rejected = await page.evaluate(() => {
  try { mersey("let bad: int32 = \"no\""); return null; } catch (e) { return String(e.message); }
});
check("ill-typed turn throws diagnostics", rejected !== null && rejected.includes("E0401"), String(rejected));
const r4 = await page.evaluate(() => mersey("answer"));
check("rejected turn left the session intact", r4 === "21", String(r4));
const names = await page.evaluate(() => mersey.completions());
check(
    "completions are the session's own visible names",
    Array.isArray(names) && names.includes("answer") && names.includes("console") &&
        !names.includes("window") && !names.includes("document"),
    JSON.stringify(names));

await browser.close();
server.close();
if (failures) process.exit(1);
console.log("\nBrowser REPL: passed");
