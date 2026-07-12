/* Web Worker bootstrap: runs a .mersey program on a worker thread.
 *
 * The engine is instantiated inside the worker with the bridge pointed at
 * the worker's own global scope, so the worker script sees `postMessage`,
 * `onmessage`, `fetch`, … exactly as ambient globals — the same imports it
 * would use on the main thread.
 *
 * Usage (from Mersey on the main thread):
 *   const w = new Worker("mersey-worker.js?src=demo/worker.mersey");
 *   w.onmessage = (ev: JsAny) => { … };
 *   w.postMessage(payload);
 */
import { startEngine } from "./mersey-engine.js";

const params = new URLSearchParams(self.location.search);
const src = params.get("src");
const engineUrl = params.get("engine") ?? "mersey_wasm.wasm";

// Messages that arrive before the engine finishes booting are replayed.
const pending = [];
let ready = false;
self.onmessage = (ev) => {
  if (!ready) pending.push(ev);
};

const engine = await startEngine({ engineUrl, realm: self });
const source = await (await fetch(src)).text();
const status = await engine.runGraph(src, source);
if (status !== 0) {
  console.error(`[mersey worker] ${src} exited with status ${status}`);
}
ready = true;

// Hand over any messages that queued during boot to the Mersey handler the
// worker script installed (self.onmessage was replaced by the bridge).
for (const ev of pending) {
  if (typeof self.onmessage === "function") self.onmessage(ev);
}
