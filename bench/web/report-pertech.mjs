// Generate report-pertech.html: a standalone page with ONLY the
// per-technology panels — time (log) and memory (PSS delta) side by side,
// the four ways of running the same program (plain JS, transpiled JS, WASM
// interpreter, native fork) each across the four browsers (·ff ·cr ·sv ·lb).
// Everything is computed here from the results JSONs and baked as static
// HTML; regenerate after refreshing results:
//
//   node bench/web/report-pertech.mjs   # -> bench/web/report-pertech.html
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { forPlatform, platformsIn } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const load = async (f) => {
  try { return JSON.parse(await readFile(join(here, f), "utf8")); }
  catch { return []; }
};

const allRows = [
  ...(await load("results.stock.json")),
  ...(await load("results.tjs.json")),
  ...(await load("results.servo.json")),
  ...(await load("results.ladybird.json")),
  ...(await load("results.native.json")),
  ...(await load("results.native.servo.json")),
  ...(await load("results.native.ladybird.json")),
  ...(await load("results.native.chromium.json")),
];

// This page carries ONE data set, so it renders a single platform — mixing them
// would collide on the same keys. Linux by default; BENCH_PLATFORM=macos to
// build the macOS page instead.
const platform = process.env.BENCH_PLATFORM || "linux";
const present = platformsIn(allRows);
if (!present.includes(platform)) {
  console.error(`no rows for platform "${platform}" — results contain: ${present.join(", ") || "(none)"}`);
  process.exit(1);
}
const rows = forPlatform(allRows, platform);

const KEY = {
  "chromium/js": "cjs", "chromium/poly": "cpoly", "chromium/tjs": "ctjs",
  "firefox/js": "fjs", "firefox/poly": "fpoly", "firefox/tjs": "ftjs",
  "servo/js": "sjs", "servo/poly": "spoly", "servo/tjs": "stjs",
  "ladybird/js": "lbjs", "ladybird/poly": "lbpoly", "ladybird/tjs": "lbtjs",
  "firefox-fork/native": "native", "chromium-fork/native": "cnative",
  "servo-fork/native": "snative", "ladybird-fork/native": "lbnative",
};

// Engine-only workloads (no js twin / no web API) stay out of this report.
const EXCLUDE = new Set(["calls", "compute", "fcompute", "mathk"]);

const DATA = {};
for (const r of rows) {
  if (r.ms == null || EXCLUDE.has(r.wl)) continue;
  const k = KEY[`${r.browser}/${r.impl}`];
  if (!k) continue;
  const d = (DATA[r.wl] ??= {});
  d[k] = Number(r.ms.toFixed(1));
  if (r.rss != null) d["m" + k] = Number((r.rss / 1024).toFixed(1));
}
const WL = Object.keys(DATA).sort();

const IMPLS = [
  ["js",     { ff: "fjs",   cr: "cjs",     sv: "sjs",     lb: "lbjs" }],
  ["tjs",    { ff: "ftjs",  cr: "ctjs",    sv: "stjs",    lb: "lbtjs" }],
  ["poly",   { ff: "fpoly", cr: "cpoly",   sv: "spoly",   lb: "lbpoly" }],
  ["native", { ff: "native", cr: "cnative", sv: "snative", lb: "lbnative" }],
];
const BROWSERS = ["ff", "cr", "sv", "lb"];

const LOG_MIN = 1, LOG_MAX = 20000; // headroom for Ladybird tjs storage (~15 s)
const logPct = (v) => Math.max(2, (Math.log10(Math.max(v, LOG_MIN)) - Math.log10(LOG_MIN)) / (Math.log10(LOG_MAX) - Math.log10(LOG_MIN)) * 100);
const fmtMs = (v) => v >= 1000 ? (v / 1000).toFixed(1) + " s" : v;

const naRow = (cls, lab) => `<div class="bar-row">
      <div class="bar-lab">${lab}</div>
      <div class="track"><div class="bar ${cls}" style="width:2%;opacity:.28">
        <span class="val out" style="color:var(--ink-faint)">n/a</span></div></div></div>`;

const column = (d, title, get, pctFn, fmtV) => `
    <div class="pt-col">
      <div class="pt-title">${title}</div>
      <div class="bars">` + IMPLS.map(([cls, keys]) => BROWSERS.map((b) => {
        const lab = `${cls}·${b}`;
        const v = get(keys[b]);
        if (v == null) return naRow(cls, lab);
        const pct = pctFn(v);
        return `<div class="bar-row">
        <div class="bar-lab">${lab}</div>
        <div class="track"><div class="bar ${cls}" style="width:${pct.toFixed(1)}%">
          <span class="val${pct < 26 ? " out" : ""}">${fmtV(v)}</span></div></div></div>`;
      }).join("")).join("") + `</div>
    </div>`;

