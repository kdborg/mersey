// Performance regression tests over the engine-only leg (engine-child.mjs):
// each technology runs on the wasm engine over the deterministic stub realm,
// and time / peak-RSS / wasm-heap are compared against the committed
// baselines in perf-baselines.json. Checksums must match exactly — a checksum
// change is a correctness regression, never a tolerance question.
//
//   node perf-test.mjs                 run all workloads, exit 1 on regression
//   node perf-test.mjs --update        rewrite perf-baselines.json from this run
//   PERF_WL=storage,json node perf-test.mjs      filter workloads
//   PERF_TIME_TOL=1.5 PERF_MEM_TOL=1.4          tolerance factors (defaults)
//
// Tolerances are deliberately generous — this suite is for catching real
// regressions (an accidental O(n²), a leaked handle table), not 5% noise.
// Time uses the MIN of the repeats (the least-disturbed run); memory floors
// absorb V8's GC timing variance in the child process.
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { runChild, blankBaseline } from "./run-engine.mjs";
import { startServer } from "./server.mjs";
import { PLATFORM } from "./host-mem.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const BASELINE_FILE = join(here, "perf-baselines.json");

const REPEATS = Number(process.env.REPEATS ?? 2);
const TIME_TOL = Number(process.env.PERF_TIME_TOL ?? 1.5);
const MEM_TOL = Number(process.env.PERF_MEM_TOL ?? 1.4);
const TIME_FLOOR_MS = 20; // below this, a diff is noise, not a regression
const MEM_FLOOR_KB = 8192;
const update = process.argv.includes("--update");

// Baselines are per host platform: {"linux": {wl: {...}}, "macos": {...}}.
// Time and memory are machine-specific, so a run only ever gates against
// numbers recorded on the same platform. A pre-platform (flat) file is read as
// Linux, which is where every committed baseline was measured.
let allBaselines = {};
try {
  const parsed = JSON.parse(await readFile(BASELINE_FILE, "utf8"));
  const nested = Object.values(parsed).every((v) => v && typeof v === "object" && !("ms" in v));
  allBaselines = nested ? parsed : { linux: parsed };
} catch {
  if (!update) {
    console.error("no perf-baselines.json — run `node perf-test.mjs --update` first");
    process.exit(2);
  }
}
const baselines = allBaselines[PLATFORM] ?? {};
if (!update && Object.keys(baselines).length === 0) {
  console.error(
    `no perf baselines for platform "${PLATFORM}" (have: ${Object.keys(allBaselines).join(", ") || "none"}).\n` +
    `Time and memory are machine-specific, so this host needs its own:\n` +
    `  node perf-test.mjs --update\n` +
    `Checksums are platform-independent and are still checked against any recorded platform.`);
  process.exit(2);
}

// A checksum is the same on every platform — it is the correctness proof. So a
// host with no timing baseline of its own still gets checksum gating, using
// whatever platform recorded one.
function baselineChecksum(wl) {
  for (const p of Object.keys(allBaselines)) {
    const c = allBaselines[p]?.[wl]?.checksum;
    if (c != null) return c;
  }
  return null;
}

const WORKLOADS = process.env.PERF_WL
  ? process.env.PERF_WL.split(",")
  : update
    ? [
        "bchannel", "blob", "calls", "canvas", "compression", "compute",
        "crypto", "cssom", "dom", "encoding", "events", "fcompute", "fetch",
        "geometry", "idb", "json", "locks", "mathk", "msgchannel", "query",
        "sse", "storage", "streams", "timers", "url", "urlpattern",
        "websocket", "worker", "xhr",
      ]
    : Object.keys(baselines);

const { server, port } = await startServer();
const env = { MERSEY_ECHO_BASE: `http://localhost:${port}` };

const baseRss = await blankBaseline();

let failures = 0;
const pass = (what) => console.log(`PASS  ${what}`);
const fail = (what) => {
  console.log(`FAIL  ${what}`);
  failures++;
};

const fresh = {};
for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const s = await runChild(wl, env);
    if (s.ms != null) samples.push(s);
  }
  if (samples.length === 0) {
    fail(`${wl}: no RESULT from the engine child`);
    continue;
  }
  const ms = Math.min(...samples.map((s) => s.ms));
  const rss = Math.max(0, Math.min(...samples.map((s) => s.vmhwm ?? 0)) - baseRss);
  const heap = Math.max(...samples.map((s) => s.wasmheap ?? 0));
  const checksum = samples[0].checksum;
  fresh[wl] = {
    ms: Number(ms.toFixed(2)),
    rss,
    heap,
    checksum: Number.isNaN(checksum) ? null : checksum,
  };

  if (update) continue;

  // Checksum first, and independently of the timing baseline: it is a
  // correctness check that holds on every platform.
  const expected = baselineChecksum(wl);
  if (expected != null && fresh[wl].checksum !== expected) {
    fail(`${wl}: CHECKSUM ${fresh[wl].checksum} != baseline ${expected} — correctness, not perf`);
    continue;
  }

  const b = baselines[wl];
  if (!b) continue;
  const tag = `${wl}: ${ms.toFixed(1)}ms (baseline ${b.ms}ms), rss ${(rss / 1024).toFixed(1)}MB (baseline ${(b.rss / 1024).toFixed(1)}MB)`;
  const timeBad = ms > b.ms * TIME_TOL && ms - b.ms > TIME_FLOOR_MS;
  const rssBad = rss > b.rss * MEM_TOL && rss - b.rss > MEM_FLOOR_KB;
  const heapBad = b.heap != null && heap > b.heap * MEM_TOL;
  if (timeBad) fail(`${tag} — time regression (> ${TIME_TOL}x)`);
  else if (rssBad) fail(`${tag} — peak-RSS regression (> ${MEM_TOL}x)`);
  else if (heapBad) fail(`${wl}: wasm heap ${heap} > baseline ${b.heap} × ${MEM_TOL} — engine heap regression`);
  else pass(tag);
}

server.close();

if (update) {
  // Rewrite only THIS platform's section; other platforms are left untouched.
  const merged = { ...baselines, ...fresh };
  const ordered = Object.fromEntries(Object.keys(merged).sort().map((k) => [k, merged[k]]));
  const out = { ...allBaselines, [PLATFORM]: ordered };
  const orderedPlatforms = Object.fromEntries(Object.keys(out).sort().map((k) => [k, out[k]]));
  await writeFile(BASELINE_FILE, JSON.stringify(orderedPlatforms, null, 2) + "\n");
  console.log(`\nwrote ${PLATFORM} baselines for ${Object.keys(fresh).length} workload(s) to bench/web/perf-baselines.json`);
  process.exit(0);
}

if (failures > 0) {
  console.error(`\n${failures} perf regression(s)`);
  process.exit(1);
}
console.log(`\nperf: all ${WORKLOADS.length} workloads within tolerance`);
