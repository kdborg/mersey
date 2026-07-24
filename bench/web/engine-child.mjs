// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// One benchmark run in a fresh process: the wasm engine over a deterministic
// stub realm, no browser. Spawned by run-engine.mjs and perf-test.mjs so each
// workload gets a cold engine and its own peak-RSS (VmHWM) reading.
//
//   node engine-child.mjs <workload>   run bench/web/mersey/<workload>.mersey
//   node engine-child.mjs --blank      instantiate engine + realm, run nothing
//
// Prints the workload's console output (the RESULT line), then one line:
//   MEMSTAT vmhwm=<KiB> wasmheap=<bytes>
//
// The realm is the same shape web/test/platform.mjs proves the bridge
// against: real Node objects wherever Node has them (URL, TextEncoder,
// crypto, JSON, fetch), spec-faithful stubs for the DOM-shaped surface.
// Checksums must match the browser legs bit-for-bit — that is the proof the
// stubs are faithful where the workloads look.
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { makeBridge } from "../../web/mersey-bridge.js";
import { selfPeakMemoryKiB } from "./host-mem.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const wasmBytes = await readFile(
  join(root, "target/wasm32-unknown-unknown/release/mersey_wasm.wasm"),
);

// ---- deterministic realm ----------------------------------------------------

function makeElement(tag) {
  const el = {
    tagName: tag.toUpperCase(),
    textContent: "",
    className: "",
    children: [],
    listeners: Object.create(null),
    appendChild(c) {
      this.children.push(c);
      return c;
    },
    addEventListener(type, fn) {
      (this.listeners[type] ??= []).push(fn);
    },
    dispatchEvent(ev) {
      for (const fn of this.listeners[ev.type] ?? []) fn(ev);
      return true;
    },
    classList: {
      contains: (c) => el.className.split(/\s+/).includes(c),
    },
    style: {
      __props: Object.create(null),
      setProperty(k, v) {
        this.__props[k] = String(v);
      },
      getPropertyValue(k) {
        return this.__props[k] ?? "";
      },
    },
  };
  if (tag === "canvas") {
    el.width = 0;
    el.height = 0;
    let ops = 0;
    el.getContext = () => ({
      fillStyle: "",
      fillRect() {
        ops++;
      },
    });
  }
  return el;
}

// tag.class selectors over the body tree — all the workloads use.
function matches(el, tag, cls) {
  if (tag && el.tagName !== tag.toUpperCase()) return false;
  if (cls && !el.className.split(/\s+/).includes(cls)) return false;
  return true;
}
function collect(el, tag, cls, out) {
  for (const c of el.children) {
    if (typeof c !== "object") continue;
    if (matches(c, tag, cls)) out.push(c);
    collect(c, tag, cls, out);
  }
  return out;
}

function makeStorage() {
  const map = new Map();
  return {
    setItem: (k, v) => void map.set(String(k), String(v)),
    getItem: (k) => (map.has(String(k)) ? map.get(String(k)) : null),
    removeItem: (k) => void map.delete(String(k)),
    clear: () => map.clear(),
    get length() {
      return map.size;
    },
  };
}

// IndexedDB, spec-shaped where the workloads look: open (upgradeneeded on
// first creation, then success), per-transaction object stores, put/get
// requests completing through their success events on the microtask queue.
function makeIndexedDB() {
  const databases = new Map(); // name -> Map(store -> Map(key -> value))
  const makeReq = () => {
    const listeners = Object.create(null);
    const req = {
      result: undefined,
      addEventListener(type, fn) {
        (listeners[type] ??= []).push(fn);
      },
      __fire(type) {
        queueMicrotask(() => {
          for (const fn of listeners[type] ?? []) fn({ target: req });
        });
      },
    };
    return req;
  };
  return {
    open(name) {
      const req = makeReq();
      const fresh = !databases.has(name);
      if (fresh) databases.set(name, new Map());
      const stores = databases.get(name);
      req.result = {
        createObjectStore(store) {
          stores.set(store, new Map());
          return { name: store };
        },
        transaction(store) {
          const kv = stores.get(store);
          return {
            objectStore() {
              return {
                put(value, key) {
                  kv.set(key, value);
                  const r = makeReq();
                  r.__fire("success");
                  return r;
                },
                get(key) {
                  const r = makeReq();
                  r.result = kv.get(key);
                  r.__fire("success");
                  return r;
                },
              };
            },
          };
        },
        close() {},
      };
      if (fresh) req.__fire("upgradeneeded");
      req.__fire("success");
      return req;
    },
  };
}

const body = makeElement("body");
let nextTimer = 1;
const echoBase = process.env.MERSEY_ECHO_BASE ?? null;

