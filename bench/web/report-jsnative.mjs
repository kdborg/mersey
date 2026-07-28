// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// JS vs native, two arenas per panel: the SAME workload run as plain JavaScript
// and as native Mersey, both in the browser (each fork: the browser's own JS
// engine vs the Mersey engine hosted in that fork) and on the command line
// (Node / Bun / Deno vs the Mersey CLI). This is the focused cut of
// report-pertech.html: only js and native — no transpiled/WASM legs — so the
// grid is the comparison, not a field of n/a. Web-only workloads show just the
// browser arena (Node can't touch the DOM); the compute workloads show both.
//
//   node bench/web/report-jsnative.mjs   # -> bench/web/report-jsnative.html
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { forPlatform, platformsIn } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const load = async (f) => {
  try { return JSON.parse(await readFile(join(here, f), "utf8")); }
  catch { return []; }
};

// ---- browser arena: js vs fork-native, per browser ----------------------------
const browserRows = [
  ...(await load("results.stock.json")),
  ...(await load("results.native.json")),
  ...(await load("results.native.servo.json")),
  ...(await load("results.native.ladybird.json")),
  ...(await load("results.native.chromium.json")),
];
const platform = process.env.BENCH_PLATFORM ||
  (platformsIn(browserRows).includes("macos") ? "macos" : "linux");
const rows = forPlatform(browserRows, platform);

// Fork → { js leg key, native leg key }. Order is the display order.
const FORKS = [
  { key: "cr", name: "Chromium", js: "chromium/js", native: "chromium-fork/native" },
  { key: "ff", name: "Firefox", js: "firefox/js", native: "firefox-fork/native" },
  { key: "sv", name: "Servo", js: "servo/js", native: "servo-fork/native" },
  { key: "lb", name: "Ladybird", js: "ladybird/js", native: "ladybird-fork/native" },
];
const legFor = new Map();
for (const f of FORKS) { legFor.set(f.js, [f.key, "js"]); legFor.set(f.native, [f.key, "native"]); }

// DATA[wl].browser[forkKey] = { js:{t,m}, native:{t,m} }
const DATA = {};
for (const r of rows) {
  if (r.ms == null) continue;
  const hit = legFor.get(`${r.browser}/${r.impl}`);
  if (!hit) continue;
  const [fork, kind] = hit;
  const d = (DATA[r.wl] ??= { browser: {}, cli: null });
  const cell = ((d.browser[fork] ??= {})[kind] ??= {});
  cell.t = Number(r.ms.toFixed(1));
  if (r.rss != null) cell.m = Number((r.rss / 1024).toFixed(1)); // KiB -> MiB
}

// ---- command-line arena: node / bun / deno vs the Mersey CLI -------------------
const cli = await load("../cli/results.json");
const CLI_RT = [
  { key: "node", name: "Node.js" },
  { key: "bun", name: "Bun" },
  { key: "deno", name: "Deno" },
  { key: "mersey", name: "Mersey CLI" },
];
for (const r of (cli.rows ?? [])) {
  const d = (DATA[r.wl] ??= { browser: {}, cli: null });
  d.cli = {};
  for (const rt of CLI_RT) {
    const leg = r.legs?.[rt.key];
    if (leg) d.cli[rt.key] = { t: Number(leg.work?.toFixed?.(1) ?? leg.work), m: leg.rssMiB };
  }
}

// Compute workloads (both arenas) first, then the web-only ones. Within each
// group, alphabetical.
const COMPUTE = new Set(["calls", "compute", "fcompute", "mathk"]);
const WL = Object.keys(DATA).sort((a, b) => {
  const ca = COMPUTE.has(a), cb = COMPUTE.has(b);
  return ca === cb ? a.localeCompare(b) : ca ? -1 : 1;
});

// ---- rendering ----------------------------------------------------------------
const T_MIN = 1, T_MAX = 60000; // engine `calls` command-line leg tops ~31 s
const logPct = (v) =>
  Math.max(2, (Math.log10(Math.max(v, T_MIN)) - Math.log10(T_MIN)) /
    (Math.log10(T_MAX) - Math.log10(T_MIN)) * 100);
const fmtMs = (v) => v >= 1000 ? (v / 1000).toFixed(1) + " s" : v;
const fmtMi = (v) => v == null ? "" : (v < 0.05 ? "≈0" : v.toFixed(1));

// One bar. `cls` sets the colour (js | native | node | bun | deno | mersey).
const bar = (lab, v, pct, txt, cls) => v == null
  ? `<div class="bar-row"><div class="bar-lab">${lab}</div><div class="track">
       <div class="bar ${cls}" style="width:2%;opacity:.28">
         <span class="val out" style="color:var(--ink-faint)">n/a</span></div></div></div>`
  : `<div class="bar-row"><div class="bar-lab">${lab}</div><div class="track">
       <div class="bar ${cls}" style="width:${pct.toFixed(1)}%">
         <span class="val${pct < 30 ? " out" : ""}">${txt}</span></div></div></div>`;

