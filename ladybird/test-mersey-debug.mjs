// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Runtime test: a Mersey breakpoint fires in the built Ladybird fork and produces
// a DevTools-grade pause snapshot (frame, line, column, locals + globals). Drives
// the engine debug controller (msy_context_debug_*) wired into MerseyScriptRunner,
// via the page-global debug surface, through Ladybird's headless `test-web`.
//
//   LADYBIRD_SRC=~/Work/mersey/browsers/ladybird node ladybird/test-mersey-debug.mjs
//
// A first mersey <script> installs the debug globals; JS arms breakpoints on the
// next inline module (engine source name "<host>"); that module runs and pauses;
// JS reads back the recorded snapshots with __merseyDebugLog(), which — unlike a
// mersey console.log — surfaces on the (js log) channel test-web captures.
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdir, writeFile, rm } from "node:fs/promises";

const here = dirname(fileURLToPath(import.meta.url));
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || join(here, "../../browsers/ladybird");
const TEST_WEB = process.env.TEST_WEB || `${LADYBIRD_SRC}/Build/release/bin/test-web`;
const PYTHON = process.env.PYTHON || "python3";
const testRoot = join(LADYBIRD_SRC, "Tests", "LibWeb");
const pageDir = join(testRoot, "Text", "input", "mersey");
const resultsDir = join(here, "..", "test-dumps", "ladybird-debug");

// The second module is pure `let` statements (no console — a bare, un-imported
// `console` would fail the strict typecheck and the module would never run).
const PAGE = `<!doctype html>
<meta charset="utf-8">
<title>mersey dbgtrace</title>
<body><div id="out"></div>
<script type="text/mersey">console.log("init");</script>
<script>__merseyDebugSetBreakpoints("<host>", "1,2,3,4,5,6");</script>
<script type="text/mersey">
let a = 1;
let b = 2;
let c = a + b;
</script>
<script src="../include.js"></script>
<script>console.log("MDBG_DLOG_START" + __merseyDebugLog() + "MDBG_DLOG_END"); test(() => {});</script>
</body>
`;

await mkdir(pageDir, { recursive: true });
await writeFile(join(pageDir, "dbgtrace.html"), PAGE);
await rm(resultsDir, { recursive: true, force: true });

const text = await new Promise((resolve) => {
  const child = spawn(TEST_WEB,
    ["--test-path", testRoot, "-f", "mersey/dbgtrace", "-P", PYTHON,
      "-j1", "-t", "20", "-R", resultsDir, "--verbose"],
    { env: { ...process.env, LADYBIRD_SOURCE_DIR: LADYBIRD_SRC },
      stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  const t = setTimeout(() => { try { child.kill("SIGKILL"); } catch {} }, 60000);
  child.stdout.on("data", (b) => { out += b; });
  child.stderr.on("data", (b) => { out += b; });
  child.on("close", () => { clearTimeout(t); resolve(out); });
});

const m = /MDBG_DLOG_START(.*)MDBG_DLOG_END/s.exec(text);
const log = m ? m[1] : "";
const ok = log.includes('"reason":"breakpoint"') && log.includes('"line":2')
  && log.includes('"name":"a","value":"1"');
console.log(ok
  ? "PASS  Mersey breakpoint fired in Ladybird; pause snapshot captured"
  : "FAIL  no breakpoint snapshot captured");
console.log("snapshot: " + log.slice(0, 300));
process.exit(ok ? 0 : 1);
