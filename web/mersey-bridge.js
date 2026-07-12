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
export function makeBridge(globalObject, invokeCallback) {
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

  const ok = (value) => JSON.stringify({ ok: encode(value) });
  const err = (e) => JSON.stringify({ err: String(e && e.message ? e.message : e) });

  return {
    global(name) {
      // Ambient globals only: the engine already gates this by import.
      return name in globalObject ? handleFor(globalObject[name]) : -1;
    },
    get(target, prop) {
      try {
        const obj = handles[target];
        if (obj == null) return err(`stale handle ${target}`);
        const v = obj[prop];
        // Methods must stay bound to their receiver.
        return ok(typeof v === "function" ? v.bind(obj) : v);
      } catch (e) {
        return err(e);
      }
    },
    set(target, prop, valueJson) {
      try {
        handles[target][prop] = decode(JSON.parse(valueJson));
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
        const fn = method === "" ? obj : obj[method];
        if (typeof fn !== "function") return err(`${method || "value"} is not a function`);
        return ok(method === "" ? fn(...args) : fn.apply(obj, args));
      } catch (e) {
        return err(e);
      }
    },
    construct(ctorName, argsJson) {
      try {
        const Ctor = globalObject[ctorName];
        if (typeof Ctor !== "function") return err(`${ctorName} is not a constructor`);
        const args = JSON.parse(argsJson).map(decode);
        return ok(new Ctor(...args));
      } catch (e) {
        return err(e);
      }
    },
  };
}
