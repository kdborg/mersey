// REAL BROWSER test: launches Chrome, serves web/ over HTTP, loads the
// actual pages, and drives them with real user input. Nothing is stubbed —
// this is Mersey compiled to WASM, executing in a browser, manipulating a
// real DOM and calling real web APIs (storage, crypto, URL, canvas, timers,
// fetch, history, media queries…) through the universal bridge.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";
import { chromium } from "playwright";

const webRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".wasm": "application/wasm",
  ".mersey": "text/plain",
};

const server = createServer(async (req, res) => {
  try {
    const path = decodeURIComponent(req.url.split("?")[0]);
    const file = join(webRoot, path === "/" ? "/index.html" : path);
    const body = await readFile(file);
    res.writeHead(200, {
      "content-type": MIME[extname(file)] ?? "application/octet-stream",
    });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});
await new Promise((r) => server.listen(0, r));
const base = `http://localhost:${server.address().port}`;

const browser = await chromium.launch({ args: ["--no-sandbox"] });

let failures = 0;
const check = (what, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}${ok ? "" : `  (${detail})`}`);
  if (!ok) failures++;
};

async function open(path) {
  const page = await browser.newPage();
  const logs = [];
  const errors = [];
  page.on("console", (m) => logs.push(m.text()));
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(`${base}${path}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(500); // engine boot + async work
  return { page, logs, errors };
}

// ---- 1. counter demo, real clicks ------------------------------------------
{
  const { page, logs, errors } = await open("/index.html");
  check("counter: no page errors", errors.length === 0, errors.join("; "));
  check("counter: engine ran", logs.some((l) => l.includes("Mersey is running")),
        logs.join(" | "));
  const initial = await page.$eval("#out", (e) => e.textContent);
  check("counter: initial render", initial === "Clicks: 0", initial);
  for (let i = 0; i < 5; i++) await page.click("#btn");
  const after = await page.$eval("#out", (e) => e.textContent);
  check("counter: 5 real browser clicks", after === "Clicks: 5 🌊", after);
  await page.close();
}

// ---- 2. TODO demo, real typing + clicking ------------------------------------
{
  const { page, errors } = await open("/todo.html");
  check("todo: no page errors", errors.length === 0, errors.join("; "));
  const empty = await page.$eval("#count", (e) => e.textContent);
  check("todo: empty state", empty === "nothing to do 🌊", empty);
  await page.fill("#new-todo", "learn mersey");
  await page.click("#add");
  await page.fill("#new-todo", "ship it");
  await page.click("#add");
  const two = await page.$eval("#count", (e) => e.textContent);
  check("todo: two items after typing", two === "2 items to do", two);
  const itemCount = await page.locator("#list li").count();
  check("todo: real <li> elements in the DOM", itemCount === 2, `${itemCount}`);
  const firstText = await page.locator("#list li").first().textContent();
  check("todo: item text", firstText.startsWith("learn mersey"), firstText);
  await page.click("#list li"); // click the item to remove it
  const left = await page.$eval("#count", (e) => e.textContent);
  check("todo: removal via click", left === "1 item to do", left);
  const remaining = await page.locator("#list li").count();
  check("todo: element removed from DOM", remaining === 1, `${remaining}`);
  await page.close();
}

// ---- 3. the web platform, in a real browser ------------------------------------
{
  const { page, logs, errors } = await open("/platform.html");
  check("platform: no page errors", errors.length === 0, errors.join("; "));
  const has = (re) => logs.some((l) => re.test(l));

  check("REAL BROWSER · Web Storage", has(/^storage: visits=1$/), logs.join(" | "));
  check("REAL BROWSER · Web Crypto", has(/^crypto: 4 random bytes drawn$/), logs.join(" | "));
  check("REAL BROWSER · URL API",
        has(/^url: host=example\.com path=\/a\/b query=\?q=mersey$/), logs.join(" | "));
  check("REAL BROWSER · JSON", has(/^json: \{.*"lang":"mersey".*\}$/), logs.join(" | "));
  check("REAL BROWSER · Canvas 2D", has(/^canvas: filled a 120x40 rect$/), logs.join(" | "));
  check("REAL BROWSER · Timers", has(/^timer: fired after 50ms$/), logs.join(" | "));
  check("REAL BROWSER · fetch + Promise", has(/^fetch: status 200$/), logs.join(" | "));

  // Verify the effects landed in the real DOM / real canvas.
  const storedInBrowser = await page.evaluate(() => localStorage.getItem("mersey.visits"));
  check("REAL BROWSER · localStorage really written", storedInBrowser === "1", `${storedInBrowser}`);
  const canvasPainted = await page.evaluate(() => {
    const c = document.querySelector("canvas");
    if (!c) return "no canvas";
    const px = c.getContext("2d").getImageData(5, 5, 1, 1).data;
    return `${px[0]},${px[1]},${px[2]},${px[3]}`;
  });
  check("REAL BROWSER · canvas pixels actually painted (#0af)",
        canvasPainted === "0,170,255,255", canvasPainted);
  const rendered = await page.$eval("#out", (e) => e.textContent.split("\n").length);
  check("REAL BROWSER · DOM shows all lines", rendered >= 7, `${rendered} lines`);

  // Newly covered: interface constants, indexed collections, CSS/CSSOM.
  check("REAL BROWSER · interface constants (Node.ELEMENT_NODE)",
        has(/^constants: ELEMENT_NODE=1 TEXT_NODE=3$/), logs.join(" | "));
  check("REAL BROWSER · indexed collections (querySelectorAll[0])",
        has(/^nodelist: \d+ <p>, first tag=P$/), logs.join(" | "));
  check("REAL BROWSER · CSS / CSSOM (style.color, className)",
        has(/^style: color=rebeccapurple class=generated$/), logs.join(" | "));
  const computed = await page.evaluate(() => {
    const el = document.querySelector("div.generated");
    return el ? getComputedStyle(el).color : "missing";
  });
  check("REAL BROWSER · CSS really applied (computed color)",
        computed === "rgb(102, 51, 153)", computed);
  await page.close();
}

// ---- 4. async / await against real browser promises ----------------------------
{
  const { page, logs, errors } = await open("/async.html");
  check("async: no page errors", errors.length === 0, errors.join("; "));
  const has = (re) => logs.some((l) => re.test(l));
  check("REAL BROWSER · sync code runs before the coroutine suspends",
        logs[0] === "sync code ran first (async is not blocking)", logs[0]);
  check("REAL BROWSER · await fetch(…) + await resp.text()",
        has(/^mersey-loader\.js → 200, \d+ bytes$/), logs.join(" | "));
  check("REAL BROWSER · Promise.all over concurrent awaits",
        has(/^parallel: 2 loads finished$/), logs.join(" | "));
  check("REAL BROWSER · await a promise wrapping setTimeout",
        has(/^await delay\(40\): resumed after a real browser timer$/), logs.join(" | "));
  check("REAL BROWSER · rejection crosses await into Mersey try/catch",
        has(/^await rethrew across the coroutine: does-not-exist\.txt → 404$/),
        logs.join(" | "));
  check("REAL BROWSER · async flow completed", has(/^async\/await: done$/), logs.join(" | "));
  await page.close();
}

await browser.close();
server.close();

if (failures) {
  console.error(`\n${failures} assertion(s) failed`);
  process.exit(1);
}
console.log("\nMersey in a REAL browser (Chromium): all assertions passed");
