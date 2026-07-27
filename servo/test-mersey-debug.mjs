// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Runtime test: a Mersey breakpoint fires in the built servoshell and produces a
// DevTools-grade pause snapshot (frame, line, column, locals + globals). Drives
// the engine debug controller (msy_context_debug_*) wired into the Servo fork,
// via the page-global debug surface, from a stock headless servoshell.
//
//   SERVO_BIN=.../servoshell node servo/test-mersey-debug.mjs
import http from "node:http";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const SERVO = process.env.SERVO_BIN ||
  join(here, "../../browsers/servo/target/release/servoshell");

// A first mersey <script> installs the debug globals; JS arms breakpoints on the
// next inline module (engine source name "<host>"); that module runs and pauses;
// JS reads back the recorded snapshot (JS console.log reaches servoshell stdout).
const PAGE = `<!doctype html><meta charset="utf-8"><body><div id="out"></div>
<script type="text/mersey">console.log("init");</script>
<script>__merseyDebugSetBreakpoints("<host>", "1,2,3,4,5,6");</script>
<script type="text/mersey">
let a = 1;
let b = 2;
let c = a + b;
</script>
<script>console.log("MDBG_DLOG:[" + __merseyDebugLog() + "]");</script></body>`;

const server = http.createServer((_req, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end(PAGE);
});
await new Promise((r) => server.listen(0, r));
const port = server.address().port;

const child = spawn(SERVO, ["-z", `http://127.0.0.1:${port}/`],
  { stdio: ["ignore", "pipe", "pipe"] });
let out = "";
await new Promise((resolve) => {
  const t = setTimeout(resolve, 15000);
  const onData = (b) => { out += b; if (out.includes("MDBG_DLOG:")) { clearTimeout(t); resolve(); } };
  child.stdout.on("data", onData);
  child.stderr.on("data", (b) => { out += b; });
});
try { process.kill(child.pid, "SIGKILL"); } catch {}
server.close();

const m = /MDBG_DLOG:\[(.*)\]/.exec(out);
const log = m ? m[1] : "";
const ok = log.includes('"reason":"breakpoint"') && log.includes('"line":2');
console.log(ok
  ? "PASS  Mersey breakpoint fired in servoshell; pause snapshot captured"
  : "FAIL  no breakpoint snapshot captured");
console.log("snapshot: " + log.slice(0, 300));
process.exit(ok ? 0 : 1);