// A stack of bars for one arena; `get(cell)` picks time or memory.
function arena(title, entries, get, pctFn, fmtV) {
  const bars = entries.map(({ lab, cell, cls }) => {
    const v = cell ? get(cell) : null;
    return bar(lab, v, v == null ? 0 : pctFn(v), v == null ? "" : fmtV(v), cls);
  }).join("");
  return `<div class="ar"><div class="ar-t">${title}</div><div class="bars">${bars}</div></div>`;
}

const panels = WL.map((w) => {
  const d = DATA[w];
  // Browser arena rows: js then native, per fork.
  const bEntries = FORKS.flatMap((f) => {
    const g = d.browser[f.key] ?? {};
    return [
      { lab: `js·${f.key}`, cell: g.js, cls: "js" },
      { lab: `mersey·${f.key}`, cell: g.native, cls: "native" },
    ];
  });
  const cliEntries = d.cli
    ? CLI_RT.map((rt) => ({ lab: rt.key, cell: d.cli[rt.key], cls: rt.key === "mersey" ? "native" : rt.key }))
    : null;

  // Memory scale is per-panel (bars are relative within a workload).
  const bMemMax = Math.max(1, ...bEntries.map((e) => e.cell?.m).filter((v) => v != null));
  const cMemMax = cliEntries ? Math.max(1, ...cliEntries.map((e) => e.cell?.m).filter((v) => v != null)) : 1;
  const memPct = (max) => (v) => Math.max(2, v / max * 100);

  const browserBlock = `
    <div class="arena-grid">
      ${arena("browser · time (ms, log)", bEntries, (c) => c.t, logPct, fmtMs)}
      ${arena("browser · memory (MiB)", bEntries, (c) => c.m, memPct(bMemMax), fmtMi)}
    </div>`;
  const cliBlock = cliEntries ? `
    <div class="arena-grid cli">
      ${arena("command line · time (ms, log)", cliEntries, (c) => c.t, logPct, fmtMs)}
      ${arena("command line · memory (MiB)", cliEntries, (c) => c.m, memPct(cMemMax), fmtMi)}
    </div>` : "";

  const tag = COMPUTE.has(w) ? "browser + command line" : "browser only";
  return `<details class="pt" id="wl-${w}"${COMPUTE.has(w) ? " open" : ""}>
    <summary class="wl-name"><span class="chev"></span><b>${w}</b><em>${tag}</em></summary>
    ${browserBlock}${cliBlock}
  </details>`;
}).join("\n");

