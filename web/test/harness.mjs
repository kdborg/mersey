// End-to-end test of the Stage A stack without a browser: loads the real
// mersey_wasm.wasm with the same import surface as mersey-loader.js, backed
// by a stub DOM, runs demo/app.mersey, simulates clicks, and asserts on the
// results. Run via web/build-and-test.sh (or: node web/test/harness.mjs).
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

// ---- stub DOM ---------------------------------------------------------------
const elements = new Map(); // id -> { textContent, listeners: [] }
const element = (id) => {
  if (!elements.has(id)) elements.set(id, { textContent: "", listeners: [] });
  return elements.get(id);
};
element("out");
element("btn");

const logs = [];
const errors = [];

// ---- the same ABI the browser loader implements ------------------------------
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

const imports = {
  env: {
    host_print: (p, l) => logs.push(readStr(p, l)),
    host_error: (p, l) => errors.push(readStr(p, l)),
    host_dom_set_text: (ip, il, tp, tl) => {
      element(readStr(ip, il)).textContent = readStr(tp, tl);
    },
    host_dom_get_text: (ip, il) => {
      const el = elements.get(readStr(ip, il));
      if (!el) return 0n;
      const [p, l] = writeStr(el.textContent);
      return (BigInt(p) << 32n) | BigInt(l);
    },
    host_dom_on_click: (ip, il, cb) => {
      element(readStr(ip, il)).listeners.push(() => exports.msy_invoke(cb));
    },
  },
};

// ---- run --------------------------------------------------------------------
const wasmBytes = await readFile(
  join(root, "target/wasm32-unknown-unknown/release/mersey_wasm.wasm"),
);
({ instance: { exports } } = await WebAssembly.instantiate(wasmBytes, imports));

const source = await readFile(join(root, "web/demo/app.mersey"), "utf8");
const [ptr, len] = writeStr(source);
const status = exports.msy_run(ptr, len);

const click = () => element("btn").listeners.forEach((fn) => fn());

let failures = 0;
const expect = (what, actual, wanted) => {
  const ok = actual === wanted;
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}: ${JSON.stringify(actual)}`);
  if (!ok) {
    console.log(`      expected ${JSON.stringify(wanted)}`);
    failures++;
  }
};

expect("msy_run status", status, 0);
expect("engine errors", errors.join("; "), "");
expect("console output", logs[0], "Mersey is running in your browser 🌊");
expect("initial render", element("out").textContent, "Clicks: 0");
click();
click();
click();
expect("after 3 clicks", element("out").textContent, "Clicks: 3");
click();
click();
expect("after 5 clicks (UTF-32 wave)", element("out").textContent, "Clicks: 5 🌊");

if (failures > 0) {
  console.error(`\n${failures} assertion(s) failed`);
  process.exit(1);
}
console.log("\nStage A end-to-end: all assertions passed");
