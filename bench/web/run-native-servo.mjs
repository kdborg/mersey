// Native-Servo leg of the web-platform benchmark: the same Mersey workloads run
// by the engine hosted INSIDE the Servo fork (components/script/mersey), reaching
// web APIs through the reflective bridge in Rust→SpiderMonkey. The Servo
// counterpart of run-native.mjs (Firefox fork) and run-native-chromium.mjs.
//
// The fork runs inline <script type="text/mersey"> only, so each workload is
// inlined into a generated page. console.log is printed to stdout by Servo's
// headless window (and by the engine's host `print` hook), so the RESULT line is
// captured the same way as the stock Servo run.
//
// The fork is built WITH the Cranelift JIT (its crates are vendored into Servo's
// vendor/ — see servo/vendor-deps.sh), so the engine runs the Tier-1 JIT: the
// compute kernel is a real JIT number, and the JIT also compiles the web loops
// once they warm.
//
// Pages are served over http (not file://): Servo exposes localStorage only on
// an http origin, so `storage` needs it — and it matches the stock runs, which
// are http too. The bridge/engine are origin-independent, so the other web
// workloads are unaffected.
import { readFile, writeFile, readdir, mkdir } from "node:fs/promises";
import { spawn, execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { startServer } from "./server.mjs";
import { treeMemoryByCmdline } from "./host-mem.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const SERVO = process.env.SERVO_BIN ||
  join(here, "../../../servo-src/target/release/servoshell");
const REPEATS = 3;
// idb + locks excluded: Servo implements neither IndexedDB nor Web Locks
// (the stock legs prove both absences).
const WEB_WORKLOADS = ["bchannel", "blob", "canvas", "compression", "crypto", "cssom", "dom", "encoding", "events", "fetch", "geometry", "json", "msgchannel", "query", "sse", "storage", "streams", "timers", "url", "urlpattern", "websocket", "worker", "xhr"];
const WORKLOADS = process.env.WL ? process.env.WL.split(",") : [...WEB_WORKLOADS, "compute"];

process.on("unhandledRejection", (e) => { console.error("UNHANDLED", e); killServo(); process.exit(3); });
process.on("exit", () => killServo());
process.on("SIGINT", () => { killServo(); process.exit(130); });
function killServo() {
  try { execSync(`pkill -9 -f "${SERVO}" 2>/dev/null`); } catch {}
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Generate one inlined text/mersey page per workload, plus a blank baseline.
const pageDir = join(here, "pages", "native-servo");
await mkdir(pageDir, { recursive: true });
for (const wl of WORKLOADS) {
  const src = await readFile(join(here, "mersey", `${wl}.mersey`), "utf8");
  const html = `<!doctype html>
<meta charset="utf-8">
<title>native-servo ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
</body>`;
  await writeFile(join(pageDir, `${wl}.html`), html);
}
await writeFile(join(pageDir, "blank.html"),
  `<!doctype html><meta charset="utf-8"><body><div id="out"></div></body>`);

const servoPss = (match) => treeMemoryByCmdline(match);

function runPage(url, expectResult = true) {
  killServo();
  return new Promise(async (resolve) => {
    await sleep(500);
    const child = spawn(SERVO, ["-z", url],
      { env: { ...process.env }, detached: true, stdio: ["ignore", "pipe", "pipe"] });
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
      const m = /RESULT (\S+) ([\d.]+) (-?\d+)/.exec(out);
      if (m && !result) result = { ms: Number(m[2]), checksum: Number(m[3]) };
      if (result && expectResult) finish();
    });
    child.on("error", (e) => { console.error("spawn error", e.message); finish(); });
    child.on("exit", () => { if (!settled) finish(); });
    setTimeout(finish, expectResult ? 40000 : 6000);
  });
}

const { server, port } = await startServer();
const base = `http://localhost:${port}/bench/web/pages/native-servo`;
console.log(`native-servo  servo=${SERVO}  origin=${base}\n  workloads: ${WORKLOADS.join(", ")}\n`);

let baseRss = 0;
{
  const { rss } = await runPage(`${base}/blank.html`, false);
  baseRss = rss ?? 0;
  console.log(`native-servo  baseline blank rss ${baseRss} KiB\n`);
}

const rows = [];
for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const { result, rss } = await runPage(`${base}/${wl}.html`);
    if (result) samples.push({ ...result, rss: (rss ?? 0) - baseRss });
  }
  if (samples.length === 0) {
    console.log(`  native-servo ${wl.padEnd(8)} — no result`);
    rows.push({ browser: "servo-fork", impl: "native", wl, ms: null });
    continue;
  }
  samples.sort((a, b) => a.ms - b.ms);
  const med = samples[Math.floor(samples.length / 2)];
  console.log(
    `  native-servo ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(med.rss).padStart(6)} KiB   (n=${samples.length})`,
  );
  rows.push({ browser: "servo-fork", impl: "native", wl, ms: med.ms, rss: med.rss, checksum: med.checksum });
}

server.close();
killServo();
// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl) pairs this run produced.

const merged = await mergeRows(here, "results.native.servo.json", tagRows(rows));
await writeFile(join(here, "results.native.servo.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.native.servo.json`);
