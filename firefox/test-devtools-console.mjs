// End-to-end test: the Mersey console in real Firefox, over RDP.
//
// Sends language:"mersey" on evaluateJSAsync — the field the console's
// language dropdown sets (EvaluationContextSelector -> evalLanguage ->
// scriptCommand.execute). Proves the switch AND the isolation contract: no
// variable crosses between the JS and Mersey realms, in either direction.
//
// Usage (from the Gecko fork; its own checkout holds the fork changes):
//   obj-mersey/dist/Nightly.app/Contents/MacOS/firefox --profile /tmp/fxprof \
//       --headless --start-debugger-server 6000 "data:text/html,<h1>mersey</h1>" &
//   node firefox/test-devtools-console.mjs 6000
//
// The profile needs devtools.debugger.remote-enabled and
// devtools.debugger.prompt-connection=false.
import net from "node:net";
const PORT = Number(process.argv[2] || 6000);
const sock = net.connect(PORT, "127.0.0.1");
let buf = Buffer.alloc(0);
const seen = [];
let onPacket = null;
function send(o) {
  const b = Buffer.from(JSON.stringify(o), "utf8");
  sock.write(Buffer.concat([Buffer.from(`${b.length}:`, "utf8"), b]));
}
sock.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const c = buf.indexOf(0x3a);
    if (c < 0) return;
    const len = Number(buf.subarray(0, c).toString());
    if (!Number.isFinite(len) || buf.length < c + 1 + len) return;
    const pkt = JSON.parse(buf.subarray(c + 1, c + 1 + len).toString("utf8"));
    buf = buf.subarray(c + 1 + len);
    seen.push(pkt);
    if (onPacket) onPacket(pkt);
  }
});
// Consume on match: an evaluationResult left in the buffer would otherwise
// satisfy the NEXT wait too, and every turn would report the first result.
const waitFor = (p, what, ms = 20000) =>
  new Promise((res, rej) => {
    const i = seen.findIndex(p);
    if (i >= 0) return res(seen.splice(i, 1)[0]);
    const t = setTimeout(() => rej(new Error("timeout: " + what)), ms);
    onPacket = (x) => {
      if (p(x)) {
        clearTimeout(t); onPacket = null;
        const j = seen.indexOf(x); if (j >= 0) seen.splice(j, 1);
        res(x);
      }
    };
  });

await new Promise((r) => sock.once("connect", r));
await waitFor((p) => p.from === "root", "root");
send({ to: "root", type: "listTabs" });
const { tabs } = await waitFor((p) => Array.isArray(p.tabs), "listTabs");
const tab = tabs.find((t) => t.selected) || tabs[0];
// Firefox: the tab descriptor hands back the frame target directly.
send({ to: tab.actor, type: "getTarget" });
const targetReply = await waitFor((p) => p.frame && p.frame.consoleActor, "getTarget");
const consoleActor = targetReply.frame.consoleActor;
console.log("console actor:", consoleActor);

let n = 0;
async function evaluate(text, language) {
  const tag = `${text}#${n++}`;
  send({ to: consoleActor, type: "evaluateJSAsync", text, language });
  const first = await waitFor((p) => p.resultID && p.from === consoleActor, tag);
  const id = first.resultID;
  const r = await waitFor(
    (p) => p.type === "evaluationResult" && p.resultID === id, `result ${tag}`);
  if (r.exceptionMessage) return "EXCEPTION: " + r.exceptionMessage;
  const v = r.result;
  return v && typeof v === "object" && v.type ? v.type : JSON.stringify(v);
}

console.log("JS      2 + 3            =>", await evaluate("2 + 3", "javascript"));
console.log("MERSEY  6 * 7            =>", await evaluate("6 * 7", "mersey"));
console.log("MERSEY  let x: int32 = 5 =>", await evaluate("let x: int32 = 5;", "mersey"));
console.log("MERSEY  x * 3            =>", await evaluate("x * 3", "mersey"));
console.log("JS      globalThis.jsOnly=>", await evaluate("globalThis.jsOnly = 99", "javascript"));
console.log("MERSEY  jsOnly           =>", await evaluate("jsOnly", "mersey"));
console.log("JS      typeof x         =>", await evaluate("typeof x", "javascript"));

// --- completion surface: Mersey mode must not see the JS realm ------------
async function complete(text, language) {
  send({ to: consoleActor, type: "autocomplete", text, cursor: text.length, language });
  const r = await waitFor((p) => p.from === consoleActor && p.matches !== undefined, "autocomplete " + language);
  return r.matches || [];
}

await evaluate("let merseyOnly: int32 = 7;", "mersey");
await evaluate("globalThis.jsOnlyFn = function () {};", "javascript");

const mCompletions = await complete("", "mersey");
console.log("MERSEY  completions            =>", JSON.stringify(mCompletions));
const leaked = mCompletions.filter((n) => ["window", "document", "jsOnlyFn", "globalThis"].includes(n));
console.log("MERSEY  JS names leaked        =>", leaked.length === 0 ? "none (correct)" : JSON.stringify(leaked));

const jCompletions = await complete("wind", "javascript");
console.log("JS      completions for 'wind' =>", JSON.stringify(jCompletions.slice(0, 4)));

sock.end();
