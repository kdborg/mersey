// Minimal static server rooted at the repo root, so a page can reach both
// /web (loader, wasm, bridge) and /bench (workloads).
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".mersey": "text/plain; charset=utf-8",
  ".json": "application/json",
  ".css": "text/css",
};

export function startServer(port = 0, opts = {}) {
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://localhost");
      // Runner-specific routes (e.g. run-firefox-real.mjs's RESULT post-back).
      if (opts.handle && (await opts.handle(req, res, url))) return;
      // Tiny dynamic endpoint for the fetch workload: deterministic payload,
      // no-store so every iteration is a real request, not a cache read.
      if (url.pathname === "/bench/echo") {
        res.writeHead(200, {
          "content-type": "text/plain; charset=utf-8",
          "cache-control": "no-store",
          // file:// pages have opaque origins (Ladybird's test-web loads
          // tests from disk); the echo endpoint admits them via CORS so the
          // fetch workload can run there too.
          "access-control-allow-origin": "*",
        });
        res.end(`payload-${url.searchParams.get("i") ?? "0"}`);
        return;
      }
      // Server-sent events for the sse workload: n events pushed immediately,
      // then the connection is HELD OPEN — ending it would trigger
      // EventSource's auto-reconnect and double-count the events. The client
      // closes when it has counted its n; socket teardown cleans up here.
      if (url.pathname === "/bench/sse") {
        const n = Number(url.searchParams.get("n") ?? "0");
        res.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-store",
          "access-control-allow-origin": "*",
        });
        for (let i = 0; i < n; i++) res.write(`data: sse-${i}\n\n`);
        return; // no res.end()
      }
      const path = join(root, decodeURIComponent(url.pathname));
      if (!path.startsWith(root)) {
        res.writeHead(403).end("forbidden");
        return;
      }
      let body = await readFile(path);
      // Optional HTML rewrite hook, so a runner can instrument the benchmark
      // pages (console capture) without the checked-in pages changing.
      if (opts.transformHtml && extname(path) === ".html") {
        body = Buffer.from(opts.transformHtml(body.toString("utf8"), url.pathname));
      }
      res.writeHead(200, {
        "content-type": MIME[extname(path)] ?? "application/octet-stream",
        // COOP/COEP so performance.memory is populated and cross-origin isolated.
        "cross-origin-opener-policy": "same-origin",
        "cross-origin-embedder-policy": "require-corp",
      });
      res.end(body);
    } catch {
      res.writeHead(404).end("not found");
    }
  });
  // WebSocket echo at /bench/ws for the websocket workload: a dependency-free
  // RFC 6455 endpoint that echoes text frames back. Small frames only
  // (payload < 126 bytes) — the workload's messages are a few bytes, and
  // keeping the codec trivial keeps the server out of the measurement.
  server.on("upgrade", (req, socket) => {
    if (new URL(req.url, "http://localhost").pathname !== "/bench/ws") {
      socket.destroy();
      return;
    }
    const accept = createHash("sha1")
      .update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
      .digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n" +
        `Connection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    let buf = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buf = Buffer.concat([buf, chunk]);
      // Client frames are masked: FIN+opcode, MASK+len7, 4-byte key, payload.
      while (buf.length >= 6) {
        const opcode = buf[0] & 0x0f;
        const len = buf[1] & 0x7f;
        if (len > 125 || buf.length < 6 + len) break;
        const key = buf.subarray(2, 6);
        const payload = Buffer.from(buf.subarray(6, 6 + len));
        for (let i = 0; i < len; i++) payload[i] ^= key[i % 4];
        buf = buf.subarray(6 + len);
        if (opcode === 8) {
          socket.end(Buffer.from([0x88, 0]));
          return;
        }
        if (opcode !== 1) continue; // echo text frames only
        socket.write(Buffer.concat([Buffer.from([0x81, payload.length]), payload]));
      }
    });
    socket.on("error", () => socket.destroy());
  });
  return new Promise((resolve) => {
    server.listen(port, () => resolve({ server, port: server.address().port }));
  });
}
