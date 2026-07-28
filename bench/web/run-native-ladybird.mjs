// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

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
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || join(here, "../../../browsers/ladybird");
const TEST_WEB = process.env.TEST_WEB ||
  `${LADYBIRD_SRC}/Build/release/bin/test-web`;
const PYTHON = process.env.PYTHON || "python3";
const REPEATS = Number(process.env.REPEATS ?? 3);
const PER_TEST_TIMEOUT = 20; // seconds; a fallback — tests complete in well under 1s
// worker excluded: file:// pages can't load a cross-origin (absolute-http)
// worker script. Everything else runs — the C++ glue re-enters the engine
// from both promise reactions and event tasks; what once read as a re-entry
// gap was the page template ending the test before an async RESULT could
// land (see ASYNC_WORKLOADS below).
const WEB_WORKLOADS = ["bchannel", "blob", "canvas", "compression", "crypto", "cssom", "dom", "encoding", "events", "fetch", "frameworkui", "frameworkui2", "geometry", "idb", "json", "locks", "msgchannel", "query", "sse", "storage", "streams", "timers", "url", "urlpattern", "websocket", "xhr"];
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

// fetch: pages are file:// but RequestServer reaches absolute http URLs, and
// the echo endpoint admits opaque origins via CORS (see server.mjs).
import { startServer } from "./server.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

// A workload's checksum is not always a number: `fcompute` and `mathk` self-check
// with a boolean, because float bit parity across two independent codegens is not
// guaranteed. Parsing with `Number()` turned those into NaN -> `null` in the
// results file, which quietly threw away the correctness proof for two of the
// eight compute workloads on every browser leg (and made every parity check
// compare null to null and pass). Keep numbers as numbers so the file shape does
// not change; keep anything else as the token the workload printed.
const parseChecksum = (raw) => (/^-?\d+$/.test(raw) ? Number(raw) : raw);
const { server: echoServer, port: echoPort } = await startServer();
const absEcho = (text) => text
  .replaceAll("/bench/echo", `http://127.0.0.1:${echoPort}/bench/echo`)
  .replaceAll("/bench/sse", `http://127.0.0.1:${echoPort}/bench/sse`)
  // websocket derives its URL from location.host — empty on file:// pages.
  .replaceAll("ws://${location.host}", `ws://127.0.0.1:${echoPort}`);
const pageDir = join(testRoot, "Text", "input", "mersey");
const resultsDir = process.env.LB_RESULTS_DIR || join(here, "..", "..", "test-dumps", "ladybird");
await mkdir(pageDir, { recursive: true });

// Generate one inline text/mersey Text test per workload. Sync workloads
// emit RESULT during parse, so test(() => {}) can end the test at once;
// async workloads self-report from a later callback/event task, so their
// test must stay open long enough for the RESULT to land — otherwise the
// leg falsely reads as "no result" (that masquerade cost this leg seven
// workloads before the list below existed).
const ASYNC_WORKLOADS = new Set([
  "bchannel", "compression", "fetch", "idb", "locks", "msgchannel", "sse",
  "streams", "websocket", "worker", "xhr",
]);
for (const wl of WORKLOADS) {
  const src = absEcho(await readFile(join(here, "mersey", `${wl}.mersey`), "utf8"));
  const html = `<!doctype html>
<meta charset="utf-8">
<title>native-ladybird ${wl}</title>
<body><div id="out"></div>
<script type="text/mersey">
${src}
</script>
<script src="../include.js"></script>
<script>${ASYNC_WORKLOADS.has(wl)
    ? "asyncTest((done) => setTimeout(done, 8000));"
    : "test(() => {});"}</script>
</body>`;
  await writeFile(join(pageDir, `${wl}.html`), html);
}

// Run one workload once; return { ms, checksum } parsed from the per-test log.
async function runOnce(wl) {
  // --verbose echoes each test's captured output (the engine's stdout
  // RESULT among it) onto test-web's own stdout — read it there, with the
  // per-test logs.html as fallback. Async workloads (fetch) only reliably
  // surface on the echo path. Async spawn: the sync variant was observed to
  // lose the echo (buffering interacts with the harness's capture teardown).
  const { execFile } = await import("node:child_process");
  const text = await new Promise((resolve) => {
    execFile(TEST_WEB,
      ["--test-path", testRoot, "-f", `mersey/${wl}`, "-P", PYTHON,
        "-j1", "-t", String(PER_TEST_TIMEOUT), "-R", resultsDir, "--verbose"],
      { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC },
        timeout: 120000, maxBuffer: 16 * 1024 * 1024 },
      (err, stdout, stderr) => resolve(String(stdout ?? "") + String(stderr ?? "")));
  });
  if (process.env.DEBUG_LB) console.error("captured", text.length, "bytes; tail:", JSON.stringify(text.slice(-300)));
  let m = /RESULT (\S+) ([\d.]+) ([^\s",]+)/.exec(text);
  if (!m) {
    try {
      const log = readFileSync(
        join(resultsDir, "Text", "input", "mersey", `${wl}.html.logs.html`), "utf8");
      m = /RESULT (\S+) ([\d.]+) ([^\s",]+)/.exec(log);
    } catch { /* no log either */ }
  }
  return m ? { ms: Number(m[2]), checksum: parseChecksum(m[3]) } : null;
}

await rm(resultsDir, { recursive: true, force: true });
console.log(`native-ladybird  test-web=${TEST_WEB}\n  workloads: ${WORKLOADS.join(", ")}\n`);

const rows = [];
for (const wl of WORKLOADS) {
  const samples = [];
  for (let r = 0; r < REPEATS; r++) {
    const res = await runOnce(wl);
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
  if (exp != null && String(med.checksum) !== String(exp)) {
    console.error(`  CHECKSUM MISMATCH ${wl}: got ${med.checksum}, expected ${exp}`);
    process.exit(2);
  }
  const ok = exp == null ? "" : "  ✓checksum";
  console.log(`  native-ladybird ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(10)} ms   (n=${samples.length})${ok}`);
  rows.push({ browser: "ladybird-fork", impl: "native", wl, ms: med.ms, checksum: med.checksum });
}

// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl) pairs this run produced.

const merged = await mergeRows(here, "results.native.ladybird.json", tagRows(rows));
echoServer.close();
await writeFile(join(here, "results.native.ladybird.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.native.ladybird.json`);
