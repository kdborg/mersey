// End-to-end test: the Mersey DEBUGGER in real Firefox, over RDP.
//
// Drives the fork's merseyDebugger target actor: getScripts ->
// setBreakpoints (installs the engine hook itself) -> a real click pauses
// (the content process spins its event loop, which is what keeps this very
// connection serviced) -> stepOver -> resume -> disable.
//
// Usage: as test-devtools-console.mjs, but load web/todo-native.html.
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

// The merseyDebugger target-scoped actor rides the same target form.
const dbg = targetReply.frame.merseyDebuggerActor;
if (!dbg) { console.error("FATAL: no merseyDebuggerActor in target form"); process.exit(1); }
console.log("debugger actor          =>", dbg);

const waitEvent = (type, ms = 20000) =>
  new Promise((res, rej) => {
    const i = seen.findIndex((p) => p.from === dbg && p.type === type);
    if (i >= 0) return res(seen.splice(i, 1)[0]);
    const t = setTimeout(() => rej(new Error("timeout event " + type)), ms);
    onPacket = (x) => {
      if (x.from === dbg && x.type === type) {
        clearTimeout(t); onPacket = null;
        const j = seen.indexOf(x); if (j >= 0) seen.splice(j, 1);
        res(x);
      }
    };
  });
const call = (packet, ms) => {
  send({ to: dbg, ...packet });
  return waitFor((p) => p.from === dbg && (p.scripts !== undefined || p.type === undefined || p.error), "reply " + packet.type, ms);
};

// 1. Sources: the inline todo script, engine lines.
const scripts = await call({ type: "getScripts" });
const source = scripts.scripts[0]?.source ?? "";
const lines = source.split("\n");
const target = lines.findIndex((l) => l.includes("if (text.length")) + 1;
console.log("scripts                 =>", scripts.scripts.length, "| bp line:", target);

// 2. Breakpoint (installs the hook itself — no separate attach).
await call({ type: "setBreakpoints", lines: [target] });
console.log("setBreakpoints          => ok");

// 3. A real listener dispatch: click Add via the console actor (its reply
//    cannot arrive while the engine is paused — do not await it).
send({ to: consoleActor, type: "evaluateJSAsync", text:
  "document.getElementById('new-todo').value='fx dbg'; document.getElementById('add').click(); 1" });

const paused = await waitEvent("paused");
const f = paused.pause.frames[0];
const locals = (f.scopes?.[0]?.variables ?? []).map((v) => `${v.name}=${v.value}`).join(", ");
console.log(`PAUSED                  => ${paused.pause.reason} at ${f.name}:${f.line} [${locals}]`);

// 4. Step: the next statement, same frame.
send({ to: dbg, type: "action", name: "stepOver" });
const stepped = await waitEvent("paused");
console.log(`STEP                    => ${stepped.pause.reason} at ${stepped.pause.frames[0].name}:${stepped.pause.frames[0].line}`);

// 5. Resume; the page finishes its click.
send({ to: dbg, type: "action", name: "resume" });
await waitEvent("resumed");
console.log("resumed                 => ok");

// 6. Detach: the next click must NOT pause.
send({ to: dbg, type: "action", name: "disable" });
const after = await evaluate("document.getElementById('new-todo').value='no pause'; document.getElementById('add').click(); document.querySelectorAll('#list li').length", "javascript");
console.log("after disable, items    =>", after, "(clicks ran without pausing)");
sock.end();
