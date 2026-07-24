// Best-effort memory capture for the native-Ladybird leg.
//
// The time numbers come from run-native-ladybird.mjs, which drives `test-web`
// synchronously and so can't watch the process while it runs. Ladybird has no
// persistent `--headless URL` browser to launch-and-sample the way the Firefox,
// Chromium and Servo memory harnesses do; test-web spawns short-lived WebContent
// (+ RequestServer/ImageDecoder/…) processes per test and tears them down.
//
// So this script measures memory a different way: it spawns test-web ASYNC and,
// while the workload runs, polls the memory of every process whose executable
// lives in the Ladybird build tree (shared pages counted once — Linux PSS or
// macOS de-duplicated footprint, see host-mem.mjs) and keeps the PEAK. The poll
// rate is the host's to choose: single-digit ms on Linux, ~10x coarser on macOS
// where each sample costs a `footprint` call, so the macOS peak is the weaker
// figure of the two. A blank page is measured the same way; the per-workload number
// is (peak workload − peak blank), i.e. the workload's own allocation on top of a
// browser that already has the engine compiled in. This is deliberately a best-
// effort, peak-sampled figure over a ~sub-second process life: it is NOT strictly
// comparable to the Firefox baseline (persistent process, settle-then-sample), and
// the report labels it as such. Same page set and completion trick as the time
// harness (inline text/mersey + include.js + test(()=>{})).
import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { existsSync, readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || join(here, "../../../browsers/ladybird");
const BUILD_BIN = process.env.LADYBIRD_BIN || `${LADYBIRD_SRC}/Build/release/bin`;
const TEST_WEB = process.env.TEST_WEB || `${BUILD_BIN}/test-web`;
const PYTHON = process.env.PYTHON || "python3";
const REPEATS = Number(process.env.MEM_REPEATS || 5);
// Linux poll cadence. /proc reads are cheap enough to sample at single-digit ms;
// macOS cannot (each footprint call is ~100ms), so on that host the sampler
// picks its own slower rate and this value is ignored unless set explicitly.
const POLL_MS = process.env.POLL_MS ? Number(process.env.POLL_MS) : undefined;
const PER_TEST_TIMEOUT = 20;
// fetch excluded for the same reason as the time runner (file:// + async RESULT).
const WEB_WORKLOADS = ["bchannel", "blob", "canvas", "compression", "compute", "crypto", "cssom", "dom", "encoding", "events", "fetch", "geometry", "idb", "json", "locks", "msgchannel", "query", "sse", "storage", "streams", "timers", "url", "urlpattern", "websocket", "xhr"];
// Async workloads self-report from a later task: their test must stay open
// long enough for the RESULT (and the workload's allocations) to happen.
const ASYNC_WORKLOADS = new Set([
  "bchannel", "compression", "fetch", "idb", "locks", "msgchannel", "sse",
  "streams", "websocket", "xhr",
]);
const WORKLOADS = process.env.WL ? process.env.WL.split(",") : WEB_WORKLOADS;

if (!existsSync(TEST_WEB)) {
  console.error(`test-web not found at ${TEST_WEB}\n  build the fork first (see ladybird/README.md), or set TEST_WEB=…`);
  process.exit(1);
}

const testRoot = join(LADYBIRD_SRC, "Tests", "LibWeb");

// fetch reaches the runner's echo server by absolute URL (see the time
// runner); the pages regenerated here need the same rewrite.
import { startServer } from "./server.mjs";
import { createPeakSampler, MEM_METRIC, PLATFORM } from "./host-mem.mjs";
import { rowPlatform } from "./rows.mjs";
const { server: echoServer, port: echoPort } = await startServer();
const absEcho = (text) => text.replaceAll("/bench/echo", `http://127.0.0.1:${echoPort}/bench/echo`);
const pageDir = join(testRoot, "Text", "input", "mersey");
const resultsDir = join(here, "..", "..", "test-dumps", "ladybird-mem");
await mkdir(pageDir, { recursive: true });

// Generate the workload pages plus a blank baseline page (no mersey workload —
// just the completion scripts, so it measures the browser with the engine
// compiled in but doing no work).
for (const wl of WORKLOADS) {
  const src = absEcho(await readFile(join(here, "mersey", `${wl}.mersey`), "utf8"));
  await writeFile(join(pageDir, `${wl}.html`), `<!doctype html>
<meta charset="utf-8">
<title>native-ladybird-mem ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
<script src="../include.js"></script>
<script>${ASYNC_WORKLOADS.has(wl)
    ? "asyncTest((done) => setTimeout(done, 8000));"
    : "test(() => {});"}</script>
</body>`);
}
await writeFile(join(pageDir, `blank.html`), `<!doctype html>
<meta charset="utf-8"><title>native-ladybird-mem blank</title>
<body><div id="out"></div>
<script src="../include.js"></script>
<script>test(() => {});</script>
</body>`);


// Run one page once; poll the tree's memory peak across its whole process life.
// Returns peak KiB. The sampler picks the host's metric and a poll rate it can
// sustain (see host-mem.mjs): Linux reads /proc every few ms, macOS drives
// `footprint`, which costs ~100ms a call and so samples far more coarsely.
function runOncePeak(wl) {
  return new Promise((resolve) => {
    const sampler = createPeakSampler(BUILD_BIN, { intervalMs: POLL_MS });
    sampler.reset();
    const child = spawn(TEST_WEB,
      ["--test-path", testRoot, "-f", `mersey/${wl}`, "-P", PYTHON,
        "-j1", "-t", String(PER_TEST_TIMEOUT), "-R", resultsDir],
      { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC }, stdio: "ignore" });
    sampler.start();
    const done = () => { sampler.stop(); resolve(sampler.peakKiB); };
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
console.log(`native-ladybird-mem  test-web=${TEST_WEB}\n  ${PLATFORM}/${MEM_METRIC}, poll ${POLL_MS ?? "auto"}ms, ${REPEATS} repeats\n`);

const basePeak = await medianPeak("blank");
console.log(`  baseline blank peak ${MEM_METRIC} ${basePeak} KiB\n`);

// Merge rss into the existing time results, matching by workload — but only
// against rows measured on THIS platform. The file holds a row per (workload,
// platform), so matching on workload alone would write these numbers into the
// other host's rows.
const resultsPath = join(here, "results.native.ladybird.json");
const rows = JSON.parse(await readFile(resultsPath, "utf8"));
const byWl = new Map(rows.filter((r) => rowPlatform(r) === PLATFORM).map((r) => [r.wl, r]));

for (const wl of WORKLOADS) {
  const peak = await medianPeak(wl);
  if (peak == null) { console.log(`  ${wl.padEnd(8)} — no sample`); continue; }
  const rss = Math.max(0, peak - basePeak);
  console.log(`  ${wl.padEnd(8)} peak ${String(peak).padStart(7)} KiB   rss ${String(rss).padStart(7)} KiB  (${(rss / 1024).toFixed(1)} MiB)`);
  const row = byWl.get(wl);
  if (row) { row.rss = rss; row.mem_metric = MEM_METRIC; }
}

await writeFile(resultsPath, JSON.stringify(rows, null, 2));
echoServer.close();
console.log(`\nupdated rss in ${resultsPath}`);
