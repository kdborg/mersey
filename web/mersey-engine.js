/* Shared engine bootstrap: instantiates the Mersey WASM engine against a JS
 * realm and wires the universal bridge. Used by the page loader
 * (realm = window) and by the worker bootstrap (realm = the worker's self),
 * so a Mersey program runs identically on either thread.
 */
import { makeBridge } from "./mersey-bridge.js";

export async function startEngine({ engineUrl = "mersey_wasm.wasm", realm = globalThis } = {}) {
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
  const packed = (s) => {
    const [ptr, len] = writeStr(s);
    return (BigInt(ptr) << 32n) | BigInt(len);
  };

  const bridge = makeBridge(realm, (cb, argsJson) => {
    const [p, l] = writeStr(argsJson);
    exports.msy_invoke_args(cb, p, l);
  });

  const imports = {
    env: {
      host_print: (p, l) => console.log(readStr(p, l)),
      host_error: (p, l) => console.error("[mersey]", readStr(p, l)),
      // Legacy DOM fast path (kept: the Stage A goldens pin it).
      host_dom_set_text: (ip, il, tp, tl) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        if (el) el.textContent = readStr(tp, tl);
      },
      host_dom_get_text: (ip, il) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        return el ? packed(el.textContent ?? "") : 0n;
      },
      host_dom_on_click: (ip, il, cb) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        if (el) el.addEventListener("click", () => exports.msy_invoke(cb));
      },
      host_dom_create: () => 0n,
      host_dom_append: () => {},
      host_dom_remove: () => {},
      host_dom_get_value: () => 0n,
      host_dom_set_value: () => {},
      // Universal bridge.
      host_web_global: (np, nl) => BigInt(bridge.global(readStr(np, nl))),
      host_web_get: (t, pp, pl) => packed(bridge.get(Number(t), readStr(pp, pl))),
      host_web_set: (t, pp, pl, vp, vl) =>
        packed(bridge.set(Number(t), readStr(pp, pl), readStr(vp, vl))),
      host_web_call: (t, mp, ml, ap, al) =>
        packed(bridge.call(Number(t), readStr(mp, ml), readStr(ap, al))),
      host_web_new: (cp, cl, ap, al) =>
        packed(bridge.construct(readStr(cp, cl), readStr(ap, al))),
      // Fast paths.
      host_web_intern: (np, nl) => bridge.intern(readStr(np, nl)),
      host_web_get_id: (t, id) => packed(bridge.getId(Number(t), id)),
      host_web_set_str: (t, id, vp, vl) =>
        packed(bridge.setScalar(Number(t), id, readStr(vp, vl))),
      host_web_set_num: (t, id, v) => packed(bridge.setScalar(Number(t), id, v)),
      host_web_call_str: (t, id, ap, al) =>
        packed(bridge.callStr(Number(t), id, readStr(ap, al))),
      host_web_iterate: (t) => packed(bridge.iterate(Number(t))),
      host_web_release: (t) => bridge.release(Number(t)),
      host_web_bytes_read: (t) => {
        const bytes = bridge.bytesRead(Number(t));
        if (!bytes) return 0n;
        const ptr = exports.msy_alloc(bytes.length);
        mem().set(bytes, ptr);
        return (BigInt(ptr) << 32n) | BigInt(bytes.length);
      },
      host_web_bytes_write: (ptr, len) =>
        BigInt(bridge.bytesWrite(mem().subarray(Number(ptr), Number(ptr) + Number(len)))),
      host_web_instanceof: (t, c) => bridge.instanceOf(Number(t), Number(c)),
    },
  };

  let instance;
  try {
    ({ instance } = await WebAssembly.instantiateStreaming(fetch(engineUrl), imports));
  } catch {
    const bytes = await (await fetch(engineUrl)).arrayBuffer();
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
  }
  exports = instance.exports;

  // Custom Elements: Mersey can't subclass a host class, so the bridge
  // builds the JS class and forwards the lifecycle callbacks into Mersey.
  realm.merseyDefineElement = (tag, handlers) => bridge.defineElement(tag, handlers);

  const readPacked = (packedValue) => {
    const ptr = Number(packedValue >> 32n);
    const len = Number(packedValue & 0xffffffffn);
    return readStr(ptr, len);
  };

  // Module graph (spec §4.5: closed before execution). The engine never does
  // I/O — it reports the specifiers it needs and the host fetches them.
  const isRelative = (s) => s.startsWith("./") || s.startsWith("../");
  const resolve = (referrer, spec) => {
    const parts = referrer.split("/").slice(0, -1);
    for (const seg of spec.split("/")) {
      if (seg === "." || seg === "") continue;
      if (seg === "..") parts.pop();
      else parts.push(seg);
    }
    return parts.join("/");
  };
  const scanImports = (source) => {
    const [p, l] = writeStr(source);
    return JSON.parse(readPacked(exports.msy_scan_imports(p, l)));
  };

  async function loadGraph(entrySpec, entrySource, fetchModule) {
    const sources = new Map([[entrySpec, entrySource]]);
    const deps = new Map();
    const queue = [entrySpec];
    while (queue.length) {
      const spec = queue.pop();
      const edges = [];
      for (const imp of scanImports(sources.get(spec))) {
        if (!isRelative(imp)) continue;
        const target = resolve(spec, imp);
        edges.push(target);
        if (!sources.has(target)) {
          sources.set(target, await fetchModule(target));
          queue.push(target);
        }
      }
      deps.set(spec, edges);
    }
    // Dependency-first order (cycles rejected).
    const order = [];
    const done = new Set();
    const path = [];
    const visit = (spec) => {
      if (done.has(spec)) return;
      if (path.includes(spec)) {
        throw new Error(`import cycle: ${[...path, spec].join(" → ")}`);
      }
      path.push(spec);
      for (const d of deps.get(spec) ?? []) visit(d);
      path.pop();
      done.add(spec);
      order.push(spec);
    };
    visit(entrySpec);
    return order.map((spec) => ({ spec, source: sources.get(spec) }));
  }

  return {
    /// Run one module (no relative imports).
    run(source) {
      const [ptr, len] = writeStr(source);
      return exports.msy_run(ptr, len);
    },
    /// Fetch and run a whole module graph rooted at `entrySpec`.
    async runGraph(entrySpec, entrySource, fetchModule) {
      const load = fetchModule ?? (async (u) => (await fetch(u)).text());
      const modules = await loadGraph(entrySpec, entrySource, load);
      const payload = JSON.stringify({ entry: entrySpec, modules });
      const [ptr, len] = writeStr(payload);
      return exports.msy_run_graph(ptr, len);
    },
    invoke: (cb) => exports.msy_invoke(cb),
    exports: () => exports,
  };
}
