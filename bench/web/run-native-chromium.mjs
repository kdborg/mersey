// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Native leg of the web-platform benchmark, Chromium edition: the same Mersey
// workloads run by the engine hosted INSIDE the Chromium fork (Blink's
// mersey_script_runner), reaching web APIs through the universal bridge in C++.
// The Gecko twin is run-native.mjs; this one drives the chrome binary instead.
//
// The fork supports inline <script type="text/mersey"> only, so each workload is
// inlined into a generated page (the same pages run-native.mjs generates). Blink
// routes console.log to its console; with --enable-logging=stderr --v=1 the
// RESULT line surfaces on stderr, which is how we capture it.
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

process.on("unhandledRejection", (e) => { console.error("UNHANDLED", e); killForks(); process.exit(3); });
process.on("exit", () => killForks());
process.on("SIGINT", () => { killForks(); process.exit(130); });

const here = dirname(fileURLToPath(import.meta.url));
// macOS: the stub launcher resolves its framework relative to argv[0], so it
// MUST be invoked through the .app bundle path — a bare `chrome` symlink makes
// dlopen look in out/../Frameworks (nonexistent) and the stub aborts
// (main -> FatalError). Linux keeps the plain `chrome`.
const CHROMIUM_SRC = process.env.CHROMIUM_SRC || join(here, "../../../browsers/chromium/src");
// Which GN output directory. Overridable because the honest way to measure what
// a build *flag* costs is to build it both ways and run the same workloads
// against each — `CHROMIUM_OUT=mersey-nodcheck` beside the default is how the
// `dcheck_always_on` question gets an answer rather than a caveat.
const CHROMIUM_OUT = process.env.CHROMIUM_OUT || "mersey-arm64";
const FORK =
  process.platform === "darwin"
    ? `${CHROMIUM_SRC}/out/${CHROMIUM_OUT}/Mersey Blink (Experimental).app/Contents/MacOS/Mersey Blink (Experimental)`
    : `${CHROMIUM_SRC}/out/${CHROMIUM_OUT}/chrome`;
// Repeats per workload. Three is enough to reject a single bad launch but not
// to resolve a change smaller than the machine's own drift — a browser leg on a
// busy laptop moved 33% between sweeps with no code touching it. `REPEATS=15`
// (or more) is what an overnight run should use; the runner takes the median of
// each metric independently, so more samples buy resolution directly.
const REPEATS = Number(process.env.REPEATS ?? 3);

const WORKLOADS = process.env.WL
  ? process.env.WL.split(",")
  : (await readdir(join(here, "mersey")))
      .filter((f) => f.endsWith(".mersey"))
      .map((f) => f.replace(/\.mersey$/, ""))
      .sort();

// Reuse the same inlined pages run-native.mjs writes (regenerate to be safe).
const pageDir = join(here, "pages", "native");
await mkdir(pageDir, { recursive: true });
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

// Sum PSS (KiB) over the chrome process tree — shared pages counted once.
const forkPss = (match) => treeMemoryByCmdline(match);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Kill every chrome process in the fork tree — the launcher and every
// renderer/gpu/zygote child re-execs the same binary path, so match on that.
function killForks() {
  try { execSync(`pkill -9 -f "${FORK}" 2>/dev/null`); } catch {}
}

