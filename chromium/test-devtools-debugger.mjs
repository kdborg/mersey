// End-to-end test: BREAKPOINTS in the Chromium fork, over CDP.
//
// Drives the fork's Mersey domain debugger: enableDebugger -> setBreakpoints ->
// run code -> Mersey.paused (stack + scopes) -> stepOver -> resume.
//
// The interesting part is that Mersey.evaluate does NOT return while paused:
// the engine blocks inside its pause callout and the renderer runs a nested
// message loop (the same one V8 uses), which is what keeps CDP alive so the
// resume command can even be delivered.
//
// Usage:
//   out/mersey-arm64/Chromium.app/Contents/MacOS/Chromium \
//     --remote-debugging-port=9222 --headless=new --user-data-dir=/tmp/crprof \
//     "data:text/html,<h1>mersey</h1>" &
//   node chromium/test-devtools-debugger.mjs 9222
import net from "node:net";
import crypto from "node:crypto";

const PORT = Number(process.argv[2] || 9222);

// Surface anything that would otherwise fail silently.
process.on("unhandledRejection", (e) => { console.error("FATAL:", e); process.exit(1); });

function connectWS(wsUrl) {
  const u = new URL(wsUrl);
  return new Promise((resolve, reject) => {
    const key = crypto.randomBytes(16).toString("base64");
    const sock = net.connect(Number(u.port), u.hostname, () => {
      sock.write(
        `GET ${u.pathname}${u.search} HTTP/1.1\r\nHost: ${u.host}\r\n` +
        `Upgrade: websocket\r\nConnection: Upgrade\r\n` +
        `Sec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`);
    });
    let buf = Buffer.alloc(0);
    let open = false;
    const handlers = [];
    sock.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (!open) {
        const end = buf.indexOf("\r\n\r\n");
        if (end < 0) return;
        if (!buf.subarray(0, end).toString().includes("101")) {
          return reject(new Error("websocket upgrade failed"));
        }
        buf = buf.subarray(end + 4);
        open = true;
        resolve({ send, onMessage: (h) => handlers.push(h), close: () => sock.end() });
      }
      // Decode server frames (never masked).
      for (;;) {
        if (buf.length < 2) return;
        const len0 = buf[1] & 0x7f;
        let off = 2, len = len0;
        if (len0 === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
        else if (len0 === 127) { if (buf.length < 10) return; len = Number(buf.readBigUInt64BE(2)); off = 10; }
        if (buf.length < off + len) return;
        const payload = buf.subarray(off, off + len).toString("utf8");
        buf = buf.subarray(off + len);
        if ((buf.length || true) && payload) {
          for (const h of handlers) h(payload);
        }
      }
    });
    sock.on("error", reject);

    function send(text) {
      const body = Buffer.from(text, "utf8");
      const mask = crypto.randomBytes(4);
      const head = [];
      head.push(0x81);
      if (body.length < 126) head.push(0x80 | body.length);
      else if (body.length < 65536) head.push(0x80 | 126, body.length >> 8, body.length & 0xff);
      else throw new Error("frame too large for this test");
      const masked = Buffer.from(body);
      for (let i = 0; i < masked.length; i++) masked[i] ^= mask[i & 3];
      sock.write(Buffer.concat([Buffer.from(head), mask, masked]));
    }
  });
}

const targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) throw new Error("no page target");
const ws = await connectWS(page.webSocketDebuggerUrl);

let nextId = 1;
const pending = new Map();
ws.onMessage((text) => {
  const msg = JSON.parse(text);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
});
const send = (method, params = {}) =>
  new Promise((res, rej) => {
    const id = nextId++;
    pending.set(id, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
    ws.send(JSON.stringify({ id, method, params }));
    setTimeout(() => pending.has(id) && rej(new Error("timeout: " + method)), 15000);
  });


// Event listeners (the console test only needed command replies).
const events = [];
const eventWaiters = [];
ws.onMessage((text) => {
  const msg = JSON.parse(text);
  if (!msg.id && msg.method) {
    events.push(msg);
    for (let i = eventWaiters.length - 1; i >= 0; i--) {
      if (eventWaiters[i].match(msg)) {
        // Consume it: leaving a matched event in the buffer would satisfy the
        // NEXT wait too, and every pause would look like the first one.
        const j = events.indexOf(msg);
        if (j >= 0) {
          events.splice(j, 1);
        }
        eventWaiters.splice(i, 1)[0].resolve(msg);
      }
    }
  }
});
const waitEvent = (method, ms = 20000) =>
  new Promise((res, rej) => {
    const hit = events.findIndex((e) => e.method === method);
    if (hit >= 0) return res(events.splice(hit, 1)[0]);
    const t = setTimeout(() => rej(new Error("timeout waiting for " + method)), ms);
    eventWaiters.push({
      match: (m) => m.method === method,
      resolve: (m) => { clearTimeout(t); res(m); },
    });
  });

const showFrames = (params) => {
  const f = params.callFrames[0];
  const locals = (f.scopeChain[0]?.variables ?? []).map((v) => `${v.name}=${v.value}`);
  return `reason=${params.reason} at ${f.functionName}:${f.lineNumber + 1} locals[${locals.join(", ")}] depth=${params.callFrames.length}`;
};

await send("Mersey.enableDebugger", {});
console.log("enableDebugger              => ok");

// The REPL session is one growing module; line 1 is the console prelude.
//   2: function addUp(...) {
//   3:   const s = a + b;
//   4:   return s;
await send("Mersey.evaluate", {
  expression: "function addUp(a: int32, b: int32): int32 {\n  const s = a + b;\n  return s;\n}",
});
console.log("defined addUp              => ok");

// CDP lines are 0-based; engine line 3 is the `const s = ...` statement.
await send("Mersey.setBreakpoints", { url: "", lines: [2] });
console.log("setBreakpoints line 3      => ok");

// Do NOT await: this call cannot return until the pause is released.
const running = send("Mersey.evaluate", { expression: "addUp(2, 3)" });

const paused = await waitEvent("Mersey.paused");
console.log("PAUSED                     =>", showFrames(paused.params));

await send("Mersey.stepOver", {});
const stepped = await waitEvent("Mersey.paused");
console.log("after stepOver             =>", showFrames(stepped.params));

await send("Mersey.resume", {});
await waitEvent("Mersey.resumed");
const result = await running;
console.log("evaluate returned          =>", JSON.stringify(result.result));

await send("Mersey.disableDebugger", {});
const after = await send("Mersey.evaluate", { expression: "addUp(10, 20)" });
console.log("after disableDebugger      =>", JSON.stringify(after.result), "(no pause expected)");
ws.close();
