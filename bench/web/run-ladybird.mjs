// Stock-Ladybird leg of the web-platform benchmark: js / transpiled-JS /
// WASM-polyfill in (effectively) unmodified Ladybird — the mersey LibWeb module
// in the fork build is dormant for these pages, which run plain JS and WASM.
//
// Ladybird ships no `--headless URL` binary; the headless driver is `test-web`,
// which loads Text tests from file:// and captures the page's `println()` text
// into `<page>.actual.txt`. Under file:// EVERY fetch and relative module
// import is refused (cors requires http), so each page is fully
// SELF-CONTAINED: the workload, the bridge, the bindings, the engine bootstrap
// and the 2.5 MB WASM engine (base64) are all inlined. Two facts make the
// polyfill legs possible at all (probed, not assumed):
//   - WebAssembly.instantiate(bytes) works — only fetch is blocked;
//   - import(blobURL) works — so the transpiled-JS backend runs as a real
//     ES module, exactly like in the other browsers.
// fetch (the workload) reaches the runner's echo server by absolute URL +
// CORS (file:// origins are opaque; the endpoint admits them).
//
// Time is self-reported (RESULT line -> actual.txt via a println'd console
// hook); memory is the peak PSS of the Ladybird process tree while the test
// runs, minus a blank-page baseline — the same approach as
// run-native-ladybird-mem.mjs (test-web tears processes down per test, so a
// settle-point sample is impossible; the peak is the honest proxy).
import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { readFileSync, readdirSync, readlinkSync, existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || join(process.env.HOME, "ladybird");
const TEST_WEB = process.env.TEST_WEB || join(LADYBIRD_SRC, "Build", "release", "bin", "test-web");
const BUILD_BIN = dirname(TEST_WEB);
const PYTHON = process.env.PYTHON || "python3";
const REPEATS = 3;
const POLL_MS = 40;
// The WASM engine runs on LibWasm, an interpreter — give poly real room.
const TIMEOUT_S = { js: 60, tjs: 300, poly: 900 };

// compute-on-WASM is the
// LibWasm interpreter interpreting an interpreter — excluded from poly/tjs.
const WEB_WORKLOADS = ["canvas", "crypto", "cssom", "dom", "encoding", "events", "fetch", "json", "query", "storage", "timers", "url"];
const PLAN = {
  js: [...WEB_WORKLOADS, "compute"],
  tjs: [...WEB_WORKLOADS],
  poly: [...WEB_WORKLOADS],
};
const IMPLS = process.env.IMPL ? process.env.IMPL.split(",") : ["js", "tjs", "poly"];
const WL = process.env.WL ? process.env.WL.split(",") : null;

if (!existsSync(TEST_WEB)) {
  console.error(`test-web not found at ${TEST_WEB} — set TEST_WEB=…`);
  process.exit(1);
}

// Reference checksums from the Servo fork results (the engine must match them).
const EXPECTED = Object.fromEntries(
  JSON.parse(await readFile(join(here, "results.native.servo.json"), "utf8"))
    .filter((r) => r.checksum != null).map((r) => [r.wl, r.checksum]));

// ---- build the self-contained assets ---------------------------------------

const guard = (label, s) => {
  if (s.includes("</script")) throw new Error(`${label} contains </script — cannot inline`);
  return s;
};
const stripModule = (s) => s.replace(/^import .*$/gm, "").replace(/^export /gm, "");

const bindings = stripModule(guard("bindings", await readFile(join(root, "web", "mersey-bindings.gen.js"), "utf8")));
const bridge = stripModule(guard("bridge", await readFile(join(root, "web", "mersey-bridge.js"), "utf8")));
let engineSrc = stripModule(guard("engine", await readFile(join(root, "web", "mersey-engine.js"), "utf8")));
{
  const before = engineSrc;
  engineSrc = engineSrc.replace(
    /let instance;\n  try \{\n    \(\{ instance \} = await WebAssembly\.instantiateStreaming[\s\S]*?\n  \}\n/,
    "let instance;\n  ({ instance } = await WebAssembly.instantiate(globalThis.__merseyWasmBytes, imports));\n",
  );
  if (engineSrc === before) throw new Error("engine instantiate seam not found — mersey-engine.js changed?");
}
const wasmB64 = (await readFile(join(root, "web", "mersey_wasm.wasm"))).toString("base64");

// The shared boot block: decode the engine bytes, tee console.log into the
// test's text output (that is where the RESULT line becomes actual.txt).
const BOOT = `
globalThis.__merseyWasmBytes = Uint8Array.from(atob(globalThis.__merseyWasmB64), (c) => c.charCodeAt(0));
globalThis.__merseyWasmB64 = "";
globalThis.__merseyAllow = new Set(["random"]);
const __origLog = console.log.bind(console);
console.log = (...a) => {
  __origLog(...a);
  if (String(a[0]).startsWith("RESULT ")) globalThis.__merseyResultSeen = true;
  try { println(a.join(" ")); } catch {}
};
const __origErr = console.error.bind(console);
console.error = (...a) => { __origErr(...a); try { println("CONSOLE-ERROR " + a.join(" ")); } catch {} };
`;

const testRoot = join(LADYBIRD_SRC, "Tests", "LibWeb");

// The fetch workload's echo endpoint: pages load from file:// (test-web has
// no http mode), but RequestServer reaches absolute http URLs and the echo
// endpoint admits opaque origins via CORS — so the runner serves it and the
// generated pages carry the absolute URL.
import { startServer } from "./server.mjs";
const { server: echoServer, port: echoPort } = await startServer();
const ECHO = `http://127.0.0.1:${echoPort}/bench/echo`;
const absEcho = (text) => text.replaceAll("/bench/echo", ECHO);
const pageDir = join(testRoot, "Text", "input", "mersey-stock");
const resultsDir = join(here, "..", "..", "test-dumps", "ladybird-stock");
await mkdir(pageDir, { recursive: true });

const page = (title, body) => `<!doctype html>
<meta charset="utf-8">
<title>${title}</title>
<body><div id="out"></div>
<script src="../include.js"></script>
${body}
</body>`;

for (const wl of PLAN.js) {
  const src = guard(`js/${wl}`, absEcho(await readFile(join(here, "js", `${wl}.js`), "utf8")));
  await writeFile(join(pageDir, `js-${wl}.html`), page(`lb-stock js ${wl}`, `<script>
${stripModule(src)}
asyncTest(async (done) => {
  try {
    if (typeof setup === "function") setup();
    await work(Math.min(1000, N)); // warm up
    const t0 = performance.now();
    const checksum = await work(N);
    const t1 = performance.now();
    println("RESULT " + name + " " + (t1 - t0) + " " + checksum);
  } catch (e) { println("ERROR " + (e && e.message)); }
  done();
});
</script>`));
}

for (const impl of ["tjs", "poly"]) {
  for (const wl of PLAN[impl]) {
    const src = guard(`mersey/${wl}`, absEcho(await readFile(join(here, "mersey", `${wl}.mersey`), "utf8")));
    const run = impl === "poly"
      ? `const status = engine.run(SOURCE);
    if (status !== 0) println("STATUS " + status);
    await new Promise((resolve) => {
      const t = setInterval(() => {
        if (globalThis.__merseyResultSeen) { clearInterval(t); resolve(); }
      }, 10);
      setTimeout(() => { clearInterval(t); resolve(); }, 240000);
    });`
      : `const js = engine.transpile(SOURCE);
    const url = URL.createObjectURL(new Blob([js], { type: "text/javascript" }));
    await import(url);
    // The module's $rt.main body is async (it awaits the WASM compute tier
    // when the program has one — timers' noop); import() resolves before it
    // runs, so wait for the RESULT line rather than tearing the page down.
    await new Promise((resolve) => {
      const t = setInterval(() => {
        if (globalThis.__merseyResultSeen) { clearInterval(t); resolve(); }
      }, 10);
      setTimeout(() => { clearInterval(t); resolve(); }, 240000);
    });`;
    await writeFile(join(pageDir, `${impl}-${wl}.html`), page(`lb-stock ${impl} ${wl}`, `<script>
${bindings}
</script>
<script>
${bridge}
</script>
<script>
${engineSrc}
</script>
<script>globalThis.__merseyWasmB64 = "${wasmB64}";</script>
<script>
const SOURCE = ${JSON.stringify(src)};
asyncTest(async (done) => {
  try {
    ${BOOT}
    const engine = await startEngine({ realm: globalThis });
    ${run}
  } catch (e) { println("ERROR " + (e && e.message)); }
  done();
});
</script>`));
  }
}
await writeFile(join(pageDir, "blank.html"), page("lb-stock blank", `<script>test(() => {});</script>`));

// ---- run --------------------------------------------------------------------

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
    } catch { /* raced exit */ }
  }
  return total;
}