// Panels are collapsed <details> — the page opens as a scannable index; the
// TOC links (and a URL fragment) expand the one you came for.
const panels = WL.map((w) => {
  const d = DATA[w];
  const memVals = IMPLS.flatMap(([, keys]) => BROWSERS.map((b) => d["m" + keys[b]])).filter((v) => v != null);
  const memMax = Math.max(...memVals, 1);
  return `<details class="pt" id="wl-${w}">
    <summary class="wl-name"><span class="chev"></span><b>${w}</b><em>4 implementations × ff / cr / sv / lb</em></summary>
    <div class="pt-grid">
      ${column(d, "time — ms, log scale", (k) => d[k], logPct, fmtMs)}
      ${column(d, "memory — PSS delta, MiB", (k) => d["m" + k], (v) => Math.max(2, v / memMax * 100), (v) => (v === 0 ? "≈0" : v))}
    </div>
  </details>`;
}).join("\n");

const toc = `<nav class="toc">${WL.map((w) => `<a href="#wl-${w}">${w}</a>`).join("\n    ")}</nav>`;

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mersey — per-technology benchmarks</title>
<style>
  :root {
    --bg: #f7f9fa; --panel: #ffffff; --ink: #10191c; --ink-soft: #4a5a60;
    --ink-faint: #839499; --line: #dde5e8; --accent: #0a97c4;
    --js: #9aa7ad; --poly: #b06fd8; --tjs: #c8871c; --native: #0f9e88;
    --shadow: 0 1px 2px rgba(16,25,28,.04), 0 8px 24px -12px rgba(16,25,28,.12);
    --mono: ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code", Menlo, Consolas, monospace;
    --sans: system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0d1416; --panel: #131e21; --ink: #eaf2f4; --ink-soft: #9fb2b8;
      --ink-faint: #6b7f85; --line: #253437; --accent: #34c0e8;
      --js: #6b7a80; --poly: #c58ce8; --tjs: #e5ab45; --native: #2bc0a6;
      --shadow: 0 1px 2px rgba(0,0,0,.3), 0 12px 30px -14px rgba(0,0,0,.5);
    }
  }
  :root[data-theme="dark"] {
    --bg: #0d1416; --panel: #131e21; --ink: #eaf2f4; --ink-soft: #9fb2b8;
    --ink-faint: #6b7f85; --line: #253437; --accent: #34c0e8;
    --js: #6b7a80; --poly: #c58ce8; --tjs: #e5ab45; --native: #2bc0a6;
    --shadow: 0 1px 2px rgba(0,0,0,.3), 0 12px 30px -14px rgba(0,0,0,.5);
  }
  :root[data-theme="light"] {
    --bg: #f7f9fa; --panel: #ffffff; --ink: #10191c; --ink-soft: #4a5a60;
    --ink-faint: #839499; --line: #dde5e8; --accent: #0a97c4;
    --js: #9aa7ad; --poly: #b06fd8; --tjs: #c8871c; --native: #0f9e88;
    --shadow: 0 1px 2px rgba(16,25,28,.04), 0 8px 24px -12px rgba(16,25,28,.12);
  }
  body { margin: 0; background: var(--bg); color: var(--ink); font-family: var(--sans); line-height: 1.55; }
  .wrap { max-width: 1060px; margin: 0 auto; padding: 2.2rem 1.2rem 3rem; }
  h1 { font-size: 1.5rem; margin: 0 0 .3rem; }
  .note { color: var(--ink-soft); font-size: .92rem; max-width: 72ch; margin: .3rem 0 1rem; }
  .legend { display: flex; flex-wrap: wrap; gap: 1.1rem; margin: .8rem 0 1.2rem; font-size: .85rem; }
  .legend span { display: inline-flex; align-items: center; gap: .45rem; color: var(--ink-soft); }
  .sw { width: 11px; height: 11px; border-radius: 3px; display: inline-block; }
  .sw.js { background: var(--js); } .sw.poly { background: var(--poly); }
  .sw.tjs { background: var(--tjs); } .sw.native { background: var(--native); }
  .toc { display: flex; flex-wrap: wrap; gap: .35rem .5rem; margin: 0 0 1.1rem; }
  .toc a { font-family: var(--mono); font-size: .75rem; color: var(--accent); text-decoration: none;
           border: 1px solid var(--line); border-radius: 999px; padding: .18rem .65rem;
           background: var(--panel); }
  .toc a:hover { border-color: var(--accent); }
  .pt { background: var(--panel); border: 1px solid var(--line); border-radius: 14px;
        padding: .55rem 1.4rem .55rem; box-shadow: var(--shadow); margin: 0 0 .6rem;
        scroll-margin-top: 1rem; }
  .pt[open] { padding: 1.1rem 1.4rem .9rem; }
  .pt-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 0 1.8rem; }
  .pt-title { font-family: var(--mono); font-size: .72rem; color: var(--ink-faint); margin: .15rem 0 .4rem; }
  @media (max-width: 760px) { .pt-grid { grid-template-columns: 1fr; } }
  .wl-name { font-family: var(--mono); font-size: .8rem; color: var(--ink); display: flex;
             align-items: center; cursor: pointer; list-style: none; }
  .wl-name::-webkit-details-marker { display: none; }
  .pt[open] .wl-name { margin-bottom: .45rem; }
  .wl-name .chev { flex: none; margin-right: .55rem; border: solid var(--ink-faint);
                   border-width: 0 2px 2px 0; padding: 2.5px; transform: rotate(-45deg);
                   transition: transform .12s ease; }
  .pt[open] .wl-name .chev { transform: rotate(45deg); }
  .wl-name b { font-weight: 600; }
  .wl-name em { font-style: normal; color: var(--ink-faint); margin-left: auto; }
  .bars { display: grid; gap: 5px; }
  .bar-row { display: grid; grid-template-columns: 64px 1fr; align-items: center; gap: .6rem; }
  .bar-lab { font-family: var(--mono); font-size: .7rem; color: var(--ink-faint); text-align: right; }
  .track { position: relative; height: 20px; }
  .bar { height: 100%; border-radius: 4px; min-width: 2px; display: flex; align-items: center;
         justify-content: flex-end; position: relative; max-width: 100%; }
  .bar.js { background: var(--js); } .bar.poly { background: var(--poly); }
  .bar.tjs { background: var(--tjs); } .bar.native { background: var(--native); }
  .bar .val { font-family: var(--mono); font-size: .68rem; color: #fff; padding: 0 6px;
              white-space: nowrap; font-variant-numeric: tabular-nums; }
  .bar .val.out { color: var(--ink-soft); position: absolute; left: calc(100% + 6px); }
  footer { color: var(--ink-faint); font-size: .8rem; margin-top: 1.6rem; }
