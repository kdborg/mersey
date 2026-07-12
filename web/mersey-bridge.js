/* Universal web bridge (shared by the browser loader and the test harness).
 *
 * This is what makes *every* web technology reachable from Mersey: instead
 * of one hand-written host function per API, five reflective operations
 * (global / get / set / call / new) reach any object in the JS realm.
 * Object identity is preserved through a handle table, Mersey closures
 * cross as real JS callbacks, and Promises are handed back as live objects
 * (so `.then(...)` works today, before `async`/`await` lands in the engine).
 *
 * Wire format (tagged JSON, matching crates/mersey_interp/src/webjson.rs):
 *   primitives  -> JSON scalars
 *   host object -> {"__ref__": handle}
 *   Mersey fn   -> {"__cb__": id}   (engine → host direction)
 *   reply       -> {"ok": value} | {"err": "message"}
 */
import { CALLS, GETS, SETS, CTORS } from "./mersey-bindings.gen.js";

export function makeBridge(globalObject, invokeCallback) {
  const realmHTMLElement = () => globalObject.HTMLElement;
  // Generated bindings: resolve (interface, member) → direct thunk once, then
  // cache. Reflection is only the fallback for objects outside the IDL
  // corpus (plain JS objects, cross-realm values).
  const thunkCache = new Map();
  const ifaceNames = (obj) => {
    const names = [];
    for (let p = obj; p && p !== Object.prototype; p = Object.getPrototypeOf(p)) {
      const n = p.constructor && p.constructor.name;
      if (n) names.push(n);
    }
    return names;
  };
  // Escape hatch for A/B measurement (see web/test/bench.mjs).
  const useBindings = !globalObject.__MERSEY_NO_BINDINGS;
  const bound = (obj, prop, table, tag) => {
    if (!useBindings) return null;
    const ctor = obj && obj.constructor ? obj.constructor.name : "";
    const key = `${tag}|${ctor}|${prop}`;
    if (thunkCache.has(key)) return thunkCache.get(key);
    let thunk = null;
    for (const iface of ifaceNames(obj)) {
      const t = table.get(`${iface}.${prop}`);
      if (t) {
        thunk = t;
        break;
      }
    }
    thunkCache.set(key, thunk);
    return thunk;
  };

  const handles = [globalObject]; // handle 0 = the global object
  const byObject = new Map([[globalObject, 0]]);

  const handleFor = (obj) => {
    let h = byObject.get(obj);
    if (h === undefined) {
      h = handles.length;
      handles.push(obj);
      byObject.set(obj, h);
    }
    return h;
  };

  const isPrimitive = (v) =>
    v === null || v === undefined || typeof v === "boolean" ||
    typeof v === "number" || typeof v === "string";

  // JS value -> tagged JSON value
  const encode = (v) => {
    if (v === undefined) return null;
    if (isPrimitive(v)) return v;
    if (typeof v === "bigint") return v.toString();
    return { __ref__: handleFor(v) };
  };

  // tagged JSON value -> JS value (callbacks become real functions)
  const decode = (v) => {
    if (v === null || typeof v !== "object") return v;
    if (Array.isArray(v)) return v.map(decode);
    if ("__ref__" in v) return handles[v.__ref__];
    if ("__cb__" in v) {
      const id = v.__cb__;
      // A Mersey closure: JS calls it with real arguments (event objects,
      // resolved promise values, …), which cross back as handles.
      return (...args) => invokeCallback(id, JSON.stringify(args.map(encode)));
    }
    const out = {};
    for (const [k, val] of Object.entries(v)) out[k] = decode(val);
    return out;
  };

  const encodeAny = (v) => (Array.isArray(v) ? v.map(encode) : encode(v));
  const ok = (value) => JSON.stringify({ ok: encodeAny(value) });
  const err = (e) => JSON.stringify({ err: String(e && e.message ? e.message : e) });

  // Interned member names: a name crosses the boundary once, then it is an
  // integer id — no TextDecoder per call.
  const names = [];
  const nameIds = new Map();
  const OK_NULL = JSON.stringify({ ok: null });

  return {
    intern(name) {
      // Opt out of fast paths for measurement (see web/test/bench.mjs).
      if (globalObject.__MERSEY_NO_FASTPATH) return 0xffffffff;
      let id = nameIds.get(name);
      if (id === undefined) {
        id = names.length;
        names.push(name);
        nameIds.set(name, id);
      }
      return id;
    },
    getId(target, nameId) {
      try {
        const obj = handles[target];
        const prop = names[nameId];
        if (obj == null) return err(`stale handle ${target}`);
        const g = bound(obj, prop, GETS, "g");
        if (g) return ok(g(obj));
        const v = obj[prop];
        return ok(typeof v === "function" ? v.bind(obj) : v);
      } catch (e) {
        return err(e);
      }
    },
    setScalar(target, nameId, value) {
      try {
        const obj = handles[target];
        const prop = names[nameId];
        const s = bound(obj, prop, SETS, "s");
        if (s) s(obj, value);
        else obj[prop] = value;
        return OK_NULL;
      } catch (e) {
        return err(e);
      }
    },
    callStr(target, nameId, arg) {
      try {
        const obj = handles[target];
        const method = names[nameId];
        if (obj == null) return err(`stale handle ${target}`);
        const c = bound(obj, method, CALLS, "c");
        if (c) return ok(c(obj, [arg]));
        const fn = obj[method];
        if (typeof fn !== "function") return err(`${method} is not a function`);
        return ok(fn.call(obj, arg));
      } catch (e) {
        return err(e);
      }
    },
    global(name) {
      // Ambient globals only: the engine already gates this by import.
      return name in globalObject ? handleFor(globalObject[name]) : -1;
    },
    get(target, prop) {
      try {
        const obj = handles[target];
        if (obj == null) return err(`stale handle ${target}`);
        const g = bound(obj, prop, GETS, "g");
        if (g) return ok(g(obj)); // generated binding
        const v = obj[prop]; // fallback: reflection
        return ok(typeof v === "function" ? v.bind(obj) : v);
      } catch (e) {
        return err(e);
      }
    },
    set(target, prop, valueJson) {
      try {
        const obj = handles[target];
        const v = decode(JSON.parse(valueJson));
        const s = bound(obj, prop, SETS, "s");
        if (s) s(obj, v);
        else obj[prop] = v;
        return JSON.stringify({ ok: null });
      } catch (e) {
        return err(e);
      }
    },
    call(target, method, argsJson) {
      try {
        const obj = handles[target];
        if (obj == null) return err(`stale handle ${target}`);
        const args = JSON.parse(argsJson).map(decode);
        // method "" => the handle is itself callable (e.g. imported fetch)
        if (method === "") {
          if (typeof obj !== "function") return err("value is not a function");
          return ok(obj(...args));
        }
        const c = bound(obj, method, CALLS, "c");
        if (c) return ok(c(obj, args)); // generated binding
        const fn = obj[method]; // fallback: reflection
        if (typeof fn !== "function") return err(`${method} is not a function`);
        return ok(fn.apply(obj, args));
      } catch (e) {
        return err(e);
      }
    },
    /// Drop a handle: the object becomes collectable by the JS GC.
    release(target) {
      const obj = handles[target];
      if (obj != null) {
        byObject.delete(obj);
        handles[target] = null;
      }
    },
    /// Snapshot a host iterable as a plain array of encoded values.
    iterate(target) {
      try {
        const obj = handles[target];
        if (obj == null) return err(`stale handle ${target}`);
        let items;
        if (Array.isArray(obj)) items = obj;
        else if (typeof obj[Symbol.iterator] === "function") items = Array.from(obj);
        else if (typeof obj.length === "number") {
          // Array-likes without the iterator protocol (older collections).
          items = Array.prototype.slice.call(obj);
        } else {
          return err("value is not iterable");
        }
        return ok(items);
      } catch (e) {
        return err(e);
      }
    },
    /// Register a custom element whose lifecycle calls back into Mersey.
    /// `handlers` is a decoded record: { connected?, disconnected?,
    /// attributeChanged?, observed? }.
    defineElement(tag, handlers) {
      const h = handlers ?? {};
      const observed = Array.isArray(h.observed) ? h.observed : [];
      class MerseyElement extends realmHTMLElement() {
        static get observedAttributes() {
          return observed;
        }
        connectedCallback() {
          if (h.connected) h.connected(this);
        }
        disconnectedCallback() {
          if (h.disconnected) h.disconnected(this);
        }
        attributeChangedCallback(name, oldV, newV) {
          if (h.attributeChanged) h.attributeChanged(this, name, oldV ?? "", newV ?? "");
        }
      }
      globalObject.customElements.define(tag, MerseyElement);
      return null;
    },
    construct(ctorName, argsJson) {
      try {
        const args = JSON.parse(argsJson).map(decode);
        const c = useBindings ? CTORS.get(ctorName) : null; // generated binding
        if (c) return ok(c(args));
        const Ctor = globalObject[ctorName]; // fallback (e.g. Promise, typed arrays)
        if (typeof Ctor !== "function") return err(`${ctorName} is not a constructor`);
        return ok(new Ctor(...args));
      } catch (e) {
        return err(e);
      }
    },
  };
}
