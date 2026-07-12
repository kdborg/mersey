// Randomness is a capability: the page grants it per script, and the *same*
// program is refused when it does not.
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
await page.goto(`http://localhost:${server.address().port}/random.html`,
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

check("REAL BROWSER · granted: random draws from the platform CSPRNG",
      text === "random: granted and in range", text);
check("REAL BROWSER · granted: no page errors", errors.length === 0, errors.join("; "));

// The same program, on a page that did not grant the capability.
{
  const server2 = createServer(async (req, res) => {
    try {
      const f = join(webRoot, req.url.split("?")[0]);
      res.writeHead(200, { "content-type": MIME[extname(f)] ?? "text/plain" });
      res.end(await readFile(f));
    } catch { res.writeHead(404).end("nf"); }
  });
  await new Promise((r) => server2.listen(0, r));
  const b2 = await chromium.launch({ args: ["--no-sandbox"] });
  const p2 = await b2.newPage();
  const logs2 = [];
  p2.on("console", (m) => logs2.push(m.text()));
  await p2.goto(`http://localhost:${server2.address().port}/random-denied.html`,
                { waitUntil: "networkidle" });
  await p2.waitForTimeout(600);
  const text2 = await p2.$eval("#out", (e) => e.textContent);
  await b2.close();
  server2.close();

  check("REAL BROWSER · denied by default: the program does not run",
        text2 !== "random: granted and in range", text2);
  check("REAL BROWSER · denied by default: it says why",
        logs2.some((l) => /random. capability/.test(l)), logs2.join(" | "));
}

if (failures) { console.error(`\n${failures} failed`); process.exit(1); }
console.log("\nCapability-gated randomness in a real browser: passed");