</style>
</head>
<body>
<div class="wrap">
  <h1>Mersey — per-technology benchmarks</h1>
  <p class="note">One panel per web technology: wall-clock of the workload loop (self-timed in-language,
    median of 3, <b>log scale</b>) and memory (PSS of the browser process tree, workload page minus a
    blank page). The same program four ways — plain JS (js), transpiled JS (tjs), the WASM interpreter (poly), and the
    Mersey engine native (native) in the browser fork — each in Firefox (·ff), Chromium (·cr), Servo (·sv) and
    Ladybird (·lb). Every bar reports the same checksum. A dimmed row is honest absence
    (the Chromium fork has no async result path, and its no-V8 glue can't reach the
    ScriptState-bound URLPattern — its sync workloads incl. geometry and blob run; Servo
    implements neither IndexedDB nor Web Locks — its fork leg runs
    everything else; the Ladybird fork runs everything except worker, which no Ladybird leg
    can run — its file:// pages would need a cross-origin worker script; the Ladybird legs
    reach the echo, sse and ws endpoints by absolute URL + CORS, since its harness loads pages
    from file://).
    A memory bar of ≈0 is the measurement floor, not zero cost: Ladybird runs one process per
    test, so its memory is peak PSS minus a blank page's peak — a workload whose allocations
    stay under startup's crest is invisible to that method.</p>
${toc}
  <div class="legend">
    <span><i class="sw js"></i> plain JS — the browser's own engine, JIT</span>
    <span><i class="sw tjs"></i> transpiled JS — Mersey→JS at load time, browser JIT runs it</span>
    <span><i class="sw poly"></i> WASM — the Mersey engine compiled to WASM, interpreting</span>
    <span><i class="sw native"></i> native — the Mersey engine hosted inside the browser fork</span>
  </div>
${panels}
  <footer>Generated by <code>bench/web/report-pertech.mjs</code> from <code>bench/web/results.*.json</code>.
    The full report with methodology and caveats is <code>report.html</code>.</footer>
</div>
<script>
  // Fragment navigation must expand the panel it points at — <details> stays
  // closed on anchor jumps in some browsers.
  const openHash = () => {
    const el = location.hash && document.querySelector(location.hash);
    if (el && el.tagName === "DETAILS") { el.open = true; el.scrollIntoView(); }
  };
  addEventListener("hashchange", openHash);
  openHash();
</script>
</body>
</html>
`;

await writeFile(join(here, "report-pertech.html"), html);
console.log(`wrote bench/web/report-pertech.html (${WL.length} panels)`);
