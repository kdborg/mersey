// Best-effort PSS (memory) capture for the native-Ladybird leg.
//
// The time numbers come from run-native-ladybird.mjs, which drives `test-web`
// synchronously and so can't watch the process while it runs. Ladybird has no
// persistent `--headless URL` browser to launch-and-sample the way the Firefox,
// Chromium and Servo memory harnesses do; test-web spawns short-lived WebContent
// (+ RequestServer/ImageDecoder/…) processes per test and tears them down.
//
// So this script measures PSS a different way: it spawns test-web ASYNC and, while
// the workload runs, polls every few ms — summing Pss across every process whose
// /proc/PID/exe lives in the Ladybird build tree (shared pages counted once) — and
// keeps the PEAK. A blank page is measured the same way; the per-workload number
// is (peak workload − peak blank), i.e. the workload's own allocation on top of a
// browser that already has the engine compiled in. This is deliberately a best-
// effort, peak-sampled figure over a ~sub-second process life: it is NOT strictly
// comparable to the Firefox baseline (persistent process, settle-then-sample), and
// the report labels it as such. Same page set and completion trick as the time
// harness (inline text/mersey + include.js + test(()=>{})).
import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { existsSync, readFileSync, readdirSync, readlinkSync } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || `${process.env.HOME}/ladybird`;
const BUILD_BIN = process.env.LADYBIRD_BIN || `${LADYBIRD_SRC}/Build/release/bin`;
const TEST_WEB = process.env.TEST_WEB || `${BUILD_BIN}/test-web`;
const PYTHON = process.env.PYTHON || "python3";
const REPEATS = Number(process.env.MEM_REPEATS || 5);
const POLL_MS = Number(process.env.POLL_MS || 8);
const PER_TEST_TIMEOUT = 20;
// fetch excluded for the same reason as the time runner (file:// + async RESULT).
const WEB_WORKLOADS = ["canvas", "crypto", "cssom", "dom", "encoding", "events", "json", "query", "storage", "timers", "url"];
const WORKLOADS = process.env.WL ? process.env.WL.split(",") : WEB_WORKLOADS;

if (!existsSync(TEST_WEB)) {
  console.error(`test-web not found at ${TEST_WEB}\n  build the fork first (see ladybird/README.md), or set TEST_WEB=…`);
  process.exit(1);
}

const testRoot = join(LADYBIRD_SRC, "Tests", "LibWeb");
const pageDir = join(testRoot, "Text", "input", "mersey");
const resultsDir = join(here, "..", "..", "test-dumps", "ladybird-mem");
await mkdir(pageDir, { recursive: true });

// Generate the workload pages plus a blank baseline page (no mersey workload —
// just the completion scripts, so it measures the browser with the engine
// compiled in but doing no work).
for (const wl of WORKLOADS) {
  const src = await readFile(join(here, "mersey", `${wl}.mersey`), "utf8");
  await writeFile(join(pageDir, `${wl}.html`), `<!doctype html>
<meta charset="utf-8">
<title>native-ladybird-mem ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
<script src="../include.js"></script>
<script>test(() => {});</script>
</body>`);
}
await writeFile(join(pageDir, `blank.html`), `<!doctype html>
<meta charset="utf-8"><title>native-ladybird-mem blank</title>
<body><div id="out"></div>
<script src="../include.js"></script>
<script>test(() => {});</script>
</body>`);

// Sum Pss (KiB) over every live process whose executable is in the Ladybird build
// tree — WebContent, RequestServer, ImageDecoder, WebWorker, test-web itself. PSS
// counts a shared mapping once, so extra content processes don't double-count. The
// harness/process-startup cost is constant and cancels in the blank subtraction.
function forkPssNow() {
  let total = 0;
  let pids;
  try { pids = readdirSync("/proc"); } catch { return 0; }
  for (const pid of pids) {
    if (!/^\d+$/.test(pid)) continue;
    let exe;
    try { exe = readlinkSync(`/proc/${pid}/exe`); } catch { continue; }
    if (!exe.startsWith(BUILD_BIN)) continue;
    try {
      const roll = readFileSync(`/proc/${pid}/smaps_rollup`, "utf8");
      const m = /^Pss:\s+(\d+) kB/m.exec(roll);
      if (m) total += Number(m[1]);
    } catch { /* process may have exited mid-scan */ }
  }
  return total;
}

// Run one page once; poll PSS peak across its whole process life. Returns peak KiB.
function runOncePeak(wl) {
  return new Promise((resolve) => {
    let peak = 0;
    const child = spawn(TEST_WEB,
      ["--test-path", testRoot, "-f", `mersey/${wl}`, "-P", PYTHON,
        "-j1", "-t", String(PER_TEST_TIMEOUT), "-R", resultsDir],
      { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC }, stdio: "ignore" });
    const timer = setInterval(() => {
      const p = forkPssNow();
      if (p > peak) peak = p;
    }, POLL_MS);
    const done = () => { clearInterval(timer); resolve(peak); };
    child.on("exit", done);
    child.on("error", done);
  });
}

async function medianPeak(wl) {
  const peaks = [];
  for (let r = 0; r < REPEATS; r++) {
    const p = await runOncePeak(wl);
    if (p > 0) peaks.push(p);
  }
  if (peaks.length === 0) return null;
  peaks.sort((a, b) => a - b);
  return peaks[Math.floor(peaks.length / 2)];
}

await rm(resultsDir, { recursive: true, force: true });
console.log(`native-ladybird-mem  test-web=${TEST_WEB}\n  poll ${POLL_MS}ms, ${REPEATS} repeats\n`);

const basePeak = await medianPeak("blank");
console.log(`  baseline blank peak PSS ${basePeak} KiB\n`);

// Merge rss into the existing time results, matching by workload.
const resultsPath = join(here, "results.native.ladybird.json");
const rows = JSON.parse(await readFile(resultsPath, "utf8"));
const byWl = new Map(rows.map((r) => [r.wl, r]));

for (const wl of WORKLOADS) {
  const peak = await medianPeak(wl);
  if (peak == null) { console.log(`  ${wl.padEnd(8)} — no sample`); continue; }
  const rss = Math.max(0, peak - basePeak);
  console.log(`  ${wl.padEnd(8)} peak ${String(peak).padStart(7)} KiB   rss ${String(rss).padStart(7)} KiB  (${(rss / 1024).toFixed(1)} MiB)`);
  const row = byWl.get(wl);
  if (row) row.rss = rss;
}

await writeFile(resultsPath, JSON.stringify(rows, null, 2));
console.log(`\nupdated rss in ${resultsPath}`);
