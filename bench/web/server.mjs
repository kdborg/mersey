// Minimal static server rooted at the repo root, so a page can reach both
// /web (loader, wasm, bridge) and /bench (workloads).
import { createServer } from "node:http";
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
        });
        res.end(`payload-${url.searchParams.get("i") ?? "0"}`);
        return;
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
  return new Promise((resolve) => {
    server.listen(port, () => resolve({ server, port: server.address().port }));
  });
}
