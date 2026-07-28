// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// End-to-end test of the Chromium fork's interactive Mersey debugger over the
// real Chrome DevTools Protocol: attach, enable the Mersey debugger, set a
// breakpoint, run a Mersey console turn (Mersey.evaluate), receive the
// Mersey.paused event with the pause snapshot, and resume. Proves the full
// pause/resume round-trip in a built fork — the engine half is msy_context_debug_*,
// the wire half is the custom `Mersey` CDP domain (inspector_mersey_agent).
//
// Pausing IS blocking: Mersey.evaluate does not return until a resume-family
// command arrives, so the paused handler must resume for evaluate to complete.
//
// NOTE: launch the branded "Mersey Blink (Experimental).app" binary (as the
// bench runner does), NOT the out/*/chrome symlink or the stock Chromium.app:
// the symlink misresolves `../Frameworks`, and Chromium.app's framework carries
// a stale pre-reorg @rpath (…/mersey/chromium/… without browsers/) so its engine
// dylib fails to load. Only the Mersey Blink app has the correct rpath.
//
//   CHROMIUM_SRC=~/Work/mersey/browsers/chromium/src node chromium/test-mersey-debugger.mjs
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const WebSocket = require(join(here, "../web/node_modules/ws"));

const CHROMIUM_SRC = process.env.CHROMIUM_SRC || join(here, "../../browsers/chromium/src");
const APP = "Mersey Blink (Experimental)";
const FORK = process.env.FORK ||
  `${CHROMIUM_SRC}/out/mersey-arm64/${APP}.app/Contents/MacOS/${APP}`;
const PORT = Number(process.env.PORT || 9333);

const child = spawn(FORK, [
  "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
  "--disable-background-networking", "--disable-component-update", "--disable-sync",
  `--remote-debugging-port=${PORT}`, `--user-data-dir=/tmp/mersey-cdp-${PORT}`,
  "data:text/html,<!doctype html><meta charset=utf-8><h1>mersey</h1>",
], { stdio: ["ignore", "pipe", "pipe"] });
child.stdout.on("data", () => {});
child.stderr.on("data", () => {});

function finish(ok, msg) {
  process.stdout.write((ok ? `PASS  ${msg}` : `FAIL  ${msg}`) + "\n");
  try { child.kill("SIGKILL"); } catch {}
  setTimeout(() => process.exit(ok ? 0 : 1), 200);
}

// Poll the CDP HTTP endpoint until the page target's WebSocket URL is up.
async function pageWsUrl(deadline) {
  for (;;) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
      const targets = await res.json();
      const page = targets.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      if (page) return page.webSocketDebuggerUrl;
    } catch { /* not up yet */ }
    if (Date.now() > deadline) throw new Error("CDP endpoint never came up");
    await new Promise((r) => setTimeout(r, 400));
  }
}

try {
  await new Promise((r) => setTimeout(r, 1500));
  const wsUrl = await pageWsUrl(Date.now() + 25000);
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.once("open", r); ws.once("error", j); });

  let nextId = 1;
  const pendingCmd = new Map();
  const eventHandlers = new Map();
  ws.on("message", (data) => {
    const msg = JSON.parse(data.toString());
    if (msg.id && pendingCmd.has(msg.id)) {
      const { resolve } = pendingCmd.get(msg.id);
      pendingCmd.delete(msg.id);
      resolve(msg.result);
    } else if (msg.method && eventHandlers.has(msg.method)) {
      eventHandlers.get(msg.method)(msg.params);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      const id = nextId++;
      pendingCmd.set(id, { resolve });
      ws.send(JSON.stringify({ id, method, params }));
    });

  const log = (...a) => { if (process.env.DEBUG) process.stderr.write(a.join(" ") + "\n"); };
  const watchdog = setTimeout(
    () => finish(false, `timeout — paused=${paused ? paused.reason : "null"}`), 30000);
  watchdog.unref?.();

  let paused = null;
  const evals = [];
  eventHandlers.set("Mersey.paused", async (p) => {
    log("<< Mersey.paused", p.reason);
    if (!paused) paused = p; // keep the first pause for the assertion
    // Evaluate-in-frame against the paused frame (0 = innermost) BEFORE
    // resuming — the engine is blocked in its nested loop, so the callout
    // window is live. `aa` resolves only once its `let` has run.
    evals.push(await send("Mersey.evaluateOnCallFrame", { callFrameIndex: 0, expression: "aa" }));
    // Pausing is blocking, and a multi-line turn hits a breakpoint per line —
    // resume EVERY pause so Mersey.evaluate can run to completion.
    await send("Mersey.resume").then(() => log("resume ack"));
  });
  eventHandlers.set("Mersey.resumed", () => log("<< Mersey.resumed"));

  log(">> enableDebugger"); await send("Mersey.enableDebugger");
  // An empty url matches every module — robust regardless of the REPL module name.
  log(">> setBreakpoints"); await send("Mersey.setBreakpoints", { url: "", lines: [1, 2, 3] });
  // This blocks in the engine's nested loop when the breakpoint fires; the
  // paused handler above resumes it. Awaits the completed turn.
  log(">> evaluate");
  await send("Mersey.evaluate", { expression: "let aa = 10;\nlet bb = aa + 5;\nbb" });
  log("<< evaluate returned");
  clearTimeout(watchdog);

  const reason = paused && paused.reason;
  const frames = paused && paused.callFrames;
  console.log("paused reason:", reason, "frames:", frames ? frames.length : 0);
  // Once `let aa = 10` has run, evaluate-in-frame against the paused frame sees
  // it (result "10", not an error); on the earliest pause it is not yet bound.
  const sawAa = evals.some((e) => e && e.result === "10" && e.isError === false);
  console.log("evaluateOnCallFrame results:", JSON.stringify(evals));
  const ok = reason === "breakpoint" && sawAa;
  finish(
    ok,
    ok
      ? "interactive Mersey pause + resume + evaluate-in-frame over CDP"
      : `pause=${reason} sawAa=${sawAa}`,
  );
} catch (e) {
  finish(false, e.message);
}
