// Stock-Servo leg of the web-platform benchmark: the same workloads run in a
// headless Servo (SpiderMonkey JS engine, Servo's own web platform), driven the
// same way as the Chromium/Firefox stock runs in run.mjs — but Servo is not a
// Playwright target, so it is spawned directly and its console.log is read from
// stdout (Servo's headless window prints console messages to stdout).
//
// Implementations measured here (all in STOCK Servo, no fork):
//   - js   : the workload in plain JavaScript
//   - poly : the same workload in Mersey, engine compiled to WASM (the polyfill)
//   - tjs  : the same Mersey, transpiled to JS at load time
// The fourth implementation — "native", the engine hosted inside the Servo fork
// — is measured by run-native-servo.mjs. Numbers merge in report.mjs / the HTML.
//
// A workload/impl that Servo's platform cannot run (a missing web API, a WASM or
// module-loader gap) yields no RESULT line; it is recorded as null (charted as
// n/a), never guessed.
import { writeFile, readdir, readFile } from "node:fs/promises";
import { spawn, execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { startServer } from "./server.mjs";
import { tagRows } from "./rows.mjs";
import { treeMemoryByCmdline } from "./host-mem.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const SERVO = process.env.SERVO_BIN ||
  join(here, "../../../browsers/servo-src/target/release/servoshell");
const REPEATS = 3;
const IMPLS = ["js", "poly", "tjs"];

// Stock legs compare against plain JS, so measure only the workloads that have
// a hand-written js/ baseline (the 6 web workloads + the compute kernel).
// calls/fcompute/mathk are Mersey-only (native-JIT targets), measured for the
// fork in run-native-servo.mjs, not here.
const WORKLOADS = (await readdir(join(here, "js")))
  .filter((f) => f.endsWith(".js"))
  .map((f) => f.replace(/\.js$/, ""))
  .sort();

// Reap any stray Servo tree on every exit path (matches run-native.mjs).
process.on("unhandledRejection", (e) => { console.error("UNHANDLED", e); killServo(); process.exit(3); });
process.on("exit", () => killServo());
process.on("SIGINT", () => { killServo(); process.exit(130); });
function killServo() {
  try { execSync(`pkill -9 -f "${SERVO}" 2>/dev/null`); } catch {}
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Sum PSS (KiB) over every process whose cmdline names the Servo binary — the
// whole multiprocess tree, shared libraries counted once (see run-native.mjs).
const servoPss = (match) => treeMemoryByCmdline(match);

// Launch headless Servo on one URL, capture the RESULT line and PSS, then kill
// the tree. expectResult=false is the blank baseline (no RESULT, just settle).
function runPage(url, expectResult = true) {
  killServo();
  return new Promise(async (resolve) => {
    await sleep(500); // let the previous tree exit
    const child = spawn(
      SERVO,
      ["-z", url],
      { env: { ...process.env }, detached: true, stdio: ["ignore", "pipe", "pipe"] },
    );
    let out = "";
    let result = null;
    let settled = false;
    const finish = async () => {
      if (settled) return;
      settled = true;
      const rss = await servoPss(SERVO);
      try { process.kill(-child.pid, "SIGKILL"); } catch {}
      resolve({ result, rss });
    };
    child.stdout.on("data", (b) => {
      out += b.toString();
      const m = /RESULT (\S+) ([\d.]+) (-?\d+)(?: heap=(\d+))?/.exec(out);
      if (m && !result) result = { ms: Number(m[2]), checksum: Number(m[3]) };
      if (result && expectResult) finish();
    });
    child.on("error", (e) => { console.error("spawn error", e.message); finish(); });
    child.on("exit", () => { if (!settled) finish(); });
    // Settle point: workloads finish in well under a second; give slow ones room.
    setTimeout(finish, expectResult ? 30000 : 6000);
  });
}

const { server, port } = await startServer();
const origin = `http://localhost:${port}`;
console.log(`serving ${origin}, servo=${SERVO}\n  workloads: ${WORKLOADS.join(", ")}\n`);

// Baseline PSS from a blank page.
let baseRss = 0;
{
  const { rss } = await runPage(`${origin}/bench/web/pages/blank.html`, false);
  baseRss = rss ?? 0;
  console.log(`servo  baseline blank rss ${baseRss} KiB\n`);
}

const rows = [];
for (const wl of WORKLOADS) {
  for (const impl of IMPLS) {
    const samples = [];
    for (let r = 0; r < REPEATS; r++) {
      const { result, rss } = await runPage(`${origin}/bench/web/pages/${impl}.html?wl=${wl}`);
      if (result) samples.push({ ...result, rss: (rss ?? 0) - baseRss });
    }
    if (samples.length === 0) {
      console.log(`  servo ${impl.padEnd(4)} ${wl.padEnd(8)} — no result`);
      rows.push({ browser: "servo", impl, wl, ms: null });
      continue;
    }
    samples.sort((a, b) => a.ms - b.ms);
    const med = samples[Math.floor(samples.length / 2)];
    console.log(
      `  servo ${impl.padEnd(4)} ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(med.rss).padStart(6)} KiB   (n=${samples.length})`,
    );
    rows.push({ browser: "servo", impl, wl, ms: med.ms, rss: med.rss, checksum: med.checksum });
  }
}

server.close();
killServo();
await writeFile(join(here, "results.servo.json"), JSON.stringify(tagRows(rows), null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.servo.json`);