const toc = `<nav class="toc">${WL.map((w) => `<a href="#wl-${w}">${w}</a>`).join("\n    ")}</nav>`;

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mersey — JS vs native</title>
<style>
  :root {
    --bg:#f7f9fa; --panel:#fff; --ink:#10191c; --ink-soft:#4a5a60; --ink-faint:#839499;
    --line:#dde5e8; --accent:#0a97c4;
    --js:#9aa7ad; --native:#0f9e88; --node:#5f8f6b; --bun:#c58aa8; --deno:#5b7391;
    --shadow:0 1px 2px rgba(16,25,28,.04),0 8px 24px -12px rgba(16,25,28,.12);
    --mono:ui-monospace,"SF Mono","JetBrains Mono",Menlo,Consolas,monospace;
    --sans:system-ui,-apple-system,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
  }
  @media (prefers-color-scheme: dark){:root{
    --bg:#0d1416; --panel:#131e21; --ink:#eaf2f4; --ink-soft:#9fb2b8; --ink-faint:#6b7f85;
    --line:#253437; --accent:#34c0e8;
    --js:#6b7a80; --native:#2bc0a6; --node:#6fae7f; --bun:#d59bbb; --deno:#7c94b4;
    --shadow:0 1px 2px rgba(0,0,0,.3),0 12px 30px -14px rgba(0,0,0,.5);}}
  :root[data-theme="dark"]{--bg:#0d1416;--panel:#131e21;--ink:#eaf2f4;--ink-soft:#9fb2b8;--ink-faint:#6b7f85;--line:#253437;--accent:#34c0e8;--js:#6b7a80;--native:#2bc0a6;--node:#6fae7f;--bun:#d59bbb;--deno:#7c94b4;--shadow:0 1px 2px rgba(0,0,0,.3),0 12px 30px -14px rgba(0,0,0,.5);}
  :root[data-theme="light"]{--bg:#f7f9fa;--panel:#fff;--ink:#10191c;--ink-soft:#4a5a60;--ink-faint:#839499;--line:#dde5e8;--accent:#0a97c4;--js:#9aa7ad;--native:#0f9e88;--node:#5f8f6b;--bun:#c58aa8;--deno:#5b7391;--shadow:0 1px 2px rgba(16,25,28,.04),0 8px 24px -12px rgba(16,25,28,.12);}
  body{margin:0;background:var(--bg);color:var(--ink);font-family:var(--sans);line-height:1.55;}
  .wrap{max-width:1100px;margin:0 auto;padding:2.2rem 1.2rem 3rem;}
  h1{font-size:1.5rem;margin:0 0 .3rem;}
  .note{color:var(--ink-soft);font-size:.92rem;max-width:74ch;margin:.3rem 0 1rem;}
  .legend{display:flex;flex-wrap:wrap;gap:1.1rem;margin:.8rem 0 1.2rem;font-size:.85rem;}
  .legend span{display:inline-flex;align-items:center;gap:.45rem;color:var(--ink-soft);}
  .sw{width:11px;height:11px;border-radius:3px;display:inline-block;}
  .sw.js{background:var(--js);}.sw.native{background:var(--native);}
  .sw.node{background:var(--node);}.sw.bun{background:var(--bun);}.sw.deno{background:var(--deno);}
  .toc{display:flex;flex-wrap:wrap;gap:.35rem .5rem;margin:0 0 1.1rem;}
  .toc a{font-family:var(--mono);font-size:.75rem;color:var(--accent);text-decoration:none;border:1px solid var(--line);border-radius:999px;padding:.18rem .65rem;background:var(--panel);}
  .toc a:hover{border-color:var(--accent);}
  .pt{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:.55rem 1.4rem;box-shadow:var(--shadow);margin:0 0 .6rem;scroll-margin-top:1rem;}
  .pt[open]{padding:1.1rem 1.4rem .9rem;}
  .arena-grid{display:grid;grid-template-columns:1fr 1fr;gap:0 1.8rem;}
  .arena-grid.cli{margin-top:.9rem;padding-top:.8rem;border-top:1px dashed var(--line);}
  @media (max-width:760px){.arena-grid{grid-template-columns:1fr;}}
  .ar-t{font-family:var(--mono);font-size:.72rem;color:var(--ink-faint);margin:.15rem 0 .4rem;}
  .wl-name{font-family:var(--mono);font-size:.8rem;color:var(--ink);display:flex;align-items:center;cursor:pointer;list-style:none;}
  .wl-name::-webkit-details-marker{display:none;}
  .pt[open] .wl-name{margin-bottom:.55rem;}
  .wl-name .chev{flex:none;margin-right:.55rem;border:solid var(--ink-faint);border-width:0 2px 2px 0;padding:2.5px;transform:rotate(-45deg);transition:transform .12s ease;}
  .pt[open] .wl-name .chev{transform:rotate(45deg);}
  .wl-name b{font-weight:600;}
  .wl-name em{font-style:normal;color:var(--ink-faint);margin-left:auto;}
  .bars{display:grid;gap:5px;}
  .bar-row{display:grid;grid-template-columns:70px 1fr;align-items:center;gap:.6rem;}
  .bar-lab{font-family:var(--mono);font-size:.7rem;color:var(--ink-faint);text-align:right;}
  .track{position:relative;height:20px;}
  .bar{height:100%;border-radius:4px;min-width:2px;display:flex;align-items:center;justify-content:flex-end;position:relative;max-width:100%;}
  .bar.js{background:var(--js);}.bar.native{background:var(--native);}
  .bar.node{background:var(--node);}.bar.bun{background:var(--bun);}.bar.deno{background:var(--deno);}
  .bar .val{font-family:var(--mono);font-size:.68rem;color:#fff;padding:0 6px;white-space:nowrap;font-variant-numeric:tabular-nums;}
  .bar .val.out{color:var(--ink-soft);position:absolute;left:calc(100% + 6px);}
  footer{color:var(--ink-faint);font-size:.8rem;margin-top:1.6rem;}
</style>
</head>
<body>
<div class="wrap">
  <h1>Mersey — JS vs native</h1>
  <p class="note">The same program as plain JavaScript and as native Mersey, in two arenas.
    <b>Browser</b> — each fork's own JS engine (<i>js</i>) vs the Mersey engine hosted natively in
    that fork (<i>mersey</i>), for Chromium (·cr), Firefox (·ff), Servo (·sv), Ladybird (·lb).
    <b>Command line</b> — Node, Bun and Deno running the JS twin vs the Mersey CLI running the Mersey
    twin (compute workloads only — Node has no DOM). Time is the self-timed workload loop
    (median, <b>log scale</b>, same checksum on every bar); memory is the process's peak/PSS delta.
    Platform: <b>${platform}</b>. A dimmed <i>n/a</i> row is honest absence (a fork that can't run
    that workload). This is report-pertech's js-vs-native cut, so there is no transpiled or WASM
    row to leave empty.</p>
${toc}
  <div class="legend">
    <span><i class="sw js"></i> plain JS (browser engine)</span>
    <span><i class="sw native"></i> native Mersey (fork engine / CLI)</span>
    <span><i class="sw node"></i> Node.js</span>
    <span><i class="sw bun"></i> Bun</span>
    <span><i class="sw deno"></i> Deno</span>
  </div>
${panels}
  <footer>Generated by <code>bench/web/report-jsnative.mjs</code> from <code>bench/web/results.*.json</code>
    and <code>bench/cli/results.json</code>. The full multi-leg report is <code>report.html</code>;
    per-technology (all legs) is <code>report-pertech.html</code>.</footer>
</div>
<script>
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

await writeFile(join(here, "report-jsnative.html"), html);
console.log(`wrote bench/web/report-jsnative.html (${WL.length} panels, platform ${platform})`);
