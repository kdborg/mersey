// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Stage A end-to-end tests without a browser (ROADMAP Phase 5):
//  1. counter demo: load → run → DOM render → click callbacks → re-render
//  2. TODO demo: element creation, tree ops, input value, removal
//  3. the ENTIRE runtime conformance suite executed inside the real WASM
//     engine, compared against the same goldens the native engine uses.
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { makeBridge } from "../mersey-bridge.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const wasmBytes = await readFile(
  join(root, "target/wasm32-unknown-unknown/release/mersey_wasm.wasm"),
);

let failures = 0;
const expect = (what, actual, wanted) => {
  const ok = actual === wanted;
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (!ok) {
    console.log(`      actual:   ${JSON.stringify(actual ?? null).slice(0, 200)}`);
    console.log(`      expected: ${JSON.stringify(wanted ?? null).slice(0, 200)}`);
    failures++;
  }
};

// A fresh engine instance with a stub DOM, mirroring mersey-loader.js.
async function engine() {
  const elements = new Map(); // id -> stub element
  const element = (id) => {
    if (!elements.has(id)) {
      elements.set(id, { textContent: "", value: "", listeners: [], children: [], removed: false });
    }
    return elements.get(id);
  };
  const logs = [];
  const errors = [];
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let exports = null;
  let nextId = 1;
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
  const imports = {
    env: {
      host_print: (p, l) => logs.push(readStr(p, l)),
      host_error: (p, l) => errors.push(readStr(p, l)),
      host_dom_set_text: (ip, il, tp, tl) => {
        element(readStr(ip, il)).textContent = readStr(tp, tl);
      },
      host_dom_get_text: (ip, il) => packed(element(readStr(ip, il)).textContent),
      host_dom_add_listener: (ip, il, ep, el_, cb) => {
        const el = element(readStr(ip, il));
        (el.listenersByEvent ??= {});
        const ev = readStr(ep, el_);
        (el.listenersByEvent[ev] ??= []).push(() => exports.msy_invoke(cb));
        // The existing tests fire `listeners` directly, which means clicks.
        if (ev === "click") el.listeners.push(() => exports.msy_invoke(cb));
      },
      host_print_level: (lp, ll, p, l) => { logs.push(readStr(p, l)); },
      host_random_bytes: () => 0,
      host_dom_create: (tp, tl) => {
        const id = `--mersey-${nextId++}`;
        const el = element(id);
        el.tag = readStr(tp, tl);
        return packed(id);
      },
      host_dom_append: (pp, pl, cp, cl) => {
        element(readStr(pp, pl)).children.push(readStr(cp, cl));
      },
      host_dom_remove: (ip, il) => {
        element(readStr(ip, il)).removed = true;
      },
      host_dom_get_value: (ip, il) => packed(element(readStr(ip, il)).value),
      host_dom_set_value: (ip, il, vp, vl) => {
        element(readStr(ip, il)).value = readStr(vp, vl);
      },
      // Universal bridge (empty realm: these suites use the DOM fast path).
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
  const bridge = makeBridge({}, (cb, argsJson) => {
    const [p, l] = writeStr(argsJson);
    exports.msy_invoke_args(cb, p, l);
  });
  ({ instance: { exports } } = await WebAssembly.instantiate(wasmBytes, imports));
  return {
    run: (source) => {
      const [ptr, len] = writeStr(source);
      return exports.msy_run(ptr, len);
    },
    element,
    elements,
    logs,
    errors,
    click: (id) => element(id).listeners.forEach((f) => f()),
  };
}

// ---- 1. counter demo ---------------------------------------------------------
{
  const e = await engine();
  const status = e.run(await readFile(join(root, "web/demo/app.mersey"), "utf8"));
  expect("counter: run status", status, 0);
  expect("counter: console", e.logs[0], "Mersey is running in your browser 🌊");
  expect("counter: initial", e.element("out").textContent, "Clicks: 0");
  e.click("btn");
  e.click("btn");
  e.click("btn");
  expect("counter: after 3 clicks", e.element("out").textContent, "Clicks: 3");
  e.click("btn");
  e.click("btn");
  expect("counter: UTF-32 wave", e.element("out").textContent, "Clicks: 5 🌊");
}

// ---- 2. TODO demo -------------------------------------------------------------
{
  const e = await engine();
  const status = e.run(await readFile(join(root, "web/demo/todo.mersey"), "utf8"));
  expect("todo: run status", status, 0);
  expect("todo: ready log", e.logs[0], "todo app ready");
  expect("todo: empty state", e.element("count").textContent, "nothing to do 🌊");
  e.element("new-todo").value = "learn mersey";
  e.click("add");
  e.element("new-todo").value = "ship stage B";
  e.click("add");
  expect("todo: two items", e.element("count").textContent, "2 items to do");
  expect("todo: input cleared", e.element("new-todo").value, "");
  const li1 = e.element("list").children[0];
  expect("todo: item text", e.element(li1).textContent, "learn mersey  (click to remove)");
  e.click(li1); // remove first item
  expect("todo: removed", e.element(li1).removed, true);
  expect("todo: one left", e.element("count").textContent, "1 item to do");
}

// ---- 3. runtime conformance suite inside the WASM engine ----------------------
{
  const dir = join(root, "tests/conformance/runtime");
  const cases = (await readdir(dir)).filter((f) => f.endsWith(".mersey")).sort();
  for (const name of cases) {
    const source = await readFile(join(dir, name), "utf8");
    // This runner executes one module at a time (`msy_run`), with no loader.
    // Cases that reach for another file need the module graph, which is what
    // modules.mjs exercises against the real loader — running them here would
    // only prove that a single-module runner cannot load a second module.
    if (/from\s+"\.\.?\//.test(source) || /import\("\.\.?\//.test(source)) continue;
    const golden = await readFile(join(dir, name.replace(/\.mersey$/, ".expect")), "utf8");
    const e = await engine();
    const status = e.run(source);
    let out = e.logs.map((l) => l + "\n").join("");
    if (status === 2 && e.errors.length > 0) {
      out += `runtime error: ${e.errors[e.errors.length - 1]}\n`;
    }
    expect(`conformance in wasm: ${name}`, out, golden);
  }
}

if (failures > 0) {
  console.error(`\n${failures} assertion(s) failed`);
  process.exit(1);
}
console.log("\nStage A end-to-end: all assertions passed");