// Run one page once: peak PSS polled over the process tree, RESULT parsed from
// the captured actual.txt after exit.
function runOnce(pageName, timeoutS) {
  return new Promise((resolve) => {
    let peak = 0;
    const child = spawn(TEST_WEB,
      ["--test-path", testRoot, "-f", `mersey-stock/${pageName}`, "-P", PYTHON,
        "-j1", "-t", String(timeoutS), "-R", resultsDir],
      { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC }, stdio: "ignore" });
    const timer = setInterval(() => {
      const p = forkPssNow();
      if (p > peak) peak = p;
    }, POLL_MS);
    const finish = () => {
      clearInterval(timer);
      let result = null;
      try {
        const text = readFileSync(
          join(resultsDir, "Text", "input", "mersey-stock", `${pageName}.html.actual.txt`), "utf8");
        const m = /RESULT (\S+) ([\d.]+) (-?\d+)/.exec(text);
        if (m) result = { ms: Number(m[2]), checksum: Number(m[3]) };
        else if (/ERROR|STATUS/.test(text)) console.error(`    [${pageName}] ${text.trim().split("\n").pop()}`);
      } catch { /* no output */ }
      resolve({ result, peak });
    };
    child.on("exit", finish);
    child.on("error", finish);
  });
}

await rm(resultsDir, { recursive: true, force: true });
console.log(`ladybird-stock  test-web=${TEST_WEB}\n  impls: ${IMPLS.join(", ")}\n`);

