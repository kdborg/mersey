/* Mersey Stage A loader polyfill (docs/architecture/browser-integration.md).
 *
 * Executes <script type="text/mersey"> tags via the Mersey engine compiled
 * to WebAssembly. This is the only JavaScript in the system, and it exists
 * precisely so the page author never writes any: it disappears entirely at
 * Stage B when Chromium hosts the engine natively.
 *
 * Usage:
 *   <script src="mersey-loader.js" data-engine="mersey_wasm.wasm" defer></script>
 *   <script type="text/mersey" src="app.mersey"></script>
 */
import { makeBridge } from "./mersey-bridge.js";

(() => {
  "use strict";

  // `document.currentScript` is null in module scripts; find our own tag.
  const self_ = document.querySelector('script[src$="mersey-loader.js"]');
  const engineUrl = (self_ && self_.dataset.engine) || "mersey_wasm.wasm";

  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let exports = null;

  const mem = () => new Uint8Array(exports.memory.buffer);
  const readStr = (ptr, len) => decoder.decode(mem().subarray(ptr, ptr + len));
  const writeStr = (s) => {
    const bytes = encoder.encode(s);
    const ptr = exports.msy_alloc(bytes.length);
    mem().set(bytes, ptr);
    return [ptr, bytes.length];
  };

  const imports = {
    env: {
      host_print: (p, l) => console.log(readStr(p, l)),
      host_error: (p, l) => console.error("[mersey]", readStr(p, l)),
      host_dom_set_text: (ip, il, tp, tl) => {
        const el = elementById(readStr(ip, il));
        if (el) el.textContent = readStr(tp, tl);
      },
      host_dom_get_text: (ip, il) => {
        const el = elementById(readStr(ip, il));
        if (!el) return 0n;
        const [ptr, len] = writeStr(el.textContent ?? "");
        return (BigInt(ptr) << 32n) | BigInt(len);
      },
      host_dom_on_click: (ip, il, cb) => {
        const el = elementById(readStr(ip, il));
        if (el) el.addEventListener("click", () => exports.msy_invoke(cb));
      },
      host_dom_create: (tp, tl) => {
        const el = document.createElement(readStr(tp, tl));
        const id = `--mersey-${nextId++}`;
        el.id = id;
        created.set(id, el);
        const [ptr, len] = writeStr(id);
        return (BigInt(ptr) << 32n) | BigInt(len);
      },
      host_dom_append: (pp, pl, cp, cl) => {
        const parent = elementById(readStr(pp, pl));
        const child = elementById(readStr(cp, cl));
        if (parent && child) parent.appendChild(child);
      },
      host_dom_remove: (ip, il) => {
        const el = elementById(readStr(ip, il));
        if (el) el.remove();
      },
      host_dom_get_value: (ip, il) => {
        const el = elementById(readStr(ip, il));
        const [ptr, len] = writeStr(el && "value" in el ? el.value : "");
        return (BigInt(ptr) << 32n) | BigInt(len);
      },
      host_dom_set_value: (ip, il, vp, vl) => {
        const el = elementById(readStr(ip, il));
        if (el && "value" in el) el.value = readStr(vp, vl);
      },
    },
  };
  let nextId = 1;
  const created = new Map();
  const elementById = (id) => created.get(id) ?? document.getElementById(id);

  // Universal bridge: every web technology, reflectively (no per-API glue).
  const bridge = makeBridge(globalThis, (cb, argsJson) => {
    const [p, l] = writeStr(argsJson);
    exports.msy_invoke_args(cb, p, l);
  });
  const reply = (s) => {
    const [ptr, len] = writeStr(s);
    return (BigInt(ptr) << 32n) | BigInt(len);
  };
  Object.assign(imports.env, {
    host_web_global: (np, nl) => BigInt(bridge.global(readStr(np, nl))),
    host_web_get: (t, pp, pl) => reply(bridge.get(Number(t), readStr(pp, pl))),
    host_web_set: (t, pp, pl, vp, vl) =>
      reply(bridge.set(Number(t), readStr(pp, pl), readStr(vp, vl))),
    host_web_call: (t, mp, ml, ap, al) =>
      reply(bridge.call(Number(t), readStr(mp, ml), readStr(ap, al))),
    host_web_new: (cp, cl, ap, al) =>
      reply(bridge.construct(readStr(cp, cl), readStr(ap, al))),
    // fast paths
    host_web_intern: (np, nl) => bridge.intern(readStr(np, nl)),
    host_web_get_id: (t, id) => reply(bridge.getId(Number(t), id)),
    host_web_set_str: (t, id, vp, vl) => reply(bridge.setScalar(Number(t), id, readStr(vp, vl))),
    host_web_set_num: (t, id, v) => reply(bridge.setScalar(Number(t), id, v)),
    host_web_call_str: (t, id, ap, al) => reply(bridge.callStr(Number(t), id, readStr(ap, al))),
  });

  async function boot() {
    const response = fetch(engineUrl);
    let instance;
    try {
      ({ instance } = await WebAssembly.instantiateStreaming(response, imports));
    } catch {
      // Server sent a non-wasm MIME type; fall back to ArrayBuffer.
      const bytes = await (await fetch(engineUrl)).arrayBuffer();
      ({ instance } = await WebAssembly.instantiate(bytes, imports));
    }
    exports = instance.exports;

    const tags = document.querySelectorAll('script[type="text/mersey"]');
    for (const tag of tags) {
      const source = tag.src
        ? await (await fetch(tag.src)).text()
        : tag.textContent;
      const [ptr, len] = writeStr(source);
      const status = exports.msy_run(ptr, len);
      if (status !== 0) {
        console.error(`[mersey] ${tag.src || "<inline>"}: exited with status ${status}`);
      }
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
