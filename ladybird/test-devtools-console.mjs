// End-to-end test: Ladybird's DevTools console in Mersey mode, over the real
// Firefox remote-debugging protocol. Proves the console switch AND the
// isolation contract (no variable crosses between the JS and Mersey realms).
//
// Usage:
//   Build/release/bin/Ladybird.app/Contents/MacOS/Ladybird --devtools=6000 \
//       "data:text/html,<h1>mersey</h1>" &
//   node ladybird/test-devtools-console.mjs 6000
//
// Ladybird ships no DevTools UI, so `mersey>` stands in for the language
// dropdown a Mersey-aware front-end sends as "language":"mersey".
import net from "node:net";

const PORT = Number(process.argv[2] || 6000);
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
    const body = buf.subarray(colon + 1, colon + 1 + len).toString("utf8");
    buf = buf.subarray(colon + 1 + len);
    const pkt = JSON.parse(body);
    if (onPacket) onPacket(pkt);
    pending.push(pkt);
  }
});

const waitFor = (pred, what, ms = 15000) =>
  new Promise((res, rej) => {
    const hit = pending.find(pred);
    if (hit) return res(hit);
    const timer = setTimeout(() => rej(new Error(`timeout waiting for ${what}`)), ms);
    onPacket = (p) => { if (pred(p)) { clearTimeout(timer); onPacket = null; res(p); } };
  });

await new Promise((r) => sock.once("connect", r));
await waitFor((p) => p.from === "root", "root hello");

send({ to: "root", type: "listTabs" });
const tabs = await waitFor((p) => Array.isArray(p.tabs), "listTabs");
const tab = tabs.tabs[0];
console.log("tab:", tab.actor, tab.url ?? "");

send({ to: tab.actor, type: "getWatcher" });
const watcher = await waitFor((p) => p.actor && p.traits, "getWatcher");
console.log("watcher:", watcher.actor);

send({ to: watcher.actor, type: "watchTargets", targetType: "frame" });
const target = await waitFor((p) => p.type === "target-available-form", "frame target");
const consoleActor = target.target.consoleActor;
console.log("console:", consoleActor);

async function evaluate(text) {
  send({ to: consoleActor, type: "evaluateJSAsync", text });
  const res = await waitFor(
    (p) => p.type === "evaluationResult" && p.input === text, `result for ${text}`);
  return res.result;
}

// JS mode, then Mersey mode via the `mersey>` prefix.
console.log("JS      2+3        =>", JSON.stringify(await evaluate("2 + 3")));
console.log("MERSEY  6 * 7      =>", JSON.stringify(await evaluate("mersey> 6 * 7")));
console.log("MERSEY  let x = 5  =>", JSON.stringify(await evaluate("mersey> let x: int32 = 5;")));
console.log("MERSEY  x * 3      =>", JSON.stringify(await evaluate("mersey> x * 3")));
// Isolation: a JS global must NOT be visible to Mersey, and vice versa.
console.log("JS      var jsOnly =>", JSON.stringify(await evaluate("globalThis.jsOnly = 99")));
console.log("MERSEY  jsOnly     =>", JSON.stringify(await evaluate("mersey> jsOnly")));
console.log("JS      typeof x   =>", JSON.stringify(await evaluate("typeof x")));
sock.end();
