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

  // Sources of every module we loaded, so an error can show a code frame.
  const sources = new Map();

  /* Errors carry `file:line:col` (the engine keeps a pc→position table and a
   * frame stack). DevTools cannot step through Mersey bytecode in Stage A —
   * that needs the native CDP integration — but it *can* show you exactly
   * which line failed, which is most of what a stack trace is for. */
  function report(message) {
    const frame = /\(([^()]+):(\d+):(\d+)\)/.exec(message);
    if (!frame) {
      console.error("[mersey]", message);
      return;
    }
    const [, file, lineNo, colNo] = frame;
    const source = sources.get(file);
    if (!source) {
      console.error("[mersey]", message);
      return;
    }
    const line = source.split("\n")[Number(lineNo) - 1] ?? "";
    const caret = " ".repeat(Math.max(0, Number(colNo) - 1)) + "^";
    console.error(
      `%c[mersey] ${message}\n\n  ${file}:${lineNo}:${colNo}\n  ${line}\n  ${caret}`,
      "color:#c00",
    );
  }

  const imports = {
    env: {
      host_print: (p, l) => console.log(readStr(p, l)),
      host_error: (p, l) => report(readStr(p, l)),
      // Legacy DOM fast path (kept: the Stage A goldens pin it).
      host_dom_set_text: (ip, il, tp, tl) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        if (el) el.textContent = readStr(tp, tl);
      },
      host_dom_get_text: (ip, il) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        return el ? packed(el.textContent ?? "") : 0n;
      },
      host_dom_add_listener: (ip, il, ep, el_, cb) => {
        const el = realm.document?.getElementById(readStr(ip, il));
        // Any event the DOM knows: the engine does not keep a list of them,
        // because the host is what owns the event loop.
        if (el) el.addEventListener(readStr(ep, el_), () => exports.msy_invoke(cb));
      },
      // Randomness is denied unless the page grants it — deny by default, like
      // every other capability (§5.3). The grant is on the script tag:
      //   <script type="text/mersey" src="app.mersey" data-allow="random">
      host_print_level: (lp, ll, p, l) => {
        const level = readStr(lp, ll);
        const line = readStr(p, l);
        // A level in Mersey is the same level in the browser's console.
        (realm.console[level] ?? realm.console.log).call(realm.console, line);
      },
      host_random_bytes: (ptr, len) => {
        if (!realm.__merseyAllow?.has?.("random")) return 0;
        const view = new Uint8Array(exports.memory.buffer, Number(ptr), Number(len));
        // The platform CSPRNG, in chunks it will actually serve.
        for (let off = 0; off < view.length; off += 65536) {
          realm.crypto.getRandomValues(view.subarray(off, Math.min(off + 65536, view.length)));
        }
        return 1;
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
      host_time_ms: (epoch) => (epoch ? Date.now() : performance.now()),
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
    sources.set(entrySpec, entrySource);
    const deps = new Map();      // static edges: execution order
    const allDeps = new Map();   // static + dynamic: checking order
    const queue = [entrySpec];
    while (queue.length) {
      const spec = queue.pop();
      const edges = [];
      const scanned = scanImports(sources.get(spec));
      for (const imp of scanned.static) {
        if (!isRelative(imp)) continue;
        const target = resolve(spec, imp);
        edges.push(target);
        if (!sources.has(target)) {
          sources.set(target, await fetchModule(target));
          queue.push(target);
        }
      }
      // A dynamic `import("./x")` target is fetched and checked with the rest —
      // the graph is closed before execution (§4.5) — but it is not an ordering
      // edge: nothing waits for it to run.
      const dynEdges = [];
      for (const imp of scanned.dynamic) {
        if (!isRelative(imp)) continue;
        const target = resolve(spec, imp);
        dynEdges.push(target);
        if (!sources.has(target)) {
          sources.set(target, await fetchModule(target));
          queue.push(target);
        }
      }
      deps.set(spec, edges);
      allDeps.set(spec, [...edges, ...dynEdges]);
    }
    // Dependency-first order (cycles rejected).
    const topo = (edges) => {
      const order = [];
      const done = new Set();
      const path = [];
      const visit = (spec) => {
        if (done.has(spec)) return;
        if (path.includes(spec)) {
          throw new Error(`import cycle: ${[...path, spec].join(" → ")}`);
        }
        path.push(spec);
        for (const d of edges.get(spec) ?? []) visit(d);
        path.pop();
        done.add(spec);
        order.push(spec);
      };
      visit(entrySpec);
      return order;
    };
    // Checking order follows both kinds of edge, so a dynamically imported
    // module's exports are known before the module that imports it is typed.
    // Execution order follows only the static ones: nothing waits for a lazy
    // module to run.
    const checkOrder = topo(allDeps);
    const execOrder = topo(deps);
    const lazy = checkOrder.filter((s) => !execOrder.includes(s));
    return {
      modules: checkOrder.map((spec) => ({ spec, source: sources.get(spec) })),
      lazy,
    };
  }

  return {
    /// Transpile one module to JavaScript (the JS-backend polyfill). Returns
    /// the JS text, or throws with the checker's diagnostics.
    transpile(source, name) {
      const [ptr, len] = writeStr(source);
      let packed;
      if (exports.msy_transpile_named) {
        const [nptr, nlen] = writeStr(name || "module");
        packed = exports.msy_transpile_named(ptr, len, nptr, nlen);
      } else {
        packed = exports.msy_transpile(ptr, len);
      }
      const out = readStr(Number(packed >> 32n), Number(packed & 0xffffffffn));
      if (out.startsWith("!")) throw new Error(out.slice(1));
      return out;
    },
    /// Transpile a whole module graph: fetches the closed graph exactly as
    /// runGraph does, checks it as one program, and returns
    /// { modules: [{spec, js}], lazy } dependency-first.
    async transpileGraph(entrySpec, entrySource, fetchModule) {
      const load = fetchModule ?? (async (u) => (await fetch(u)).text());
      const { modules, lazy } = await loadGraph(entrySpec, entrySource, load);
      const payload = JSON.stringify({ entry: entrySpec, modules });
      const [ptr, len] = writeStr(payload);
      const packed = exports.msy_transpile_graph(ptr, len);
      const out = readStr(Number(packed >> 32n), Number(packed & 0xffffffffn));
      if (out.startsWith("!")) throw new Error(out.slice(1));
      return { modules: JSON.parse(out).modules, lazy };
    },
    /// One browser-console REPL turn (`globalThis.mersey`): the echo text
    /// (may be empty), or throws with the diagnostics of a rejected turn.
    replTurn(source) {
      if (!exports.msy_repl_turn) {
        throw new Error("this engine build has no REPL");
      }
      const [ptr, len] = writeStr(source);
      const packed = exports.msy_repl_turn(ptr, len);
      const out = readStr(Number(packed >> 32n), Number(packed & 0xffffffffn));
      if (out.startsWith("!")) throw new Error(out.slice(1));
      return out;
    },
    /// The REPL session's visible names (JSON array) for console completion.
    replComplete() {
      if (!exports.msy_repl_complete) return "[]";
      const packed = exports.msy_repl_complete();
      return readStr(Number(packed >> 32n), Number(packed & 0xffffffffn));
    },
    /// The standalone runtime module text ($rt), shared by a graph's modules.
    runtimeJs() {
      const packed = exports.msy_rt_js();
      return readStr(Number(packed >> 32n), Number(packed & 0xffffffffn));
    },
    /// Run one module (no relative imports).
    run(source) {
      sources.set("<script>", source);
      const [ptr, len] = writeStr(source);
      return exports.msy_run(ptr, len);
    },
    /// Fetch and run a whole module graph rooted at `entrySpec`.
    async runGraph(entrySpec, entrySource, fetchModule) {
      const load = fetchModule ?? (async (u) => (await fetch(u)).text());
      const { modules, lazy } = await loadGraph(entrySpec, entrySource, load);
      const payload = JSON.stringify({ entry: entrySpec, modules, lazy });
      const [ptr, len] = writeStr(payload);
      return exports.msy_run_graph(ptr, len);
    },
    invoke: (cb) => exports.msy_invoke(cb),
    exports: () => exports,
  };
}
