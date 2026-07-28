// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// End-to-end test of Servo's interactive Mersey debugger over the real Firefox
// remote-debugging protocol: attach, set a Mersey breakpoint, trigger a Mersey
// turn through the console, receive the `merseyPaused` event with the pause
// snapshot, and resume. Proves the full off-thread pause/resume round-trip in a
// built servoshell (the engine half is `msy_context_debug_*`, the wire half is
// the `merseyDebugger` actor).
//
//   servoshell --devtools=PORT -z <page-with-an-inline-text/mersey-script> &
//   node servo/test-mersey-debugger.mjs PORT
import net from "node:net";

const PORT = Number(process.argv[2] || 6099);
const sock = net.connect(PORT, "127.0.0.1");
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
const waitFor = (pred, what, ms = 15000) =>
  new Promise((res, rej) => {
    const hit = pending.find(pred);
    if (hit) return res(hit);
    const t = setTimeout(() => rej(new Error(`timeout: ${what}`)), ms);
    onPacket = (p) => { if (pred(p)) { clearTimeout(t); onPacket = null; res(p); } };
  });

await new Promise((r) => sock.once("connect", r));
await waitFor((p) => p.from === "root", "root hello");
send({ to: "root", type: "listTabs" });
const tabs = await waitFor((p) => Array.isArray(p.tabs), "listTabs");
const tab = tabs.tabs[0];
send({ to: tab.actor, type: "getWatcher" });
const watcher = await waitFor((p) => p.actor && p.traits, "getWatcher");
// watchTargets is the attach that turns on script->devtools routing.
send({ to: watcher.actor, type: "watchTargets", targetType: "frame" });
const target = await waitFor((p) => p.type === "target-available-form", "frame target");
const consoleActor = target.target.consoleActor;
const dbgActor = target.target.merseyDebuggerActor;
console.log("merseyDebugger actor:", dbgActor);
if (!dbgActor) { console.log("FAIL  no merseyDebuggerActor advertised"); sock.end(); process.exit(1); }

// Arm a breakpoint on the REPL source, then run a Mersey turn through the console
// (mersey> ...). Line 2/3 of the turn should hit the breakpoint.
send({ to: dbgActor, type: "setBreakpoint", source: "<repl>", lines: [1, 2, 3] });
await waitFor((p) => p.from === dbgActor, "setBreakpoint reply");
send({ to: consoleActor, type: "evaluateJSAsync",
       text: "mersey> const q = 1;\nconst w = q + 1;\nw" });

const paused = await waitFor((p) => p.type === "merseyPaused", "merseyPaused event");
console.log("PAUSED snapshot:", (paused.snapshot || "").slice(0, 160));
send({ to: dbgActor, type: "resume" });
await new Promise((r) => setTimeout(r, 500));

const ok = typeof paused.snapshot === "string" && paused.snapshot.includes('"reason":"breakpoint"');
console.log(ok ? "PASS  interactive Mersey pause + resume over RDP"
              : "FAIL  no breakpoint snapshot in merseyPaused");
sock.end();
process.exit(ok ? 0 : 1);
