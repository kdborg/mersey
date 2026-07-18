// Native-Ladybird leg of the web-platform benchmark: the same Mersey workloads
// run by the engine hosted INSIDE the Ladybird fork (Libraries/LibWeb/Mersey),
// reaching web APIs through the reflective bridge in C++→LibJS. The Ladybird
// counterpart of run-native.mjs (Firefox), run-native-chromium.mjs and
// run-native-servo.mjs.
//
// Ladybird has no `--headless URL` binary in current trees; its headless runner
// is `test-web` (a WPT-style harness). So each workload is written as an inline
// `<script type="text/mersey">` Text test under the Ladybird test tree, plus a
// trailing `include.js` + `test(() => {})` so the test completes immediately
// instead of hitting the per-test timeout. The engine's host `print` hook writes
// the RESULT line to the WebContent process's stdout, which test-web captures
// into a per-test `.logs.html` file — that is where we read RESULT from (the
// mersey script runs at HTML-parse time, so RESULT is emitted before the test
// completes).
//
// The fork links libmersey_capi.a built WITH the Cranelift JIT (default feature),
// so compute is a real Tier-1 JIT number and the JIT compiles the web loops once
// warm. Web workloads cross the reflective bridge (every fast-path pointer NULL),
// so they sit well above the direct-C++ forks — the same gap Servo shows, and the
// tracked next lever (typed LibWeb fast paths).
//
// Checksums are asserted to match the other forks' native results; a mismatch is
// a hard error (the engine must do identical work in every host).
import { readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || `${process.env.HOME}/ladybird`;
const TEST_WEB = process.env.TEST_WEB ||
  `${LADYBIRD_SRC}/Build/release/bin/test-web`;
const PYTHON = process.env.PYTHON || "python3";
const REPEATS = 3;
const PER_TEST_TIMEOUT = 20; // seconds; a fallback — tests complete in well under 1s
const WEB_WORKLOADS = ["canvas", "crypto", "dom", "json", "storage", "url"];
const WORKLOADS = process.env.WL ? process.env.WL.split(",") : [...WEB_WORKLOADS, "compute"];

// Reference checksums (the other native forks); the engine must match them.
const EXPECTED = Object.fromEntries(
  (JSON.parse(await readFile(join(here, "results.native.servo.json"), "utf8")))
    .filter((r) => r.checksum != null).map((r) => [r.wl, r.checksum]));

if (!existsSync(TEST_WEB)) {
  console.error(`test-web not found at ${TEST_WEB}\n  build the fork first (see ladybird/README.md), or set TEST_WEB=…`);
  process.exit(1);
}

// The Ladybird test tree: put the generated pages under Text/input/mersey so
// test-web's file:// loader and its hardcoded standard-tree paths both work.
const testRoot = join(LADYBIRD_SRC, "Tests", "LibWeb");
const pageDir = join(testRoot, "Text", "input", "mersey");
const resultsDir = join(here, "..", "..", "test-dumps", "ladybird");
await mkdir(pageDir, { recursive: true });

// Generate one inline text/mersey Text test per workload. The trailing
// include.js + test(() => {}) signals completion so the test ends at once.
for (const wl of WORKLOADS) {
  const src = await readFile(join(here, "mersey", `${wl}.mersey`), "utf8");
  const html = `<!doctype html>
<meta charset="utf-8">
<title>native-ladybird ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
<script src="../include.js"></script>
<script>test(() => {});</script>
</body>`;
  await writeFile(join(pageDir, `${wl}.html`), html);
}

// Run one workload once; return { ms, checksum } parsed from the per-test log.
function runOnce(wl) {
  try {
    execFileSync(TEST_WEB,
      ["--test-path", testRoot, "-f", `mersey/${wl}`, "-P", PYTHON,
        "-j1", "-t", String(PER_TEST_TIMEOUT), "-R", resultsDir],
      { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC }, stdio: "ignore", timeout: 120000 });
  } catch { /* test-web exits non-zero (no expectation file); we read the log */ }
  const log = join(resultsDir, "Text", "input", "mersey", `${wl}.html.logs.html`);
  let text = "";
  try { text = readFileSync(log, "utf8"); } catch { return null; }
  const m = /RESULT (\S+) ([\d.]+) (-?\d+)/.exec(text);
  return m ? { ms: Number(m[2]), checksum: Number(m[3]) } : null;
}

await rm(resultsDir, { recursive: true, force: true });
console.log(`native-ladybird  test-web=${TEST_WEB}\n  workloads: ${WORKLOADS.join(", ")}\n`);

const rows = [];
for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const res = runOnce(wl);
    if (res) samples.push(res);
  }
  if (samples.length === 0) {
    console.log(`  native-ladybird ${wl.padEnd(8)} — no result`);
    rows.push({ browser: "ladybird-fork", impl: "native", wl, ms: null });
    continue;
  }
  samples.sort((a, b) => a.ms - b.ms);
  const med = samples[Math.floor(samples.length / 2)];
  const exp = EXPECTED[wl];
  if (exp != null && med.checksum !== exp) {
    console.error(`  CHECKSUM MISMATCH ${wl}: got ${med.checksum}, expected ${exp}`);
    process.exit(2);
  }
  const ok = exp == null ? "" : "  ✓checksum";
  console.log(`  native-ladybird ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(10)} ms   (n=${samples.length})${ok}`);
  rows.push({ browser: "ladybird-fork", impl: "native", wl, ms: med.ms, checksum: med.checksum });
}

await writeFile(join(here, "results.native.ladybird.json"), JSON.stringify(rows, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.native.ladybird.json`);