async function runPage(pageUrl, profileDir, expectResult = true) {
  killForks();
  await sleep(800); // let the previous tree's processes exit
  return new Promise((resolve) => {
    const child = spawn(
      FORK,
      [
        "--headless=new",
        "--no-sandbox",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--enable-logging=stderr",
        // Quiet the background services that would steal CPU during the timed loop.
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-breakpad",
        "--disable-sync",
        "--metrics-recording-only",
        // OSCrypt otherwise asks the login keychain for "Chromium Safe Storage"
        // — the key that decrypts the *real* browser's saved passwords and
        // cookies. A locally built binary is not in that item's ACL, so macOS
        // raises a consent dialog; headless runs got `errSecInteractionNotAllowed`
        // and logged "Encryption is not available", but an interactive one blocks
        // on a modal, which would stall an overnight run and perturb what it did
        // measure. The mock keychain is what ChromeDriver uses for the same
        // reason, and nothing here stores a secret worth encrypting.
        "--use-mock-keychain",
        "--no-first-run",
        "--no-default-browser-check",
        `--user-data-dir=${profileDir}`,
        pageUrl,
      ],
      { env: { ...process.env, MERSEY_CONSOLE_STDOUT: "1" }, detached: true },
    );
    let out = "";
    let result = null;
    let settled = false;
    const finish = async () => {
      if (settled) return;
      settled = true;
      const rss = await forkPss(FORK);
      try { process.kill(-child.pid, "SIGKILL"); } catch {}
      resolve({ result, rss });
    };
    const scan = (b) => {
      out += b.toString();
      // The RESULT line, wherever it lands (stdout echo or Blink console on stderr).
      // Blink's console wraps the message in quotes ("RESULT …", source: …),
      // so stop the checksum at the first space, quote, or comma.
      const m = /RESULT (\S+) ([\d.]+) ([^\s",]+)/.exec(out);
      if (m && !result) result = { ms: Number(m[2]), checksum: parseChecksum(m[3]) };
    };
    child.stdout.on("data", scan);
    child.stderr.on("data", scan);
    child.on("error", (e) => { console.error("spawn error", e.message); finish(); });
    child.on("exit", () => { if (!settled) finish(); });
    setTimeout(finish, 8000);
  });
}

// The Chromium fork aborts at startup at a low but nonzero rate on macOS
// (a component-build ANGLE duplicate-class hazard — main -> FatalError ->
// SIGABRT). A crashed launch yields no RESULT, so retry with a fresh profile
// rather than let one abort null out a whole workload column.
async function runPageWithRetry(pageUrl, profileBase, tag, expectResult = true) {
  const MAX = 5;
  for (let attempt = 0; attempt < MAX; attempt++) {
    const prof = join(profileBase, `${tag}-a${attempt}`);
    await mkdir(prof, { recursive: true });
    const r = await runPage(pageUrl, prof, expectResult);
    if (!expectResult || r.result) {
      return r;
    }
    if (attempt < MAX - 1) {
      console.log(`  chromium-native ${tag} — no result (attempt ${attempt + 1}/${MAX}), retrying`);
    }
  }
  return { result: null, rss: 0 };
}

const rows = [];
const profileBase = await mkdtemp(join(tmpdir(), "mersey-cr-"));

// Serve the generated pages over http (not file://) so same-origin requests —
// the fetch workload's /bench/echo — work; the server is rooted at the repo.
const { server, port } = await startServer();
const base = `http://localhost:${port}/bench/web/pages/native`;

let baseRss = 0;
{
  const { rss } = await runPageWithRetry(`${base}/blank.html`, profileBase, "blank", false);
  baseRss = rss ?? 0;
  console.log(`chromium-native  baseline blank rss ${baseRss} KiB\n`);
}

for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const { result, rss } = await runPageWithRetry(`${base}/${wl}.html`, profileBase, `${wl}-${r}`);
    if (result) samples.push({ ...result, rss: (rss ?? 0) - baseRss });
  }
  if (samples.length === 0) {
    console.log(`  chromium-native ${wl.padEnd(8)} — no result`);
    rows.push({ browser: "chromium-fork", impl: "native", wl, ms: null });
    continue;
  }
  samples.sort((a, b) => a.ms - b.ms);
  const med = samples[Math.floor(samples.length / 2)];
  const medRss = medianRss(samples);
  console.log(
    `  chromium-native ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(medRss).padStart(6)} KiB   (n=${samples.length})`,
  );
  rows.push({ browser: "chromium-fork", impl: "native", wl, ms: med.ms, rss: medRss, checksum: med.checksum });
}

killForks();
server.close();
// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl) pairs this run produced.

const merged = await mergeRows(here, "results.native.chromium.json", tagRows(rows));
await writeFile(join(here, "results.native.chromium.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.native.chromium.json`);
