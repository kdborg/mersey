// Proof that Mersey reaches EVERY web technology at runtime, not just in
// the type system: the universal bridge is pointed at a real JS global
// object (Node's, augmented with DOM/storage/canvas stubs), and a Mersey
// program drives storage, crypto, URL, JSON, canvas, timers, promises/fetch
// and the DOM — none of them hand-wired into the engine.
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { makeBridge } from "../mersey-bridge.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const wasmBytes = await readFile(
  join(root, "target/wasm32-unknown-unknown/release/mersey_wasm.wasm"),
);

// ---- a realistic JS realm (real objects wherever Node has them) ------------
const canvasOps = [];
const makeElement = (tag) => {
  const el = {
    tagName: tag.toUpperCase(),
    textContent: "",
    value: "",
    width: 0,
    height: 0,
    children: [],
    listeners: {},
    appendChild(c) {
      this.children.push(c);
    },
    remove() {
      this.removed = true;
    },
    addEventListener(type, fn) {
      (this.listeners[type] ??= []).push(fn);
    },
    className: "",
    style: {},
    getContext(kind) {
      return {
        canvasKind: kind,
        fillStyle: "",
        fillRect(x, y, w, h) {
          canvasOps.push(`fillRect(${x},${y},${w},${h}) style=${this.fillStyle}`);
        },
      };
    },
  };
  return el;
};

const byId = new Map([
  ["out", makeElement("div")],
]);
const store = new Map();

const paras = [makeElement("p"), makeElement("p")];
const realm = {
  Node: { ELEMENT_NODE: 1, TEXT_NODE: 3 },
  document: {
    body: makeElement("body"),
    getElementById: (id) => byId.get(id) ?? null,
    createElement: (tag) => makeElement(tag),
    querySelector: () => null,
    querySelectorAll: () => paras,
    title: "test realm",
  },
  localStorage: {
    setItem: (k, v) => store.set(k, String(v)),
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    get length() {
      return store.size;
    },
  },
  crypto: globalThis.crypto, // Node's real Web Crypto
  URL: globalThis.URL, // Node's real URL
  JSON: globalThis.JSON, // the real JSON
  Uint8Array: globalThis.Uint8Array, // the real typed array
  setTimeout: globalThis.setTimeout,
  fetch: async (url) => ({ status: 200, ok: true, url, text: async () => "hi" }),
};

// ---- engine + bridge ---------------------------------------------------------
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

const noop = () => {};
const imports = {
  env: {
    host_print: (p, l) => logs.push(readStr(p, l)),
    host_error: (p, l) => errors.push(readStr(p, l)),
    host_dom_set_text: noop,
    host_dom_get_text: () => 0n,
    host_dom_on_click: noop,
    host_dom_create: () => 0n,
    host_dom_append: noop,
    host_dom_remove: noop,
    host_dom_get_value: () => 0n,
    host_dom_set_value: noop,
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
  },
};
({ instance: { exports } } = await WebAssembly.instantiate(wasmBytes, imports));

const source = await readFile(join(root, "web/demo/platform.mersey"), "utf8");
const [ptr, len] = writeStr(source);
const status = exports.msy_run(ptr, len);

// Let the timer and the fetch promise settle.
await new Promise((r) => setTimeout(r, 120));

// ---- assertions ----------------------------------------------------------------
let failures = 0;
const check = (what, cond, detail = "") => {
  console.log(`${cond ? "PASS" : "FAIL"}  ${what}${cond ? "" : `  (${detail})`}`);
  if (!cond) failures++;
};
const logged = (re) => logs.some((l) => re.test(l));

check("engine status 0", status === 0, `status ${status}; ${errors.join("; ")}`);
check("Web Storage (setItem/getItem)", logged(/^storage: visits=1$/), logs.join(" | "));
check("Web Crypto (getRandomValues on a real Uint8Array)",
      logged(/^crypto: 4 random bytes drawn$/), logs.join(" | "));
check("URL API (real URL parsing)",
      logged(/^url: host=example\.com path=\/a\/b query=\?q=mersey$/), logs.join(" | "));
// Record field order is not preserved across the bridge (Mersey records
// are unordered maps) — assert on content, not key order.
check("JSON (platform's own stringify)",
      logged(/^json: \{.*"lang":"mersey".*\}$/) && logged(/^json: \{.*"version":1.*\}$/),
      logs.join(" | "));
check("Canvas 2D (getContext + fillRect)", logged(/^canvas: filled/), logs.join(" | "));
check("canvas op actually reached JS",
      canvasOps.some((o) => o.includes("fillRect(0,0,120,40)") && o.includes("#0af")),
      canvasOps.join(" | "));
check("canvas appended to document.body",
      realm.document.body.children.some((c) => c.tagName === "CANVAS"));
check("Timers (setTimeout fired a Mersey closure)",
      logged(/^timer: fired after 50ms$/), logs.join(" | "));
check("Network (fetch promise resolved into Mersey)",
      logged(/^fetch: status 200$/), logs.join(" | "));
check("interface constants (Node.ELEMENT_NODE)", logged(/^constants: ELEMENT_NODE=1 TEXT_NODE=3$/),
      logs.join(" | "));
check("indexed collections (querySelectorAll[0])", logged(/^nodelist: 2 <p>, first tag=P$/),
      logs.join(" | "));
check("CSS / CSSOM", logged(/^style: color=rebeccapurple class=generated$/), logs.join(" | "));
check("DOM render (textContent aggregated)",
      byId.get("out").textContent.split("\n").length >= 6,
      JSON.stringify(byId.get("out").textContent));

if (failures) {
  console.error(`\n${failures} assertion(s) failed`);
  console.error("logs:", logs);
  console.error("errors:", errors);
  process.exit(1);
}
console.log(`\nWeb platform bridge: all technologies reached from Mersey`);
