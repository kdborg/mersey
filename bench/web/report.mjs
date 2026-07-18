// Merge the stock (js/poly × chromium/firefox) and native (firefox-fork)
// results into one comparison, printed as a table and written as Markdown.
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const load = async (f) => {
  try { return JSON.parse(await readFile(join(here, f), "utf8")); }
  catch { return []; }
};

const rows = [...(await load("results.stock.json")), ...(await load("results.tjs.json")), ...(await load("results.native.json")), ...(await load("results.servo.json")), ...(await load("results.native.servo.json")), ...(await load("results.ladybird.json")), ...(await load("results.native.ladybird.json"))];
const workloads = [...new Set(rows.map((r) => r.wl))].sort();
const key = (browser, impl) => `${browser}/${impl}`;
const get = (wl, browser, impl) => rows.find((r) => r.wl === wl && r.browser === browser && r.impl === impl);

// Columns, in reading order.
const cols = [
  ["chromium", "js", "Chromium JS"],
  ["chromium", "poly", "Chromium WASM poly"],
  ["chromium", "tjs", "Chromium JS-backend"],
  ["firefox", "js", "Firefox JS"],
  ["firefox", "poly", "Firefox WASM poly"],
  ["firefox", "tjs", "Firefox JS-backend"],
  ["servo", "js", "Servo JS"],
  ["servo", "poly", "Servo WASM poly"],
  ["servo", "tjs", "Servo JS-backend"],
  ["ladybird", "js", "Ladybird JS"],
  ["ladybird", "poly", "Ladybird WASM poly"],
  ["ladybird", "tjs", "Ladybird JS-backend"],
  ["firefox-fork", "native", "Firefox fork native"],
  ["servo-fork", "native", "Servo fork native"],
  ["ladybird-fork", "native", "Ladybird fork native"],
];

const fmtMs = (r) => (r && r.ms != null ? `${r.ms.toFixed(1)}` : "—");
const fmtRss = (r) => (r && r.rss != null ? `${(r.rss / 1024).toFixed(1)}` : "—");

let md = "# Mersey web-platform benchmarks\n\n";
md += "Wall-clock of the workload loop (self-timed in-language, startup excluded), ";
md += "median of 3 runs. Lower is faster.\n\n";
md += "- **js** — the workload in plain JavaScript (the browser's own engine)\n";
md += "- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser\n";
md += "- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge\n\n";

md += "## Time (ms)\n\n";
md += "| workload | " + cols.map((c) => c[2]).join(" | ") + " |\n";
md += "|" + "---|".repeat(cols.length + 1) + "\n";
for (const wl of workloads) {
  md += `| ${wl} | ` + cols.map(([b, i]) => fmtMs(get(wl, b, i))).join(" | ") + " |\n";
}

md += "\n## Memory — PSS delta vs blank page (MiB)\n\n";
md += "Proportional set size of the whole browser process tree, workload page minus a blank page ";
md += "(PSS counts shared libraries once, so a new renderer process does not inflate the delta). ";
md += "The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native ";
md += "engine is compiled into the browser binary, so its delta is workload allocation only.\n\n";
md += "| workload | " + cols.map((c) => c[2]).join(" | ") + " |\n";
md += "|" + "---|".repeat(cols.length + 1) + "\n";
for (const wl of workloads) {
  md += `| ${wl} | ` + cols.map(([b, i]) => fmtRss(get(wl, b, i))).join(" | ") + " |\n";
}

// Polyfill and native slowdown vs JS (Chromium JS as the baseline).
md += "\n## Slowdown vs plain JS (Chromium JS = 1×)\n\n";
md += "| workload | Chromium polyfill | Firefox polyfill | Servo polyfill | Ladybird polyfill | Firefox fork native | Servo fork native | Ladybird fork native |\n|---|---|---|---|---|---|---|---|\n";
for (const wl of workloads) {
  const base = get(wl, "chromium", "js");
  const ratio = (r) => (base && base.ms && r && r.ms != null ? `${(r.ms / base.ms).toFixed(1)}×` : "—");
  md += `| ${wl} | ${ratio(get(wl, "chromium", "poly"))} | ${ratio(get(wl, "firefox", "poly"))} | ${ratio(get(wl, "servo", "poly"))} | ${ratio(get(wl, "ladybird", "poly"))} | ${ratio(get(wl, "firefox-fork", "native"))} | ${ratio(get(wl, "servo-fork", "native"))} | ${ratio(get(wl, "ladybird-fork", "native"))} |\n`;
}

await writeFile(join(here, "REPORT.md"), md);
console.log(md);
console.log(`\nwrote bench/web/REPORT.md`);
