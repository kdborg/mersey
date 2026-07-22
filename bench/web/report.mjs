// Merge the stock (js/poly × chromium/firefox) and native (firefox-fork)
// results into one comparison, printed as a table and written as Markdown.
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { forPlatform, platformsIn, rowMemMetric } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const load = async (f) => {
  try { return JSON.parse(await readFile(join(here, f), "utf8")); }
  catch { return []; }
};

const allRows = [...(await load("results.stock.json")), ...(await load("results.tjs.json")), ...(await load("results.firefox-real.json")), ...(await load("results.native.json")), ...(await load("results.servo.json")), ...(await load("results.native.servo.json")), ...(await load("results.ladybird.json")), ...(await load("results.native.ladybird.json")), ...(await load("results.native.chromium.json")), ...(await load("results.engine.json"))];

// One section per host platform. Numbers are NEVER merged across platforms:
// the machines differ, and the memory metric differs outright (Linux PSS vs
// macOS footprint), so a single blended table would be meaningless.
const platforms = platformsIn(allRows);

// Columns, in reading order.
const cols = [
  ["chromium", "js", "Chromium JS"],
  ["chromium", "poly", "Chromium WASM poly"],
  ["chromium", "tjs", "Chromium JS-backend"],
  ["firefox", "js", "Firefox JS"],
  ["firefox", "poly", "Firefox WASM poly"],
  ["firefox", "tjs", "Firefox JS-backend"],
  ["firefox-real", "js", "Firefox real JS"],
  ["firefox-real", "poly", "Firefox real WASM poly"],
  ["firefox-real", "tjs", "Firefox real JS-backend"],
  ["servo", "js", "Servo JS"],
  ["servo", "poly", "Servo WASM poly"],
  ["servo", "tjs", "Servo JS-backend"],
  ["ladybird", "js", "Ladybird JS"],
  ["ladybird", "poly", "Ladybird WASM poly"],
  ["ladybird", "tjs", "Ladybird JS-backend"],
  ["firefox-fork", "native", "Firefox fork native"],
  ["servo-fork", "native", "Servo fork native"],
  ["ladybird-fork", "native", "Ladybird fork native"],
  ["chromium-fork", "native", "Chromium fork native"],
  ["engine", "wasm", "Engine wasm (Node)"],
];

const fmtMs = (r) => (r && r.ms != null ? `${r.ms.toFixed(1)}` : "—");
const fmtRss = (r) => (r && r.rss != null ? `${(r.rss / 1024).toFixed(1)}` : "—");

let md = "# Mersey web-platform benchmarks\n\n";
md += "Wall-clock of the workload loop (self-timed in-language, startup excluded), ";
md += "median of 3 runs. Lower is faster.\n\n";
md += "- **js** — the workload in plain JavaScript (the browser's own engine)\n";
md += "- **polyfill** — the same workload in Mersey, engine compiled to WASM, in a stock browser\n";
md += "- **native** — the same Mersey, engine hosted inside the browser fork, web APIs via the C++ bridge\n";
md += "- **engine wasm** — no browser: the wasm engine over a deterministic stub realm in Node ";
md += "(`run-engine.mjs`), same checksums as every browser leg. Its memory column is the child ";
md += "process's peak RSS minus a blank engine child. This is the leg the perf regression tests ";
md += "gate on (`perf-test.mjs` vs `perf-baselines.json`).\n\n";
md += "> **Caveat — Playwright Firefox understates wasm.** The \"Firefox\" columns are measured ";
md += "through Playwright, which drives Firefox with the JS debugger attached; SpiderMonkey runs ";
md += "ALL WebAssembly baseline-only while debugging (microsoft/playwright#11102), so the Firefox ";
md += "WASM-poly and JS-backend (wasm compute tier) columns are 5-7× slower than real Firefox. ";
md += "The \"Firefox real\" columns are the honest numbers: the system Firefox, headless, no ";
md += "driver attached (`run-firefox-real.mjs`). Its memory deltas use a fresh browser per ";
md += "sample (blank page → self-navigation to the workload in one process tree), so they run ";
md += "slightly higher than the Playwright columns, which reuse a warm browser.\n\n";

