// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

/* Service Worker bootstrap: runs a .mersey program as a service worker.
 *
 * Registration (from Mersey on the page):
 *   navigator.serviceWorker.register("mersey-sw.js?src=demo/sw.mersey",
 *                                    { type: "module" });
 *
 * The engine is instantiated in the SW global scope, so the Mersey program
 * sees `addEventListener`, `caches`, `clients`, `fetch` … as ambient globals.
 *
 * One wrinkle the spec forces: a service worker only receives `fetch` events
 * if it registered a listener *synchronously during initial evaluation* — and
 * booting a WASM engine is asynchronous. So this shim registers the real
 * listener up front, holds each request until the engine is ready, and then
 * dispatches it to whatever handlers the Mersey program installed.
 */
import { startEngine } from "./mersey-engine.js";

const params = new URLSearchParams(self.location.search);
const src = params.get("src");
const engineUrl = params.get("engine") ?? "mersey_wasm.wasm";

// Capture the Mersey program's `fetch` listeners instead of letting them
// reach the real (already-dispatched) event target.
const fetchHandlers = [];
const realAddEventListener = self.addEventListener.bind(self);
self.addEventListener = (type, fn, opts) => {
  if (type === "fetch") {
    fetchHandlers.push(fn);
    return;
  }
  realAddEventListener(type, fn, opts);
};

const booted = (async () => {
  const engine = await startEngine({ engineUrl, realm: self });
  const source = await (await fetch(src)).text();
  const status = await engine.runGraph(src, source);
  if (status !== 0) {
    console.error(`[mersey sw] ${src} exited with status ${status}`);
  }
})();

realAddEventListener("install", (ev) => ev.waitUntil(booted.then(() => self.skipWaiting())));
realAddEventListener("activate", (ev) => ev.waitUntil(booted.then(() => self.clients.claim())));

// Registered synchronously — this is what makes the worker fetch-capable.
realAddEventListener("fetch", (ev) => {
  ev.respondWith(
    (async () => {
      await booted;
      // A proxy event: Mersey calls `respondWith` on it, we capture the
      // response. If no handler answers, fall through to the network.
      let answer = null;
      const proxy = {
        request: ev.request,
        respondWith: (response) => {
          answer = response;
        },
      };
      for (const handler of fetchHandlers) {
        handler(proxy);
      }
      return answer ? await answer : fetch(ev.request);
    })(),
  );
});
