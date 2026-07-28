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
  ...(await load("results.stock.json")),     // chromium + firefox JS
  ...(await load("results.servo.json")),     // servo JS
  ...(await load("results.ladybird.json")),  // ladybird JS
  ...(await load("results.native.json")),    // firefox fork native
  ...(await load("results.native.servo.json")),
  ...(await load("results.native.ladybird.json")),
  ...(await load("results.native.chromium.json")),
];
const platform = process.env.BENCH_PLATFORM ||
  (platformsIn(browserRows).includes("macos") ? "macos" : "linux");
const rows = forPlatform(browserRows, platform);

// Fork → { js leg key, native leg key }. Order is the display order.
//
// `extra: true` marks a fork that is off by default and revealed by the
// selector at the top of the page. Chromium and Firefox are the ports under
// active work, so they and the command line are what the page opens on;
// Servo and Ladybird are measured and kept, just not in the way of the
// comparison most readers came for.
const FORKS = [
  { key: "cr", name: "Chromium", js: "chromium/js", native: "chromium-fork/native" },
  { key: "ff", name: "Firefox", js: "firefox/js", native: "firefox-fork/native" },
  { key: "sv", name: "Servo", js: "servo/js", native: "servo-fork/native", extra: true },
  { key: "lb", name: "Ladybird", js: "ladybird/js", native: "ladybird-fork/native", extra: true },
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

// ---- why a cell has no bar ----------------------------------------------------
//
// Omitting an unmeasured cell keeps the grid honest, but on its own it cannot
// tell "this fork cannot do it" apart from "nobody has run it yet". Each rule
// below was established by running the workload in the fork and reading the
// error it printed, not inferred from the missing row. A rule with no `wls`
// matches every gap in that leg. Rules are tried in order; the catch-all at the
// end is deliberately an admission, not an excuse.
const CHROMIUM_BRIDGE =
  "The Chromium fork constructs host objects in C++ from a hand-written allowlist " +
  "(<code>TextEncoder</code>, <code>TextDecoder</code>, <code>DOMMatrix</code>, " +
  "<code>Blob</code>, <code>URL</code>); anything else throws " +
  "<code>unknown constructor</code>. The Firefox fork reaches the same interfaces " +
  "through the reflective bridge, which is why its column is complete.";
const CHROMIUM_GLOBALS =
  "The Chromium fork does not expose these globals to Mersey: the page throws " +
  "<code>`fetch` is not defined</code>, <code>`indexedDB` is not defined</code>, " +
  "<code>`navigator` is not defined</code>, <code>`location` is not defined</code>. " +
  "Same root as the row above — a hand-written native surface rather than reflection.";
const CHROMIUM_BUG =
  "A fork bug, distinct from the two gaps above: the workload throws " +
  "<code>no member `length` on null</code>, so a bridge call returns null where the " +
  "workload expects a list.";
const PAUSED = (who) =>
  `Not investigated — ${who} support is paused, so these were left as measured.`;
const RUNNER_LIST = (who) =>
  `The ${who} runner takes a hard-coded workload list that predates these three ` +
  "compute kernels; they are not attempted, rather than attempted and failed.";

const WHY_RULES = [
  { leg: "msy·cr", wls: ["bchannel", "compression", "msgchannel", "sse", "streams", "urlpattern", "worker", "xhr"], why: CHROMIUM_BRIDGE },
  { leg: "msy·cr", wls: ["fetch", "idb", "locks", "websocket"], why: CHROMIUM_GLOBALS },
  { leg: "msy·cr", wls: ["frameworkui"], why: CHROMIUM_BUG },
  { leg: "js·sv", wls: ["calls", "fcompute", "mathk"], why: RUNNER_LIST("Servo") },
  { leg: "js·lb", wls: ["calls", "fcompute", "mathk"], why: RUNNER_LIST("Ladybird") },
  { leg: "msy·sv", wls: ["calls", "fcompute", "mathk"], why: RUNNER_LIST("Servo") },
  { leg: "msy·lb", wls: ["calls", "fcompute", "mathk"], why: RUNNER_LIST("Ladybird") },
  { leg: "js·sv", why: PAUSED("Servo") },
  { leg: "msy·sv", why: PAUSED("Servo") },
  { leg: "js·lb", why: PAUSED("Ladybird") },
  { leg: "msy·lb", why: PAUSED("Ladybird") },
];
const whyFor = (leg, wl) =>
  WHY_RULES.find((r) => r.leg === leg && (!r.wls || r.wls.includes(wl)))?.why ??
  "Not yet investigated — nobody has run this cell and read the error.";

// ---- rendering ----------------------------------------------------------------
const T_MIN = 1, T_MAX = 60000; // engine `calls` command-line leg tops ~31 s
const logPct = (v) =>
  Math.max(2, (Math.log10(Math.max(v, T_MIN)) - Math.log10(T_MIN)) /
    (Math.log10(T_MAX) - Math.log10(T_MIN)) * 100);
const fmtMs = (v) => v >= 1000 ? (v / 1000).toFixed(1) + " s" : v;
const fmtMi = (v) => v == null ? "" : (v < 0.05 ? "≈0" : v.toFixed(1));

// The browser arena's memory number is a *delta*: the process tree's footprint
// on the workload page minus its footprint on a blank page, from two separate
// launches. A browser's footprint moves by hundreds of KiB between launches, so
// a workload that allocates almost nothing lands inside that noise and can come
// back slightly negative — Chromium-native `crypto` measured -256 KiB against a
// 96,834 KiB baseline, which is 0.3%.
//
// That is a real measurement at the floor of the method, not a missing one, and
// the two deserve different treatment: an absent cell gets no row, while a
// floor cell keeps its row and says `<0.5` so the reader can see the leg ran.
// Drawing it to scale instead would put a visible bar on noise.
const MEM_FLOOR_MIB = 0.5;
const atMemFloor = (c) => c.m != null && c.m <= MEM_FLOOR_MIB;
const memOrNull = (c) => (c.m == null ? null : Math.max(c.m, MEM_FLOOR_MIB));

// One measured bar. `cls` sets the colour (js | native | node | bun | deno | mersey);
// `fk` tags the row with its fork so the selector can hide it; `v` is the raw
// value, kept on the row because a memory arena's scale is relative and has to
// be recomputed whenever the visible set changes.
const bar = (lab, pct, txt, cls, fk, v) =>
  `<div class="bar-row${fk ? ` fk-${fk}` : ""}" data-v="${v}"><div class="bar-lab">${lab}</div><div class="track">
     <div class="bar ${cls}" style="width:${pct.toFixed(1)}%">
       <span class="val${pct < 30 ? " out" : ""}">${txt}</span></div></div></div>`;

// A stack of bars for one arena; `get(cell)` picks time or memory. Rows with no
// value are OMITTED, not drawn as n/a — the grid stays the comparison. Returns
// "" when the arena has nothing to show, so the caller can drop it entirely.
// `kind` is "time" (log, absolute) or "mem" (linear, relative to the widest
// *visible* row — which is why the mem arenas are re-scaled client-side).
function arena(title, entries, get, pctFn, fmtV, kind) {
  const withVal = entries
    .map((e) => ({ ...e, v: e.cell ? get(e.cell) : null }))
    .filter((e) => e.v != null);
  if (!withVal.length) return "";
  const bars = withVal
    .map((e) => {
      const floor = kind === "mem" && atMemFloor(e.cell);
      return bar(e.lab, pctFn(e.v), floor ? `&lt;${MEM_FLOOR_MIB}` : fmtV(e.v),
                 e.cls + (floor ? " sub" : ""), e.fk, e.v);
    })
    .join("");
  return `<div class="ar ${kind}"><div class="ar-t">${title}</div><div class="bars">${bars}</div></div>`;
}

const panels = WL.map((w) => {
  const d = DATA[w];
  // Browser arena rows: js then native, per fork.
  const bEntries = FORKS.flatMap((f) => {
    const g = d.browser[f.key] ?? {};
    return [
      { lab: `js·${f.key}`, cell: g.js, cls: "js", fk: f.key },
      { lab: `mersey·${f.key}`, cell: g.native, cls: "native", fk: f.key },
    ];
  });
  const cliEntries = d.cli
    ? CLI_RT.map((rt) => ({ lab: rt.key, cell: d.cli[rt.key], cls: rt.key === "mersey" ? "native" : rt.key }))
    : null;

  // Memory scale is per-panel and relative, so it is seeded from the forks that
  // are visible on load; revealing Servo/Ladybird re-scales it in the browser.
  const memOf = (es) => es.map((e) => (e.cell ? memOrNull(e.cell) : null)).filter((v) => v != null);
  // `1` only guards the no-values case; using it as a floor would under-scale a
  // panel whose largest reading is a fraction of a MiB (a lone 0.8 MiB row drew
  // at 80% of the track instead of filling it).
  const maxOf = (es) => { const v = memOf(es); return v.length ? Math.max(...v) : 1; };
  const shown = bEntries.filter((e) => !FORKS.find((f) => f.key === e.fk)?.extra);
  const bMemMax = maxOf(shown.length ? shown : bEntries);
  const cMemMax = cliEntries ? maxOf(cliEntries) : 1;
  const memPct = (max) => (v) => Math.max(2, Math.min(100, v / max * 100));

  const bTime = arena("browser · time (ms, log)", bEntries, (c) => c.t, logPct, fmtMs, "time");
  const bMem = arena("browser · memory (MiB)", bEntries, memOrNull, memPct(bMemMax), fmtMi, "mem");
  const browserBlock = (bTime || bMem) ? `<div class="arena-grid">${bTime}${bMem}</div>` : "";

  const cTime = cliEntries ? arena("command line · time (ms, log)", cliEntries, (c) => c.t, logPct, fmtMs, "time") : "";
  const cMem = cliEntries ? arena("command line · memory (MiB)", cliEntries, memOrNull, memPct(cMemMax), fmtMi, "mem") : "";
  const cliBlock = (cTime || cMem) ? `<div class="arena-grid cli">${cTime}${cMem}</div>` : "";

  if (!browserBlock && !cliBlock) return "";
  return `<details class="pt" id="wl-${w}"${COMPUTE.has(w) ? " open" : ""}>
    <summary class="wl-name"><span class="chev"></span><b>${w}</b><em></em></summary>
    ${browserBlock}${cliBlock}
  </details>`;
}).filter(Boolean).join("\n");

const toc = `<nav class="toc">${WL.map((w) => `<a href="#wl-${w}" data-wl="${w}">${w}</a>`).join("\n    ")}</nav>`;

// ---- coverage: every browser cell with no bar, grouped by cause ---------------
// Derived from DATA, never hand-listed, so it cannot drift from the grid above.
const gaps = new Map(); // why -> Map(leg -> [wl])
let cells = 0, missing = 0;
for (const w of WL) {
  for (const f of FORKS) {
    for (const [kind, leg] of [["js", `js·${f.key}`], ["native", `msy·${f.key}`]]) {
      cells++;
      if (DATA[w].browser[f.key]?.[kind]?.t != null) continue;
      missing++;
      const why = whyFor(leg, w);
      const byLeg = gaps.get(why) ?? (gaps.set(why, new Map()), gaps.get(why));
      byLeg.set(leg, [...(byLeg.get(leg) ?? []), w]);
    }
  }
}
const covRows = [...gaps.entries()]
  .sort((a, b) => a[0].localeCompare(b[0]))
  .flatMap(([why, byLeg]) => [...byLeg.entries()].map(([leg, ws]) =>
    `<tr><td class="cov-leg">${leg}</td><td class="cov-wl">${ws.sort().join(" ")}</td><td>${why}</td></tr>`))
  .join("\n      ");
const coverage = `<details class="cov">
  <summary><b>Coverage</b> — ${missing} of ${cells} browser cells have no bar, and why</summary>
  <table>
    <thead><tr><th>leg</th><th>workloads</th><th>why</th></tr></thead>
    <tbody>
      ${covRows}
    </tbody>
  </table>
  <p class="cov-note">The command-line arena covers only the workloads with a Mersey twin that needs
    no host API — the DOM and networked workloads have no Node equivalent, so they show a browser
    arena alone. A memory bar reading <code>&lt;${MEM_FLOOR_MIB}</code> is hatched: the delta between
    the workload page and a blank page fell inside the noise of two separate browser launches, which
    is a measurement at the floor of the method rather than a missing one.</p>
</details>`;

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<script>
  /* Applied before first paint so the default view never flashes the extra
     forks. The stored choice wins over the default. */
  document.documentElement.dataset.forks =
    (() => { try { const v = localStorage.getItem("mersey-jsnative-forks");
                   return ["none", "sv", "lb", "all"].includes(v) ? v : "none"; }
             catch { return "none"; } })();
</script>
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
  .picker{display:flex;align-items:center;gap:.55rem;margin:0 0 1.1rem;font-size:.85rem;color:var(--ink-soft);}
  .picker select{font-family:var(--sans);font-size:.85rem;color:var(--ink);background:var(--panel);
    border:1px solid var(--line);border-radius:8px;padding:.3rem .55rem;}
  .picker select:focus-visible{outline:2px solid var(--accent);outline-offset:1px;}
  /* Servo and Ladybird are rendered but folded away until the selector asks for
     them; the re-scaling of the relative memory bars happens in script. */
  [data-forks="none"] .fk-sv,[data-forks="none"] .fk-lb,
  [data-forks="sv"] .fk-lb,[data-forks="lb"] .fk-sv{display:none;}
  .toc{display:flex;flex-wrap:wrap;gap:.35rem .5rem;margin:0 0 1.1rem;}
  .toc a.off{display:none;}
  /* .arena-grid sets display:grid, which would otherwise beat the UA rule for
     the hidden attribute the selector script uses. */
  [hidden]{display:none !important;}
  .toc a{font-family:var(--mono);font-size:.75rem;color:var(--accent);text-decoration:none;border:1px solid var(--line);border-radius:999px;padding:.18rem .65rem;background:var(--panel);}
  .toc a:hover{border-color:var(--accent);}
  .cov{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:.55rem 1.4rem;
    box-shadow:var(--shadow);margin:0 0 1rem;font-size:.85rem;}
  .cov[open]{padding-bottom:1rem;}
  .cov summary{cursor:pointer;color:var(--ink-soft);}
  .cov table{width:100%;border-collapse:collapse;margin-top:.8rem;}
  .cov th{text-align:left;font-weight:600;color:var(--ink-faint);font-size:.72rem;
    text-transform:uppercase;letter-spacing:.04em;padding:0 .8rem .35rem 0;border-bottom:1px solid var(--line);}
  .cov td{padding:.5rem .8rem .5rem 0;border-bottom:1px solid var(--line);vertical-align:top;color:var(--ink-soft);}
  .cov td.cov-leg,.cov td.cov-wl{font-family:var(--mono);font-size:.72rem;color:var(--ink);}
  .cov td.cov-wl{max-width:19ch;}
  .cov code{font-family:var(--mono);font-size:.8em;}
  .cov-note{color:var(--ink-faint);margin:.9rem 0 0;}
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
  /* A reading at the floor of the delta method: the bar is there so the row
     reads as measured, hatched so it never reads as a quantity. */
  .bar.sub{background-image:repeating-linear-gradient(45deg,rgba(255,255,255,.55) 0 2px,transparent 2px 5px);opacity:.65;}
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
    that fork (<i>mersey</i>), for Chromium (·cr) and Firefox (·ff).
    <b>Command line</b> — Node, Bun and Deno running the JS twin vs the Mersey CLI running the Mersey
    twin (compute workloads only — Node has no DOM). Time is the self-timed workload loop
    (median, <b>log scale</b>, same checksum on every bar); memory is the process's peak/PSS delta
    and is scaled within each panel, so revealing a fork re-scales those bars.
    Platform: <b>${platform}</b>. Rows appear only where measured: a fork that can't run a workload,
    a compute kernel with no in-browser JS twin (<code>calls</code>, <code>fcompute</code>,
    <code>mathk</code> — those live in the command-line arena), or a memory delta that never rose
    clear of the runner's baseline, simply has no bar. The grid is the comparison, not a field of
    n/a. Servo and Ladybird are measured too — the selector brings them in.</p>
  <div class="picker">
    <label for="forks">Browser ports</label>
    <select id="forks">
      <option value="none">Chromium and Firefox</option>
      <option value="sv">…and Servo</option>
      <option value="lb">…and Ladybird</option>
      <option value="all">All four ports</option>
    </select>
  </div>
${toc}
  <div class="legend">
    <span><i class="sw js"></i> plain JS (browser engine)</span>
    <span><i class="sw native"></i> native Mersey (fork engine / CLI)</span>
    <span><i class="sw node"></i> Node.js</span>
    <span><i class="sw bun"></i> Bun</span>
    <span><i class="sw deno"></i> Deno</span>
  </div>
${coverage}
${panels}
  <footer>Generated by <code>bench/web/report-jsnative.mjs</code> from <code>bench/web/results.*.json</code>
    and <code>bench/cli/results.json</code>. The full multi-leg report is <code>report.html</code>;
    per-technology (all legs) is <code>report-pertech.html</code>.</footer>
</div>
<script>
  const sel = document.getElementById("forks");
  const CHOICES = ["none", "sv", "lb", "all"];
  sel.value = CHOICES.includes(document.documentElement.dataset.forks)
    ? document.documentElement.dataset.forks : "none";

  /* Structural, not layout-based: a row inside a collapsed <details> has no box
     but is still part of the comparison, so asking the DOM whether it is on
     screen would give the wrong answer for every closed panel. */
  const shown = (row, v) =>
    row.classList.contains("fk-sv") ? v === "sv" || v === "all"
    : row.classList.contains("fk-lb") ? v === "lb" || v === "all"
    : true;

  /* A memory arena's bars are relative to the widest row *in that arena*, so the
     scale is only honest if it comes from the rows on show. Time is a log scale
     against a fixed range and needs no such fix-up. */
  function rescale(v) {
    for (const ar of document.querySelectorAll(".ar.mem")) {
      const rows = [...ar.querySelectorAll(".bar-row")].filter((r) => shown(r, v));
      if (!rows.length) continue;
      const max = Math.max(...rows.map((r) => Number(r.dataset.v)));
      for (const r of rows) {
        const pct = Math.max(2, Math.min(100, Number(r.dataset.v) / max * 100));
        const b = r.querySelector(".bar");
        b.style.width = pct.toFixed(1) + "%";
        b.querySelector(".val").classList.toggle("out", pct < 30);
      }
    }
  }

  /* An arena, an arena grid or a whole panel with nothing left to show is
     hidden rather than left as an empty frame — and its table-of-contents chip
     goes with it, so the index never points at something that isn't there. */
  function prune(v) {
    const any = (el) => [...el.querySelectorAll(".bar-row")].some((r) => shown(r, v));
    for (const ar of document.querySelectorAll(".ar")) ar.hidden = !any(ar);
    for (const g of document.querySelectorAll(".arena-grid")) g.hidden = !any(g);
    for (const p of document.querySelectorAll(".pt")) {
      const grids = [...p.querySelectorAll(".arena-grid")].filter((g) => !g.hidden);
      p.hidden = !grids.length;
      p.querySelector(".wl-name em").textContent =
        grids.map((g) => g.classList.contains("cli") ? "command line" : "browser").join(" + ");
      const chip = document.querySelector('.toc a[data-wl="' + p.id.slice(3) + '"]');
      if (chip) chip.classList.toggle("off", !grids.length);
    }
  }

  function apply(v) {
    document.documentElement.dataset.forks = v;
    try { localStorage.setItem("mersey-jsnative-forks", v); } catch {}
    prune(v);
    rescale(v);
  }
  sel.addEventListener("change", () => apply(sel.value));
  apply(sel.value);

  const openHash = () => {
    const el = location.hash && document.querySelector(location.hash);
    if (el && el.tagName === "DETAILS" && !el.hidden) { el.open = true; el.scrollIntoView(); }
  };
  addEventListener("hashchange", openHash);
  openHash();
</script>
</body>
</html>
`;

await writeFile(join(here, "report-jsnative.html"), html);
console.log(`wrote bench/web/report-jsnative.html (${WL.length} panels, platform ${platform})`);
