// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Native leg of the web-platform benchmark: the same Mersey workloads, run by
// the engine hosted INSIDE the Firefox fork (dom/mersey), reaching web APIs
// through the universal bridge in C++. Compare with the "poly" rows from
// run.mjs (same source, engine compiled to WASM) and the "js" rows (plain JS).
//
// The fork supports inline <script type="text/mersey"> only, so each workload
// is inlined into a generated page. console.log is echoed to stdout by the
// fork when MERSEY_CONSOLE_STDOUT is set; that is how we capture the RESULT.
import { readFile, writeFile, readdir, mkdtemp, mkdir } from "node:fs/promises";
import { spawn, execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { startServer } from "./server.mjs";
import { treeMemoryByCmdline } from "./host-mem.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

// A workload's checksum is not always a number: `fcompute` and `mathk` self-check
// with a boolean, because float bit parity across two independent codegens is not
// guaranteed. Parsing with `Number()` turned those into NaN -> `null` in the
// results file, which quietly threw away the correctness proof for two of the
// eight compute workloads on every browser leg (and made every parity check
// compare null to null and pass). Keep numbers as numbers so the file shape does
// not change; keep anything else as the token the workload printed.
const parseChecksum = (raw) => (/^-?\d+$/.test(raw) ? Number(raw) : raw);

// Time and memory are independent measurements of the same run set, so each
// gets its own median. Reporting `samples[medianByTime].rss` — which is what
// this did — picks one arbitrary memory reading out of the repeats, and browser
// footprint swings by tens of MiB between launches; that is how a workload's
// delta against the blank baseline came out NEGATIVE often enough to matter.
const medianRss = (samples) => {
  const v = samples.map((s) => s.rss).filter((x) => x != null).sort((a, b) => a - b);
  return v.length ? v[Math.floor(v.length / 2)] : null;
};

// Reap the whole fork tree on any exit path so a crash or interrupt can't leave
// orphans (killForks is a hoisted declaration, safe to reference here).
process.on("unhandledRejection", (e) => { console.error("UNHANDLED", e); killForks(); process.exit(3); });
process.on("exit", () => killForks());
process.on("SIGINT", () => { killForks(); process.exit(130); });

const here = dirname(fileURLToPath(import.meta.url));
const FORK_DIR = process.env.GECKO_SRC
  ? `${process.env.GECKO_SRC}/obj-mersey`
  : join(here, "../../../browsers/firefox/obj-mersey");
// macOS: dist/bin/firefox is a raw binary that cannot resolve XPCOM/libxul
// ("Couldn't load XPCOM"); the engine lives in the .app bundle. Linux keeps
// the plain dist/bin path.
const FORK =
  process.platform === "darwin"
    ? `${FORK_DIR}/dist/Mersey Gecko (Experimental).app/Contents/MacOS/firefox`
    : `${FORK_DIR}/dist/bin/firefox`;
const PAGE_SIZE = 4096;
const REPEATS = 3;

const WORKLOADS = process.env.WL
  ? process.env.WL.split(",")
  : (await readdir(join(here, "mersey")))
      .filter((f) => f.endsWith(".mersey"))
      .map((f) => f.replace(/\.mersey$/, ""))
      .sort();

const pageDir = join(here, "pages", "native");
await mkdir(pageDir, { recursive: true });

// Generate one inlined page per workload (plus a blank baseline page).
for (const wl of WORKLOADS) {
  const src = await readFile(join(here, "mersey", `${wl}.mersey`), "utf8");
  const html = `<!doctype html>
<meta charset="utf-8">
<title>native ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
</body>`;
  await writeFile(join(pageDir, `${wl}.html`), html);
}
await writeFile(join(pageDir, "blank.html"),
  `<!doctype html><meta charset="utf-8"><body><div id="out"></div></body>`);

// Memory (KiB) over the whole fork process tree, counting a page shared by
// several processes once — libxul is mapped into every content process, and
// summing RSS over the tree overcounts it wildly. host-mem.mjs picks the metric
// the host can provide (Linux PSS / macOS de-duplicated footprint); the two are
// not comparable, which is why every row records which one it used.
const forkPss = (match) => treeMemoryByCmdline(match);

// Launch the fork on one page, capture the RESULT line and RSS, then kill it.
// For the blank baseline (expectResult=false) there is no RESULT, so just let
// the browser settle and sample RSS.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Kill every fork process — launcher AND content/GPU/socket children. Firefox
// content processes re-exec the same binary but do NOT share the launcher's
// process name, so `pkill firefox` misses them and they pile up until the VM is
// out of memory. Match the binary PATH instead, which every process in the tree
// carries in argv0. The `[f]irefox` class keeps this pkill from matching its own
// command line (a literal "firefox" would).
function killForks() {
  try { execSync(`pkill -9 -f "${FORK_DIR}/dist/bin/[f]irefox" 2>/dev/null`); } catch {}
}

// Quiet the browser: the background services (region lookup, telemetry, search,
// updates) fire network requests and steal CPU *during* the timed loop, which is
// what makes the full-browser numbers bounce. Disable them per profile.
const QUIET_PREFS = [
  'user_pref("toolkit.telemetry.enabled", false);',
  'user_pref("datareporting.healthreport.uploadEnabled", false);',
  'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
  'user_pref("browser.region.network.url", "");',
  'user_pref("browser.region.update.enabled", false);',
  'user_pref("app.update.enabled", false);',
  'user_pref("browser.search.update", false);',
  'user_pref("browser.newtabpage.enabled", false);',
  'user_pref("browser.startup.homepage_override.mstone", "ignore");',
  'user_pref("network.captive-portal-service.enabled", false);',
  'user_pref("browser.safebrowsing.downloads.enabled", false);',
  'user_pref("extensions.update.enabled", false);',
].join("\n");

async function writePrefs(profileDir) {
  const { writeFile } = await import("node:fs/promises");
  await writeFile(join(profileDir, "prefs.js"), QUIET_PREFS);
}

async function runPage(pageUrl, profileDir, expectResult = true) {
  killForks();
  await writePrefs(profileDir);
  await sleep(800); // let the previous tree's processes exit
  return new Promise((resolve) => {
    // detached: own process group, so we can kill the whole tree at the end.
    const child = spawn(
      FORK,
      ["--headless", "-no-remote", "-profile", profileDir, pageUrl],
      { env: { ...process.env, MERSEY_CONSOLE_STDOUT: "1", MOZ_HEADLESS: "1" }, detached: true },
    );
    let out = "";
    let result = null;
    let settled = false;
    const finish = async () => {
      if (settled) return;
      settled = true;
      const rss = await forkPss(FORK_DIR);
      try { process.kill(-child.pid, "SIGKILL"); } catch {}
      resolve({ result, rss });
    };
    child.stdout.on("data", (b) => {
      out += b.toString();
      const m = /RESULT (\S+) ([\d.]+) (\S+)/.exec(out);
      if (m && !result) result = { ms: Number(m[2]), checksum: parseChecksum(m[3]) };
    });
    if (process.env.MERSEY_DEBUG) child.stderr.on("data", (b) => process.stderr.write(b));
    if (process.env.MERSEY_DEBUG) child.stdout.on("data", (b) => process.stderr.write(b));
    child.on("error", (e) => { console.error("spawn error", e.message); finish(); });
    child.on("exit", () => { if (!settled) finish(); });
    // Sample memory at a fixed settle point for BOTH blank and workload, so the
    // browser is equally spun up in each — the workload itself finishes in well
    // under a second, so its allocations are present by then.
    setTimeout(finish, 8000);
  });
}

const rows = [];
const profileBase = await mkdtemp(join(tmpdir(), "mersey-fork-"));

// Serve the generated pages over http (not file://) so same-origin requests —
// the fetch workload's /bench/echo — work; the server is rooted at the repo.
const { server, port } = await startServer();
const base = `http://localhost:${port}/bench/web/pages/native`;

// Baseline RSS from the blank page.
let baseRss = 0;
{
  const prof = join(profileBase, "blank");
  await mkdir(prof, { recursive: true });
  const { rss } = await runPage(`${base}/blank.html`, prof, false);
  baseRss = rss ?? 0;
  console.log(`native  baseline blank rss ${baseRss} KiB\n`);
}

for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const prof = join(profileBase, `${wl}-${r}`);
    await mkdir(prof, { recursive: true });
    const { result, rss } = await runPage(`${base}/${wl}.html`, prof);
    if (result) samples.push({ ...result, rss: (rss ?? 0) - baseRss });
  }
  if (samples.length === 0) {
    console.log(`  native ${wl.padEnd(8)} — no result`);
    rows.push({ browser: "firefox-fork", impl: "native", wl, ms: null });
    continue;
  }
  samples.sort((a, b) => a.ms - b.ms);
  const med = samples[Math.floor(samples.length / 2)];
  const medRss = medianRss(samples);
  console.log(
    `  native ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(medRss).padStart(6)} KiB   (n=${samples.length})`,
  );
  rows.push({ browser: "firefox-fork", impl: "native", wl, ms: med.ms, rss: medRss, checksum: med.checksum });
}

killForks();
server.close();
// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl, platform) triples this run
// produced — so a macOS run leaves the Linux rows (and vice versa) intact.
const merged = await mergeRows(here, "results.native.json", tagRows(rows));
await writeFile(join(here, "results.native.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.native.json`);
