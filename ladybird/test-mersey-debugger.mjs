// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// End-to-end test of Ladybird's interactive Mersey debugger over the real Firefox
// remote-debugging protocol: attach, set a Mersey breakpoint, trigger a Mersey
// turn through the console, receive the `merseyPaused` event with the pause
// snapshot, and resume. Proves the full cross-process pause/resume round-trip in
// a built Ladybird (the engine half is `msy_context_debug_*`; the pause blocks
// WebContent on a nested Core::EventLoop pumping IPC until DevTools resumes).
//
//   LADYBIRD_SRC=~/Work/mersey/browsers/ladybird node ladybird/test-mersey-debugger.mjs
import net from "node:net";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const LADYBIRD_SRC = process.env.LADYBIRD_SRC || join(here, "../../browsers/ladybird");
const BIN = process.env.LADYBIRD_BIN ||
  `${LADYBIRD_SRC}/Build/release/bin/Ladybird.app/Contents/MacOS/Ladybird`;
const PORT = Number(process.env.PORT || 6099);

// A minimal page: arming (setBreakpoint) initializes the engine context on this
// document's realm, and the console Mersey turn runs against that same context.
const child = spawn(BIN,
  [`--devtools=${PORT}`, "data:text/html,<!doctype html><meta charset=utf-8><h1>mersey</h1>"],
  { stdio: ["ignore", "pipe", "pipe"] });
child.stdout.on("data", () => {});
child.stderr.on("data", () => {});

function finish(ok, msg) {
  // Write synchronously and delay the exit: console.log + immediate
  // process.exit() truncates on a pipe (e.g. over ssh).
  process.stdout.write((ok ? `PASS  ${msg}` : `FAIL  ${msg}`) + "\n");
  try { child.kill("SIGKILL"); } catch {}
  setTimeout(() => process.exit(ok ? 0 : 1), 200);
}

// The DevTools server takes a few seconds to bind; retry the connect until it's
// up (Ladybird's GUI + WebContent spin-up is ~6s cold).
function connectWithRetry(deadline) {
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const s = net.connect(PORT, "127.0.0.1");
      s.once("connect", () => resolve(s));
      s.once("error", () => {
        s.destroy();
        if (Date.now() > deadline) reject(new Error("could not connect to DevTools port"));
        else setTimeout(attempt, 500);
      });
    };
    attempt();
  });
}

await new Promise((r) => setTimeout(r, 3000));
const sock = await connectWithRetry(Date.now() + 20000);
let buf = Buffer.alloc(0);
const pending = [];
let onPacket = null;

function send(obj) {
  const body = Buffer.from(JSON.stringify(obj), "utf8");
  sock.write(Buffer.concat([Buffer.from(`${body.length}:`, "utf8"), body]));
}
sock.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const colon = buf.indexOf(0x3a);
    if (colon < 0) return;
    const len = Number(buf.subarray(0, colon).toString("utf8"));
    if (!Number.isFinite(len) || buf.length < colon + 1 + len) return;
    const pkt = JSON.parse(buf.subarray(colon + 1, colon + 1 + len).toString("utf8"));
    buf = buf.subarray(colon + 1 + len);
    pending.push(pkt);
    if (onPacket) onPacket(pkt);
  }
});
sock.on("error", (e) => finish(false, `socket error: ${e.message}`));

const waitFor = (pred, what, ms = 15000) =>
  new Promise((res, rej) => {
    const hit = pending.find(pred);
    if (hit) return res(hit);
    const t = setTimeout(() => rej(new Error(`timeout: ${what}`)), ms);
    onPacket = (p) => { if (pred(p)) { clearTimeout(t); onPacket = null; res(p); } };
  });

try {
  await waitFor((p) => p.from === "root", "root hello");
  send({ to: "root", type: "listTabs" });
  const tabs = await waitFor((p) => Array.isArray(p.tabs), "listTabs");
  const tab = tabs.tabs[0];
  send({ to: tab.actor, type: "getWatcher" });
  const watcher = await waitFor((p) => p.actor && p.traits, "getWatcher");
  // watchTargets is the attach that turns on the script->devtools event routing.
  send({ to: watcher.actor, type: "watchTargets", targetType: "frame" });
  const target = await waitFor((p) => p.type === "target-available-form", "frame target");
  const consoleActor = target.target.consoleActor;
  const dbgActor = target.target.merseyDebuggerActor;
  console.log("merseyDebugger actor:", dbgActor);
  if (!dbgActor) finish(false, "no merseyDebuggerActor advertised");

  // Arm a breakpoint on the REPL source, then run a Mersey console turn. Line 2/3
  // of the turn should hit the breakpoint and pause the engine.
  send({ to: dbgActor, type: "setBreakpoint", source: "<repl>", lines: [1, 2, 3] });
  await waitFor((p) => p.from === dbgActor, "setBreakpoint reply");
  send({ to: consoleActor, type: "evaluateJSAsync",
         text: "mersey> const q = 1;\nconst w = q + 1;\nw" });

  const paused = await waitFor((p) => p.type === "merseyPaused", "merseyPaused event");
  console.log("PAUSED snapshot:", (paused.snapshot || "").slice(0, 160));
  send({ to: dbgActor, type: "resume" });
  await new Promise((r) => setTimeout(r, 500));

  const ok = typeof paused.snapshot === "string" && paused.snapshot.includes('"reason":"breakpoint"');
  finish(ok, ok ? "interactive Mersey pause + resume over RDP" : "no breakpoint snapshot in merseyPaused");
} catch (e) {
  finish(false, e.message);
}