const realm = {
  document: {
    body,
    createElement: (tag) => makeElement(tag),
    querySelector: (sel) => collect(body, ...parseSel(sel), [])[0] ?? null,
    querySelectorAll: (sel) => collect(body, ...parseSel(sel), []),
  },
  localStorage: makeStorage(),
  sessionStorage: makeStorage(),
  Event: globalThis.Event,
  URL: globalThis.URL,
  TextEncoder: globalThis.TextEncoder,
  TextDecoder: globalThis.TextDecoder,
  crypto: globalThis.crypto,
  JSON: globalThis.JSON,
  Uint8Array: globalThis.Uint8Array,
  // Numeric ids like the browser's; nothing here ever fires (the timers
  // workload arms + disarms, measuring registration).
  setTimeout: () => nextTimer++,
  clearTimeout: () => undefined,
  fetch: (path) => {
    if (!echoBase) throw new Error("fetch workload needs MERSEY_ECHO_BASE");
    return globalThis.fetch(new globalThis.URL(path, echoBase));
  },
  // The websocket workload derives its ws:// URL from location.host, like a
  // served page would; WebSocket and ReadableStream are Node's own.
  location: { host: echoBase ? new globalThis.URL(echoBase).host : "" },
  WebSocket: globalThis.WebSocket,
  ReadableStream: globalThis.ReadableStream,
  indexedDB: makeIndexedDB(),
  CompressionStream: globalThis.CompressionStream,
  DecompressionStream: globalThis.DecompressionStream,
  Response: globalThis.Response,
  BroadcastChannel: globalThis.BroadcastChannel,
  Blob: globalThis.Blob,
  URLPattern: globalThis.URLPattern,
  MessageChannel: globalThis.MessageChannel,
  navigator: globalThis.navigator, // Node's own — has the real locks manager
  // Node has no EventSource: fetch the stream and parse SSE blocks, message
  // listeners fired per `data:` block like the browser parser.
  EventSource: class {
    constructor(url) {
      this.__listeners = Object.create(null);
      this.__reader = null;
      this.__closed = false;
      globalThis.fetch(new globalThis.URL(url, echoBase)).then(async (r) => {
        const reader = (this.__reader = r.body.getReader());
        const dec = new TextDecoder();
        let buf = "";
        while (!this.__closed) {
          const { done, value } = await reader.read().catch(() => ({ done: true }));
          if (done) break;
          buf += dec.decode(value, { stream: true });
          let idx;
          while ((idx = buf.indexOf("\n\n")) >= 0) {
            const block = buf.slice(0, idx);
            buf = buf.slice(idx + 2);
            const data = block
              .split("\n")
              .filter((l) => l.startsWith("data: "))
              .map((l) => l.slice(6))
              .join("\n");
            if (data) for (const fn of this.__listeners.message ?? []) fn({ data });
          }
        }
      });
    }
    addEventListener(type, fn) {
      (this.__listeners[type] ??= []).push(fn);
    }
    close() {
      this.__closed = true;
      this.__reader?.cancel().catch(() => {});
    }
  },
  // Node has no DOMMatrix: the 2D subset the geometry workload exercises,
  // exact per the spec's matrix math (post-multiplied translate/scale).
  DOMMatrix: class DOMMatrix {
    constructor() {
      this.a = 1; this.b = 0; this.c = 0; this.d = 1; this.m41 = 0; this.m42 = 0;
    }
    __mul(o) {
      const r = new this.constructor();
      r.a = this.a * o.a + this.c * o.b;
      r.b = this.b * o.a + this.d * o.b;
      r.c = this.a * o.c + this.c * o.d;
      r.d = this.b * o.c + this.d * o.d;
      r.m41 = this.a * o.m41 + this.c * o.m42 + this.m41;
      r.m42 = this.b * o.m41 + this.d * o.m42 + this.m42;
      return r;
    }
    translate(x = 0, y = 0) {
      const t = new this.constructor();
      t.m41 = x; t.m42 = y;
      return this.__mul(t);
    }
    scale(sx = 1, sy) {
      const s = new this.constructor();
      s.a = sx; s.d = sy ?? sx;
      return this.__mul(s);
    }
  },
  // Node has no XMLHttpRequest: a load-event-only XHR over fetch, shaped
  // like the surface the workload touches.
  XMLHttpRequest: class {
    constructor() {
      this.status = 0;
      this.responseText = "";
      this.__listeners = Object.create(null);
    }
    open(method, url) {
      this.__url = url;
    }
    addEventListener(type, fn) {
      (this.__listeners[type] ??= []).push(fn);
    }
    send() {
      globalThis.fetch(new globalThis.URL(this.__url, echoBase)).then(async (r) => {
        this.status = r.status;
        this.responseText = await r.text();
        for (const fn of this.__listeners.load ?? []) fn({ target: this });
      });
    }
  },
  // Node has no Web Worker global: fetch the worker script and run it
  // in-process against a `self` shim, messages delivered on the microtask
  // queue — the script is the real one the browser legs run.
  Worker: class {
    constructor(url) {
      const listeners = (this.__listeners = Object.create(null));
      this.__self = {
        onmessage: null,
        postMessage: (data) =>
          queueMicrotask(() => {
            for (const fn of listeners.message ?? []) fn({ data });
          }),
      };
      this.__ready = globalThis
        .fetch(new globalThis.URL(url, echoBase))
        .then((r) => r.text())
        .then((src) => new Function("self", src)(this.__self));
    }
    addEventListener(type, fn) {
      (this.__listeners[type] ??= []).push(fn);
    }
    postMessage(data) {
      this.__ready.then(() => queueMicrotask(() => this.__self.onmessage?.({ data })));
    }
    terminate() {}
  },
};
function parseSel(sel) {
  const m = /^([a-zA-Z]+)?(?:\.([\w-]+))?$/.exec(sel.trim());
  if (!m) return ["", ""];
  return [m[1] ?? "", m[2] ?? ""];
}

