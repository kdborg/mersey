// End-to-end test: the Mersey console in real Chromium, over CDP.
//
// Drives the fork's own `Mersey` protocol domain (third_party/blink/public/
// devtools_protocol/domains/Mersey.pdl) — Mersey.evaluate / Mersey.completions
// — and checks it against Runtime.evaluate in the JS realm. Proves the console
// switch AND the isolation contract: no variable crosses between the two.
//
// Usage:
//   out/mersey-arm64/Chromium.app/Contents/MacOS/Chromium \
//     --remote-debugging-port=9222 --headless=new --user-data-dir=/tmp/crprof \
//     "data:text/html,<h1>mersey</h1>" &
//   node chromium/test-devtools-console.mjs 9222
//
// No dependencies: a minimal WebSocket client, because CDP speaks WS and this
// repo carries no node_modules.
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

const js = async (expr) => {
  const r = await send("Runtime.evaluate", { expression: expr, returnByValue: true });
  return r.exceptionDetails ? "EXCEPTION: " + r.exceptionDetails.text : JSON.stringify(r.result.value);
};
const mersey = async (src) => {
  const r = await send("Mersey.evaluate", { expression: src });
  if (r.isError) return `${r.isCompileError ? "COMPILE ERROR" : "RUNTIME ERROR"}: ${r.result}`;
  return r.result === "" ? "undefined" : JSON.stringify(r.result);
};

console.log("JS      2 + 3                  =>", await js("2 + 3"));
console.log("MERSEY  6 * 7                  =>", await mersey("6 * 7"));
console.log("MERSEY  let x: int32 = 5;      =>", await mersey("let x: int32 = 5;"));
console.log("MERSEY  x * 3                  =>", await mersey("x * 3"));
console.log("JS      globalThis.jsOnly = 99 =>", await js("globalThis.jsOnly = 99"));
console.log("MERSEY  jsOnly                 =>", await mersey("jsOnly"));
console.log("JS      typeof x               =>", await js("typeof x"));
const comp = await send("Mersey.completions", {});
console.log("MERSEY  completions            =>", JSON.stringify(comp.names));
ws.close();