if (platforms.length > 1) {
  md += "> **Two host platforms below.** Each section is a separate machine and a ";
  md += "separate memory metric; compare rows WITHIN a section, never across.\n\n";
}

const MEM_LABEL = {
  pss: ["PSS", "Proportional set size of the whole browser process tree, workload page minus a blank page (PSS counts shared libraries once, so a new renderer process does not inflate the delta)."],
  footprint: ["footprint", "De-duplicated phys_footprint of the whole browser process tree (macOS `footprint`), workload page minus a blank page — shared objects counted once. This is NOT the same metric as Linux PSS and the two are not comparable."],
};

for (const platform of platforms) {
  const rows = forPlatform(allRows, platform);
  const workloads = [...new Set(rows.map((r) => r.wl))].sort();
  const get = (wl, browser, impl) => rows.find((r) => r.wl === wl && r.browser === browser && r.impl === impl);
  const metric = rowMemMetric(rows.find((r) => r.rss != null) ?? {}) ?? "pss";
  const [metricName, metricNote] = MEM_LABEL[metric] ?? [metric, ""];

  if (platforms.length > 1) md += `\n# ${platform}\n\n`;

  md += "## Time (ms)\n\n";
  md += "| workload | " + cols.map((c) => c[2]).join(" | ") + " |\n";
  md += "|" + "---|".repeat(cols.length + 1) + "\n";
  for (const wl of workloads) {
    md += `| ${wl} | ` + cols.map(([b, i]) => fmtMs(get(wl, b, i))).join(" | ") + " |\n";
  }

  md += `\n## Memory — ${metricName} delta vs blank page (MiB)\n\n`;
  md += metricNote + " ";
  md += "The polyfill delta includes the ~2.3 MB WASM module and the engine's heap; the native ";
  md += "engine is compiled into the browser binary, so its delta is workload allocation only.\n\n";
  md += "| workload | " + cols.map((c) => c[2]).join(" | ") + " |\n";
  md += "|" + "---|".repeat(cols.length + 1) + "\n";
  for (const wl of workloads) {
    md += `| ${wl} | ` + cols.map(([b, i]) => fmtRss(get(wl, b, i))).join(" | ") + " |\n";
  }

  // Polyfill and native slowdown vs JS (Chromium JS as the baseline).
  md += "\n## Slowdown vs plain JS (Chromium JS = 1×)\n\n";
  md += "| workload | Chromium polyfill | Firefox polyfill | Firefox real polyfill | Servo polyfill | Ladybird polyfill | Firefox fork native | Servo fork native | Ladybird fork native | Engine wasm (Node) |\n|---|---|---|---|---|---|---|---|---|---|\n";
  for (const wl of workloads) {
    const base = get(wl, "chromium", "js");
    const ratio = (r) => (base && base.ms && r && r.ms != null ? `${(r.ms / base.ms).toFixed(1)}×` : "—");
    md += `| ${wl} | ${ratio(get(wl, "chromium", "poly"))} | ${ratio(get(wl, "firefox", "poly"))} | ${ratio(get(wl, "firefox-real", "poly"))} | ${ratio(get(wl, "servo", "poly"))} | ${ratio(get(wl, "ladybird", "poly"))} | ${ratio(get(wl, "firefox-fork", "native"))} | ${ratio(get(wl, "servo-fork", "native"))} | ${ratio(get(wl, "ladybird-fork", "native"))} | ${ratio(get(wl, "engine", "wasm"))} |\n`;
  }
}

await writeFile(join(here, "REPORT.md"), md);
console.log(md);
console.log(`\nwrote bench/web/REPORT.md`);