// ---- engine instantiation (the same import table as web/test/harness.mjs) ---

const logs = [];
const errors = [];
const decoder = new TextDecoder();
const encoder = new TextEncoder();
let exports = null;
const mem = () => new Uint8Array(exports.memory.buffer);
const readStr = (p, l) => decoder.decode(mem().subarray(p, p + l));
const writeStr = (s) => {
  const b = encoder.encode(s);
  const p = exports.msy_alloc(b.length);
  mem().set(b, p);
  return [p, b.length];
};
const packed = (s) => {
  const [p, l] = writeStr(s);
  return (BigInt(p) << 32n) | BigInt(l);
};
const bridge = makeBridge(realm, (cb, argsJson) => {
  const [p, l] = writeStr(argsJson);
  exports.msy_invoke_args(cb, p, l);
});
const imports = {
  env: {
    host_print: (p, l) => logs.push(readStr(p, l)),
    host_error: (p, l) => errors.push(readStr(p, l)),
    host_dom_set_text: () => {},
    host_dom_get_text: () => packed(""),
    host_dom_add_listener: () => {},
    host_print_level: (lp, ll, p, l) => logs.push(readStr(p, l)),
    host_random_bytes: () => 0,
    host_dom_create: () => packed(""),
    host_dom_append: () => {},
    host_dom_remove: () => {},
    host_dom_get_value: () => packed(""),
    host_dom_set_value: () => {},
    host_web_global: (np, nl) => BigInt(bridge.global(readStr(np, nl))),
    host_web_get: (t, pp, pl) => packed(bridge.get(Number(t), readStr(pp, pl))),
    host_web_set: (t, pp, pl, vp, vl) =>
      packed(bridge.set(Number(t), readStr(pp, pl), readStr(vp, vl))),
    host_web_call: (t, mp, ml, ap, al) =>
      packed(bridge.call(Number(t), readStr(mp, ml), readStr(ap, al))),
    host_web_new: (cp, cl, ap, al) =>
      packed(bridge.construct(readStr(cp, cl), readStr(ap, al))),
    host_web_intern: (np, nl) => bridge.intern(readStr(np, nl)),
    host_web_get_id: (t, id) => packed(bridge.getId(Number(t), id)),
    host_web_set_str: (t, id, vp, vl) => packed(bridge.setScalar(Number(t), id, readStr(vp, vl))),
    host_web_set_num: (t, id, v) => packed(bridge.setScalar(Number(t), id, v)),
    host_web_call_str: (t, id, ap, al) => packed(bridge.callStr(Number(t), id, readStr(ap, al))),
    host_web_iterate: (t) => packed(bridge.iterate(Number(t))),
    host_web_release: (t) => bridge.release(Number(t)),
    host_web_bytes_read: (t) => {
      const b = bridge.bytesRead(Number(t));
      if (!b) return 0n;
      const ptr = exports.msy_alloc(b.length);
      mem().set(b, ptr);
      return (BigInt(ptr) << 32n) | BigInt(b.length);
    },
    host_web_bytes_write: (ptr, len) =>
      BigInt(bridge.bytesWrite(mem().subarray(Number(ptr), Number(ptr) + Number(len)))),
    host_web_instanceof: (t, c) => bridge.instanceOf(Number(t), Number(c)),
    host_time_ms: (epoch) => (epoch ? Date.now() : performance.now()),
  },
};
({ instance: { exports } } = await WebAssembly.instantiate(wasmBytes, imports));

// ---- run --------------------------------------------------------------------

const arg = process.argv[2];
if (!arg) {
  console.error("usage: node engine-child.mjs <workload>|--blank");
  process.exit(2);
}

let status = 0;
if (arg !== "--blank") {
  const source = await readFile(join(here, "mersey", `${arg}.mersey`), "utf8");
  const [ptr, len] = writeStr(source);
  status = exports.msy_run(ptr, len);
  // Async workloads (fetch) print RESULT from their last callback after
  // msy_run returns; wait for the line, not for the call.
  const deadline = Date.now() + 30000;
  while (!logs.some((l) => l.startsWith("RESULT ")) && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 10));
  }
}
for (const l of logs) console.log(l);
if (errors.length > 0) {
  for (const e of errors) console.error(e);
  process.exit(1);
}

// Peak memory of this child, in KiB. Linux reads VmHWM; macOS reports
// phys_footprint_peak (see host-mem.mjs). The field keeps its historical
// vmhwm= name so the parent's parser is unchanged.
const vmhwm = await selfPeakMemoryKiB();
console.log(`MEMSTAT vmhwm=${vmhwm} wasmheap=${exports.memory.buffer.byteLength}`);
process.exit(status === 0 ? 0 : 1);