// Blank baseline: engine-free page, like the other browsers' blank tab.
let basePeak = 0;
{
  const peaks = [];
  for (let r = 0; r < REPEATS; r++) peaks.push((await runOnce("blank", 30)).peak);
  peaks.sort((a, b) => a - b);
  basePeak = peaks[Math.floor(peaks.length / 2)];
  console.log(`  baseline blank peak PSS ${basePeak} KiB\n`);
}

const rows = [];
for (const impl of IMPLS) {
  for (const wl of PLAN[impl]) {
    if (WL && !WL.includes(wl)) continue;
    const samples = [];
    for (let r = 0; r < REPEATS; r++) {
      const { result, peak } = await runOnce(`${impl}-${wl}`, TIMEOUT_S[impl]);
      if (result) samples.push({ ...result, rss: Math.max(0, peak - basePeak) });
    }
    if (samples.length === 0) {
      console.log(`  ladybird ${impl.padEnd(4)} ${wl.padEnd(8)} — no result`);
      rows.push({ browser: "ladybird", impl, wl, ms: null });
      continue;
    }
    samples.sort((a, b) => a.ms - b.ms);
    const med = samples[Math.floor(samples.length / 2)];
    const expect = EXPECTED[wl];
    const mark = expect == null || Number(expect) === med.checksum ? "✓checksum" : `✗checksum ${med.checksum} != ${expect}`;
    console.log(`  ladybird ${impl.padEnd(4)} ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(10)} ms   rss ${String(med.rss).padStart(7)} KiB  (n=${samples.length})  ${mark}`);
    rows.push({ browser: "ladybird", impl, wl, ms: med.ms, rss: med.rss, checksum: med.checksum });
  }
}

// Merge into any existing rows (a WL=/IMPL=-filtered run must not clobber the
// rest of the file), newest row wins per impl/wl.
let merged = rows;
try {
  const prior = JSON.parse(await readFile(join(here, "results.ladybird.json"), "utf8"));
  const seen = new Set(rows.map((r) => `${r.impl}/${r.wl}`));
  merged = [...prior.filter((r) => !seen.has(`${r.impl}/${r.wl}`)), ...rows];
} catch { /* first run */ }
echoServer.close();
await writeFile(join(here, "results.ladybird.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${merged.length} rows to bench/web/results.ladybird.json`);
