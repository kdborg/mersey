// Mersey JS runtime ($rt): the semantics the emitter can't express inline.
// One object, no globals leaked beyond $rt itself. The conformance goldens
// gate everything here against the engine byte for byte.
const $rt = (() => {
  // ---- bigdec: exact decimal (§3.7) — BigInt mantissa + scale --------------
  class BigDec {
    constructor(m, s) { this.m = m; this.s = s; this.$bigdec = 1; }
    static parse(text) {
      // 19.99 | 2.5e-2 | 1e3 — normalized so scale >= 0.
      const m = /^([+-]?)(\d+)(?:\.(\d+))?(?:[eE]([+-]?\d+))?$/.exec(text);
      if (!m) return null;
      const [, sign, whole, frac = "", exp = "0"] = m;
      let mant = BigInt(whole + frac);
      if (sign === "-") mant = -mant;
      let scale = frac.length - Number(exp);
      while (scale < 0) { mant *= 10n; scale++; }
      return new BigDec(mant, scale);
    }
    static align(a, b) {
      let { m: ma, s: sa } = a, { m: mb, s: sb } = b;
      while (sa < sb) { ma *= 10n; sa++; }
      while (sb < sa) { mb *= 10n; sb++; }
      return [ma, mb, sa];
    }
    add(o) { const [a, b, s] = BigDec.align(this, o); return new BigDec(a + b, s); }
    sub(o) { const [a, b, s] = BigDec.align(this, o); return new BigDec(a - b, s); }
    mul(o) { return new BigDec(this.m * o.m, this.s + o.s); }
    div(o) {
      if (o.m === 0n) throw new RangeError("division by zero");
      // Exact or refuse: find the smallest result scale that divides evenly.
      for (let sr = Math.max(0, this.s - o.s); sr <= this.s + o.s + 64; sr++) {
        const num = this.m * 10n ** BigInt(sr + o.s - this.s + (this.s > sr + o.s ? 0 : 0));
        const shift = BigInt(sr + o.s - this.s);
        if (shift < 0n) continue;
        const n = this.m * 10n ** shift;
        if (n % o.m === 0n) return new BigDec(n / o.m, sr);
      }
      throw new RangeError("inexact bigdec division needs a rounding context (§3.7)");
    }
    divide(o, ctx) {
      if (o.m === 0n) throw new RangeError("division by zero");
      const scale = ctx?.scale ?? 0;
      const mode = ctx?.mode ?? "HALF_EVEN";
      const shift = BigInt(scale + o.s - this.s);
      let n = this.m;
      let d = o.m;
      if (shift >= 0n) n *= 10n ** shift;
      else d *= 10n ** -shift;
      if (d < 0n) { n = -n; d = -d; }
      let q = n / d;
      const r = n % d;
      if (r !== 0n) {
        const neg = n < 0n;
        const twice = (neg ? -r : r) * 2n;
        const roundAway = () => { q += neg ? -1n : 1n; };
        switch (mode) {
          case "UP": roundAway(); break;
          case "DOWN": break;
          case "FLOOR": if (neg) q -= 1n; break;
          case "CEILING": if (!neg) q += 1n; break;
          case "HALF_UP": if (twice >= d) roundAway(); break;
          case "HALF_DOWN": if (twice > d) roundAway(); break;
          default: // HALF_EVEN
            if (twice > d || (twice === d && (q % 2n !== 0n))) roundAway();
        }
      }
      return new BigDec(q, scale);
    }
    cmp(o) { const [a, b] = BigDec.align(this, o); return a < b ? -1 : a > b ? 1 : 0; }
    neg() { return new BigDec(-this.m, this.s); }
    abs() { return new BigDec(this.m < 0n ? -this.m : this.m, this.s); }
    toString() {
      const neg = this.m < 0n;
      let digits = (neg ? -this.m : this.m).toString();
      if (this.s === 0) return (neg ? "-" : "") + digits;
      while (digits.length <= this.s) digits = "0" + digits;
      const at = digits.length - this.s;
      return (neg ? "-" : "") + digits.slice(0, at) + "." + digits.slice(at);
    }
  }
  const bigdec = (text) => {
    const v = BigDec.parse(text);
    if (!v) throw new TypeError(`bad bigdec literal \`${text}\``);
    return v;
  };
  const isDec = (v) => typeof v === "object" && v !== null && v.$bigdec === 1;

  // ---- display: the engine's to_display, exactly -------------------------
  const D = (v) => {
    if (v === null || v === undefined) return "null";
    const t = typeof v;
    if (t === "string") return v;
    if (t === "number" || t === "boolean" || t === "bigint") return String(v);
    if (t === "function") return v.$class ? `<class ${v.$class}>` : "<function>";
    if (Array.isArray(v)) return `[${v.map(D).join(", ")}]`;
    if (v instanceof Map) {
      const items = [];
      for (const [k, val] of v) items.push(`${D(k)} => ${D(val)}`);
      return `Map{${items.join(", ")}}`;
    }
    if (v instanceof Set) {
      const items = [];
      for (const x of v) items.push(D(x));
      return `Set{${items.join(", ")}}`;
    }
    if (v instanceof Uint8Array) return `bytes(${v.length})`;
    if (isDec(v)) return v.toString();
    if (v instanceof Error) return `<${v.name}>`;
    const proto = Object.getPrototypeOf(v);
    if (proto === Object.prototype || proto === null) {
      const fields = [];
      for (const k of Object.keys(v)) fields.push(`${k}: ${D(v[k])}`);
      return `{${fields.join(", ")}}`;
    }
    // A class that provides its own toString (the Display protocol) is
    // honoured everywhere a value is shown.
    if (typeof v.toString === "function" && v.toString !== Object.prototype.toString) {
      return v.toString();
    }
    return `<${proto.constructor.name}>`;
  };

  // ---- numbers ------------------------------------------------------------
  const idiv = (a, b) => {
    if (b === 0) throw new RangeError("division by zero");
    return (a / b) | 0;
  };
  const imod = (a, b) => {
    if (b === 0) throw new RangeError("division by zero");
    return a % b | 0;
  };
  const idiv64 = (a, b) => {
    if (b === 0n) throw new RangeError("division by zero");
    return a / b;
  };
  const imod64 = (a, b) => {
    if (b === 0n) throw new RangeError("division by zero");
    return a % b;
  };
  const wI64 = (v) => (typeof v === "bigint" ? BigInt.asIntN(64, v) : BigInt(Math.trunc(v)));
  const wU64 = (v) => (typeof v === "bigint" ? BigInt.asUintN(64, v) : BigInt.asUintN(64, BigInt(Math.trunc(v))));
  const wI32 = (v) => (typeof v === "bigint" ? Number(BigInt.asIntN(32, v)) : v | 0);
  const wU32 = (v) => (typeof v === "bigint" ? Number(BigInt.asUintN(32, v)) : v >>> 0);
  const wI16 = (v) => (wI32(v) << 16) >> 16;
  const wU16 = (v) => wI32(v) & 0xffff;
  const wI8 = (v) => (wI32(v) << 24) >> 24;
  const wU8 = (v) => wI32(v) & 0xff;
  const wF64 = (v) => (typeof v === "bigint" ? Number(v) : v);

  const kindOf = (v) => {
    if (v === null || v === undefined) return "null";
    const t = typeof v;
    if (t === "number") return Number.isInteger(v) ? "int32" : "float64";
    if (t === "bigint") return "int64";
    if (t === "string") return "string";
    if (t === "boolean") return "bool";
    if (t === "function") return "function";
    if (Array.isArray(v)) return "array";
    if (isDec(v)) return "bigdec";
    const p = Object.getPrototypeOf(v);
    if (p === Object.prototype || p === null) return "record";
    return p.constructor.name;
  };

  // Defaults fire on null (Mersey's missing value), not just undefined.
  const dflt = (v, desc) => {
    if (Array.isArray(desc)) {
      const out = v.slice();
      for (let i = 0; i < desc.length; i++) {
        if (desc[i] && (out[i] === null || out[i] === undefined)) out[i] = desc[i]();
      }
      return out;
    }
    const out = { ...v };
    for (const k of Object.keys(desc)) {
      if (desc[k] && (out[k] === null || out[k] === undefined)) out[k] = desc[k]();
    }
    return out;
  };

  const INT_RANGES = {
    int8: [-128, 127], int16: [-32768, 32767], int32: [-2147483648, 2147483647],
    int: [-2147483648, 2147483647],
    uint8: [0, 255], uint16: [0, 65535], uint32: [0, 4294967295],
  };
  // `x as T` — checked (throws RangeError out of range), `wrapping as` wraps.
  const cast = (ty, wrapping, v) => {
    if (typeof v === "string") v = v.codePointAt(0) ?? 0; // char -> code point
    if (isDec(v)) {
      throw new TypeError(`cannot cast bigdec to \`${ty}\``);
    }
    if (ty === "float64") return typeof v === "bigint" ? Number(v) : v;
    if (ty === "float32") return Math.fround(typeof v === "bigint" ? Number(v) : v);
    if (ty === "int64") {
      if (typeof v === "bigint") return BigInt.asIntN(64, v);
      return BigInt.asIntN(64, BigInt(Math.trunc(v)));
    }
    if (ty === "uint64") {
      if (typeof v === "bigint") return BigInt.asUintN(64, v);
      return BigInt.asUintN(64, BigInt(Math.trunc(v)));
    }
    if (typeof v !== "number" && typeof v !== "bigint") {
      throw new TypeError(`cannot cast ${kindOf(v)} to \`${ty}\``);
    }
    let n = typeof v === "bigint" ? Number(v) : v;
    n = Math.trunc(n);
    const [lo, hi] = INT_RANGES[ty];
    if (!wrapping && (n < lo || n > hi)) {
      throw new RangeError(`value does not fit \`${ty}\` (use \`as wrapping\`)`);
    }
    switch (ty) {
      case "int8": return (n << 24) >> 24;
      case "int16": return (n << 16) >> 16;
      case "int32": case "int": return n | 0;
      case "uint8": return n & 0xff;
      case "uint16": return n & 0xffff;
      case "uint32": return n >>> 0;
    }
  };

  // ---- runtime type tests (`is`, checked ref casts, typed catch) ----------
  const classes = new Map();
  const is = (v, ty) => {
    switch (ty) {
      case "string": return typeof v === "string";
      case "bool": return typeof v === "boolean";
      case "char": return typeof v === "string" && [...v].length === 1;
      case "null": return v === null;
      case "unknown": case "JsAny": return true;
      case "int": case "int32": case "int8": case "int16":
      case "uint8": case "uint16": case "uint32":
        return typeof v === "number" && Number.isInteger(v);
      case "int64": case "uint64": case "bigint": return typeof v === "bigint";
      case "float32": case "float64": return typeof v === "number";
      case "bigdec": return isDec(v);
      case "array": return Array.isArray(v);
      case "record": {
        if (v === null || typeof v !== "object") return false;
        const p = Object.getPrototypeOf(v);
        return p === Object.prototype || p === null;
      }
      case "function": return typeof v === "function";
      default: {
        if (ty.endsWith("[]")) {
          const el = ty.slice(0, -2);
          return Array.isArray(v) && v.every((x) => is(x, el));
        }
        const C = classes.get(ty) ?? globalThis[ty];
        return typeof C === "function" && v instanceof C;
      }
    }
  };
  const castRef = (ty, v) => {
    if (ty === "unknown" || is(v, ty)) return v;
    throw new TypeError(`cannot cast ${kindOf(v)} to \`${ty}\``);
  };

  // ---- equality / arithmetic dispatch --------------------------------------
  const eq = (a, b) => {
    if (isDec(a) && isDec(b)) return a.cmp(b) === 0;
    return a === b;
  };
  const add = (a, b) => {
    if (isDec(a)) return a.add(b);
    return a + b;
  };
  const ord = (op, a, b) => {
    if (isDec(a) && isDec(b)) {
      const c = a.cmp(b);
      switch (op) {
        case "<": return c < 0;
        case ">": return c > 0;
        case "<=": return c <= 0;
        case ">=": return c >= 0;
      }
    }
    switch (op) {
      case "<": return a < b;
      case ">": return a > b;
      case "<=": return a <= b;
      case ">=": return a >= b;
    }
  };

  const arith = (op, a, b) => {
    if (isDec(a)) {
      switch (op) {
        case "-": return a.sub(b);
        case "*": return a.mul(b);
        case "/": return a.div(b);
      }
    }
    switch (op) {
      case "-": return a - b;
      case "*": return a * b;
      case "/": return a / b;
      case "%": return a % b;
      case "**": return a ** b;
    }
  };

  // ---- indexing --------------------------------------------------------------
  const index = (o, i) => {
    if (typeof o === "string") {
      if (i < 0 || i >= o.length) {
        throw new RangeError(`index ${i} out of bounds (length ${o.length})`);
      }
      return String.fromCodePoint(o.codePointAt(i));
    }
    if (Array.isArray(o) || o instanceof Uint8Array) {
      if (i < 0 || i >= o.length) {
        throw new RangeError(`index ${i} out of bounds (length ${o.length})`);
      }
      return o[i];
    }
    if (o === null || o === undefined) {
      throw new TypeError("no member on null");
    }
    return o[i];
  };

  // ---- iteration -----------------------------------------------------------
  // Arrays, strings, generators and Maps/Sets iterate natively; a class that
  // implements Iterable<T> hands over its `iter()` generator.
  const iter = (v) => {
    if (v === null || v === undefined) throw new TypeError("value is not iterable");
    if (typeof v === "object" && !(Symbol.iterator in v) && typeof v.iter === "function") {
      return v.iter();
    }
    return v;
  };

  // ---- method dispatch -------------------------------------------------------
  // Mersey's built-in method surface on strings/arrays/maps/sets where it
  // differs from (or does not exist on) the JS native. Everything else calls
  // straight through to the object's own method.
  const resolveAt = (i, len) => (i < 0 ? len + i : i);
  const STR = {
    contains: (s, x) => s.includes(x),
    at: (s, i) => {
      const n = resolveAt(i, s.length);
      if (n < 0 || n >= s.length) return null;
      return String.fromCodePoint(s.codePointAt(n));
    },
    charAt: (s, i) => {
      const n = resolveAt(i, s.length);
      return n < 0 || n >= s.length ? "" : s[n];
    },
    codePointAt: (s, i) => {
      const n = resolveAt(i, s.length);
      if (n < 0 || n >= s.length) return null;
      return s.codePointAt(n);
    },
    split: (s, sep) => (sep === "" ? [...s].map((c) => c) : s.split(sep)),
    toString: (s) => s,
  };
  const ARR = {
    contains: (a, x) => a.includes(x),
    remove: (a, x) => {
      const i = a.indexOf(x);
      if (i >= 0) a.splice(i, 1);
      return i >= 0;
    },
    removeAt: (a, i) => a.splice(i, 1)[0],
    insertAt: (a, i, x) => { a.splice(i, 0, x); },
    clear: (a) => { a.length = 0; },
    first: (a) => (a.length ? a[0] : null),
    last: (a) => (a.length ? a[a.length - 1] : null),
    isEmpty: (a) => a.length === 0,
    clone: (a) => a.slice(),
    take: (a, n) => a.slice(0, n),
    flat: (a) => a.flat(),
    toReversed: (a) => a.slice().reverse(),
    reverseInPlace: (a) => { a.reverse(); },
    fillInPlace: (a, x) => { a.fill(x); },
    toSorted: (a, cmp) =>
      cmp ? a.slice().sort((x, y) => cmp(x, y)) : a.slice().sort(defaultCmp),
    sort: (a, cmp) => {
      if (cmp) a.sort((x, y) => cmp(x, y));
      else a.sort(defaultCmp);
    },
    find: (a, f) => {
      const v = a.find(f);
      return v === undefined ? null : v;
    },
    findIndex: (a, f) => a.findIndex(f),
    pop: (a) => (a.length ? a.pop() : null),
    at: (a, i) => {
      const n = resolveAt(i, a.length);
      return n < 0 || n >= a.length ? null : a[n];
    },
    toString: (a) => D(a),
  };
  const defaultCmp = (x, y) => (x < y ? -1 : x > y ? 1 : 0);
  const MAPM = {
    clear: (m) => { m.clear(); },
    isEmpty: (m) => m.size === 0,
    keys: (m) => [...m.keys()],
    values: (m) => [...m.values()],
    entries: (m) => [...m.entries()].map(([k, v]) => [k, v]),
    remove: (m, k) => m.delete(k),
    get: (m, k) => {
      const v = m.get(k);
      return v === undefined ? null : v;
    },
  };
  const SETM = {
    remove: (s, x) => s.delete(x),
    isEmpty: (s) => s.size === 0,
    values: (s) => [...s.values()],
    clear: (s) => { s.clear(); },
  };
  const call = (obj, name, optional, ...args) => {
    if (obj === null || obj === undefined) {
      if (optional) return null;
      throw new TypeError(`no member \`${name}\` on null`);
    }
    const t = typeof obj;
    let tab = null;
    if (t === "string") tab = STR;
    else if (Array.isArray(obj)) tab = ARR;
    else if (obj instanceof Map) tab = MAPM;
    else if (obj instanceof Set) tab = SETM;
    if (tab) {
      const m = tab[name];
      if (m) return m(obj, ...args);
    }
    const tag = obj[Symbol.toStringTag];
    if (name === "next" && (tag === "Generator" || tag === "AsyncGenerator")) {
      // Mersey's iterator protocol: next() is the value itself, null when done.
      const r = obj.next(...args);
      if (tag === "AsyncGenerator") return r.then((x) => (x.done ? null : x.value ?? null));
      return r.done ? null : r.value ?? null;
    }
    if (isDec(obj) && name === "divide") return obj.divide(...args);
    const f = obj[name];
    if (typeof f !== "function") {
      throw new TypeError(`no method \`${name}\` on ${t === "object" ? D(obj) : t}`);
    }
    return f.apply(obj, args);
  };
  const get = (obj, name, optional) => {
    if (obj === null || obj === undefined) {
      if (optional) return null;
      throw new TypeError(`no member \`${name}\` on null`);
    }
    if (name === "length") {
      if (typeof obj === "string" || Array.isArray(obj) || obj instanceof Uint8Array) {
        return obj.length;
      }
      if (obj instanceof Map || obj instanceof Set) return obj.size;
    }
    if (name === "size" && (obj instanceof Map || obj instanceof Set)) return obj.size;
    const v = obj[name];
    return typeof v === "function" ? v.bind(obj) : v === undefined ? null : v;
  };

  // ---- errors / entry -------------------------------------------------------
  const print = (s) => {
    if (typeof process !== "undefined" && process.stdout) process.stdout.write(s + "\n");
    else console.log(s);
  };
  // Source-mapped rich errors (browser): parse the JS stack, fetch the blob
  // modules it names, decode their inline maps, and render the Mersey stack +
  // a code frame pointing at the erroring expression — the transpiled twin of
  // the engine's own error rendering.
  const VLQ = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const vlqSeg = (s) => {
    const out = [];
    let v = 0, shift = 0;
    for (const ch of s) {
      const d = VLQ.indexOf(ch);
      v |= (d & 31) << shift;
      if (d & 32) { shift += 5; continue; }
      out.push((v >> 1) * ((v & 1) ? -1 : 1));
      v = 0; shift = 0;
    }
    return out;
  };
  const decodeMap = (json) => {
    // Per generated line: [genCol, srcLine, srcCol] triples (absolute, sorted).
    const lines = [];
    let srcLine = 0, srcCol = 0;
    for (const [gl, part] of json.mappings.split(";").entries()) {
      const row = [];
      let genCol = 0;
      for (const segText of part ? part.split(",") : []) {
        const f = vlqSeg(segText);
        if (!f.length) continue;
        genCol += f[0];
        if (f.length >= 4) {
          srcLine += f[2];
          srcCol += f[3];
          row.push([genCol, srcLine + 1, srcCol + 1]);
        }
      }
      lines[gl] = row;
    }
    return { source: json.sources[0], content: json.sourcesContent[0], lines };
  };
  const mapCache = new Map();
  const mapFor = async (url) => {
    if (mapCache.has(url)) return mapCache.get(url);
    let m = null;
    try {
      const text = await (await fetch(url)).text();
      const b64 = /sourceMappingURL=data:application\/json;base64,([A-Za-z0-9+\/=]+)/.exec(text);
      if (b64) m = decodeMap(JSON.parse(atob(b64[1])));
    } catch {}
    mapCache.set(url, m);
    return m;
  };
  const resolveFrame = (map, line, col) => {
    // Nearest mapping at or before (line, col), same line only.
    const row = map.lines[line - 1] || [];
    let best = null;
    for (const [genCol, sl, sc] of row) {
      if (genCol <= col) best = [sl, sc];
    }
    return best ?? (row.length ? [row[0][1], row[0][2]] : null);
  };
  const richError = async (e) => {
    const head = `runtime error: ${e.name}: ${e.message}`;
    const frames = [];
    for (const raw of String(e.stack || "").split("\n")) {
      const m = /at (?:(.+?) \()?((?:blob|https?|file):[^)]+?):(\d+):(\d+)\)?\s*$/.exec(raw);
      if (!m) continue;
      const map = await mapFor(m[2]);
      if (!map) continue;
      const pos = resolveFrame(map, Number(m[3]), Number(m[4]));
      if (!pos) continue;
      frames.push({ name: m[1] || "<module>", source: map, line: pos[0], col: pos[1] });
    }
    if (!frames.length) return null;
    let out = head;
    for (const f of frames) {
      out += `\n    at ${f.name} (${f.source.source}:${f.line}:${f.col})`;
    }
    // Code frame for the innermost mapped frame.
    const top = frames[0];
    const text = (top.source.content || "").split("\n")[top.line - 1];
    if (text !== undefined) {
      const num = String(top.line);
      out += `\n\n  ${num} | ${text}\n  ${" ".repeat(num.length)} | ${" ".repeat(Math.max(0, top.col - 1))}^`;
    }
    return out;
  };
  const uncaught = (e) => {
    const line =
      e instanceof Error
        ? `runtime error: ${e.name}: ${e.message}`
        : `runtime error: uncaught: ${D(e)}`;
    if (typeof process !== "undefined" && process.stderr) {
      process.stderr.write(line + "\n");
      process.exitCode = 2;
    } else if (e instanceof Error && typeof fetch !== "undefined") {
      richError(e).then(
        (rich) => console.error(rich ?? line),
        () => console.error(line),
      );
    } else {
      console.error(line);
    }
  };
  const main = (body) => body().catch(uncaught);

  // ---- std modules -----------------------------------------------------------
  const std_console = {
    console: {
      log: (...a) => print(a.map(D).join(" ")),
      info: (...a) => print(a.map(D).join(" ")),
      warn: (...a) => print(a.map(D).join(" ")),
      error: (...a) => print(a.map(D).join(" ")),
      debug: (...a) => print(a.map(D).join(" ")),
    },
  };
  // round: half away from zero (C, not JS's half-toward-+∞).
  const std_math = {
    math: {
      abs: Math.abs, floor: Math.floor, ceil: Math.ceil, trunc: Math.trunc,
      round: (x) => Math.sign(x) * Math.round(Math.abs(x)),
      sqrt: Math.sqrt, cbrt: Math.cbrt, pow: Math.pow,
      min: Math.min, max: Math.max,
      clamp: (x, lo, hi) => Math.min(Math.max(x, lo), hi),
      sin: Math.sin, cos: Math.cos, tan: Math.tan,
      asin: Math.asin, acos: Math.acos, atan: Math.atan, atan2: Math.atan2,
      log: Math.log, log2: Math.log2, log10: Math.log10, exp: Math.exp,
      hypot: Math.hypot, sign: Math.sign,
      isNaN: (x) => Number.isNaN(x),
      isFinite: (x) => Number.isFinite(x),
      random: Math.random, PI: Math.PI, E: Math.E,
    },
  };
  const std_time = {
    time: {
      now: () => Date.now(),
      monotonic: () => (typeof performance !== "undefined" ? performance.now() : Date.now()),
      format: (ms) => new Date(ms).toISOString(),
      parse: (s) => {
        if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{3})?Z$/.test(s)) return null;
        const v = Date.parse(s);
        return Number.isNaN(v) ? null : v;
      },
      parts: (ms) => {
        const d = new Date(ms);
        return {
          year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate(),
          hour: d.getUTCHours(), minute: d.getUTCMinutes(), second: d.getUTCSeconds(),
          millisecond: d.getUTCMilliseconds(), weekday: d.getUTCDay(),
        };
      },
      fromParts: (p) =>
        Date.UTC(p.year, p.month - 1, p.day, p.hour ?? 0, p.minute ?? 0, p.second ?? 0, p.millisecond ?? 0),
    },
  };
  const std_parse = {
    parse: {
      int: (s, radix) => {
        const r = radix ?? 10;
        const t = s.trim();
        if (t === "" || !/^[+-]?[0-9a-zA-Z]+$/.test(t)) return null;
        const v = parseInt(t, r);
        return Number.isNaN(v) ? null : v;
      },
      float: (s) => {
        const t = s.trim();
        if (t === "") return null;
        const v = Number(t);
        return Number.isNaN(v) ? null : v;
      },
      bool: (s) => (s === "true" ? true : s === "false" ? false : null),
    },
  };
  const std_json = {
    json: {
      stringify: (v) => JSON.stringify(v, (k, x) => (typeof x === "bigint" ? x.toString() : x)),
      parse: (s) => {
        try {
          return JSON.parse(s);
        } catch {
          return null;
        }
      },
    },
  };
  const std_format = {
    format: {
      fixed: (x, d) => x.toFixed(d),
      pad: (s, w) => String(s).padStart(w),
      hex: (n) => n.toString(16),
    },
  };
  let gcLive = 0;
  const std_gc = {
    gc: {
      collect: () => {},
      stats: () => ({ live: (gcLive += 1) }),
      heapSize: () => 0,
    },
  };
  const std_caps = {
    caps: {
      // The conformance runner grants nothing — deny-by-default (§5.3).
      list: () => [],
      has: () => false,
    },
  };
  const std_env = {
    env: {
      get: () => null, // no `env` capability in the conformance runner
      args: () => [],
    },
  };
  const std_fs = {
    fs: {
      readText: () => {
        throw new TypeError("no `read` capability (run with --allow-read)");
      },
    },
  };
  const std_async = {
    Promise: globalThis.Promise,
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
  };
  const utf8enc = new TextEncoder();
  const std_bytes = {
    bytes: {
      alloc: (n) => new Uint8Array(n),
      of: (arr) => Uint8Array.from(arr),
      fill: (b, v) => { b.fill(v); },
      concat: (a, b) => {
        const out = new Uint8Array(a.length + b.length);
        out.set(a);
        out.set(b, a.length);
        return out;
      },
      slice: (b, s, e) => b.slice(s, e),
      toArray: (b) => [...b],
      encodeUtf8: (s) => utf8enc.encode(s),
      fromHost: (x) => new Uint8Array(x.buffer.slice(x.byteOffset, x.byteOffset + x.byteLength)),
      toHost: (b) => new Uint8ClampedArray(b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength)),
      decodeUtf8: (b) => {
        try {
          return new TextDecoder("utf-8", { fatal: true }).decode(b);
        } catch {
          return null; // invalid UTF-8 -> null, not U+FFFD
        }
      },
    },
  };
  // std:regex — Mersey's regex surface over the JS engine.
  const mkMatch = (m) => ({
    text: m[0],
    start: m.index,
    end: m.index + m[0].length,
    groups: m.slice(1).map((g) => (g === undefined ? null : g)),
  });
  class MerseyRegex {
    constructor(pattern, flags) {
      this.re = new RegExp(pattern, (flags ?? "") + "g");
    }
    test(s) {
      this.re.lastIndex = 0;
      return this.re.test(s);
    }
    find(s) {
      this.re.lastIndex = 0;
      const m = this.re.exec(s);
      return m ? mkMatch(m) : null;
    }
    findAll(s) {
      this.re.lastIndex = 0;
      const out = [];
      let m;
      while ((m = this.re.exec(s))) {
        out.push(mkMatch(m));
        if (m[0] === "") this.re.lastIndex++;
      }
      return out;
    }
    replace(s, w) {
      const one = new RegExp(this.re.source, this.re.flags.replace("g", ""));
      return s.replace(one, w);
    }
    replaceAll(s, w) {
      this.re.lastIndex = 0;
      return s.replaceAll(this.re, w);
    }
    split(s) {
      return s.split(this.re);
    }
  }
  const std_regex = { regex: { compile: (p, f) => new MerseyRegex(p, f) } };

  const needCap = (name) => {
    const allow = globalThis.__merseyAllow;
    if (!allow || !allow.has(name)) {
      throw new TypeError(`no \`${name}\` capability (run with --allow-${name})`);
    }
  };
  const std_random = {
    random: {
      float: () => {
        needCap("random");
        const u = new Uint32Array(2);
        crypto.getRandomValues(u);
        // 53 random bits -> [0, 1)
        return (u[0] * 0x200000 + (u[1] >>> 11)) / 0x20000000000000;
      },
      int: (lo, hi) => {
        needCap("random");
        const l = BigInt(lo), h = BigInt(hi);
        const span = h - l + 1n;
        const u = new BigUint64Array(1);
        crypto.getRandomValues(u);
        return l + (u[0] % span);
      },
      bytes: (n) => {
        needCap("random");
        const b = new Uint8Array(n);
        crypto.getRandomValues(b);
        return b;
      },
    },
  };
  const std = {
    console: std_console,
    random: std_random, math: std_math, time: std_time, parse: std_parse,
    json: std_json, format: std_format, gc: std_gc, caps: std_caps,
    env: std_env, fs: std_fs, async: std_async, bytes: std_bytes,
    regex: std_regex,
    // std:result is written in Mersey and needs the module loader — absent
    // here exactly as in the engine's single-module runner.
  };

  // browser:dom — bind the real global, or a "not defined" thrower so the
  // error surfaces at USE, not import (feature detection, like the engine).
  // A constructable stand-in for a host class: JS refuses to `new` a real
  // host subclass outside custom-element upgrade, so transpiled classes
  // extend this instead — its prototype chains to the host's, and `attach`
  // welds instances onto real host objects.
  const hostBase = (HostClass) => {
    class Base {}
    if (HostClass && HostClass.prototype) {
      Object.setPrototypeOf(Base.prototype, HostClass.prototype);
    }
    return Base;
  };

  const WEB_NATIVES = {
    // Handles are GC'd by the JS engine itself; release is free.
    release: () => {},
    // Weld a Mersey instance onto a host object: the object takes the
    // instance's class chain (whose tail reaches the host prototype via
    // hostBase) and its fields, and IS the instance from here on — the
    // transpiled twin of the engine's web.attach, which sets instance.host.
    attach: (instance, el) => {
      Object.setPrototypeOf(el, Object.getPrototypeOf(instance));
      for (const k of Object.keys(instance)) el[k] = instance[k];
      return el;
    },
    performance: typeof performance !== "undefined" ? performance : undefined,
    // Custom Elements: Mersey can't subclass a host class, so the runtime
    // builds the JS class and calls the handler closures directly — the
    // transpiled twin of mersey-bridge.js's defineElement (which does the
    // same for the engine path, forwarding through the bridge).
    merseyDefineElement:
      typeof customElements !== "undefined" && typeof HTMLElement !== "undefined"
        ? (tag, handlers) => {
            const h = handlers ?? {};
            const observed = Array.isArray(h.observed) ? h.observed : [];
            class MerseyElement extends HTMLElement {
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
            customElements.define(tag, MerseyElement);
            return null;
          }
        : undefined,
  };
  const web = (name) => {
    if (name in WEB_NATIVES && WEB_NATIVES[name] !== undefined) return WEB_NATIVES[name];
    if (name in globalThis) return globalThis[name];
    return new Proxy(function () {}, {
      get() { throw new TypeError(`\`${name}\` is not defined`); },
      apply() { throw new TypeError(`\`${name}\` is not defined`); },
      construct() { throw new TypeError(`\`${name}\` is not defined`); },
    });
  };

  // The WASM compute tier: decode, instantiate, and wrap the exports so the
  // boundary matches Mersey — unsigned results re-normalized, the only
  // possible trap (integer division by zero) rethrown as the engine's error.
  const wasm = async (b64, sigs) => {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    const { instance } = await WebAssembly.instantiate(bytes);
    const out = {};
    for (const [name, params, ret] of sigs) {
      const f = instance.exports[name];
      out[name] = (...a) => {
        try {
          const r = f(...a);
          if (ret === "u32") return r >>> 0;
          if (ret === "u64") return BigInt.asUintN(64, r);
          return ret === null ? null : r;
        } catch (e) {
          if (e instanceof WebAssembly.RuntimeError) {
            throw new RangeError("division by zero");
          }
          throw e;
        }
      };
    }
    return out;
  };

  const dynImport = (spec) => {
    // A literal specifier was already rewritten to its blob URL by the loader;
    // anything else resolves through the published module map.
    if (spec.startsWith("blob:")) return import(spec);
    const map = globalThis.__merseyModUrls;
    const url = map && map[spec];
    if (!url) throw new TypeError(`module \`${spec}\` was not loaded`);
    return import(url);
  };

  const stdGet = (mod) => {
    const m = std[mod];
    if (!m) {
      throw new TypeError(`module \`std:${mod}\` was not loaded (resolved to \`std:${mod}\`)`);
    }
    return m;
  };

  return {
    D, idiv, imod, idiv64, imod64,
    wI64, wU64, wI32, wU32, wI16, wU16, wI8, wU8, wF64,
    cast, castRef, is, eq, add, arith, ord, index, iter, call, get, dflt, kindOf,
    classes, std: new Proxy(std, { get: (t, k) => stdGet(k) }),
    web, bigdec, dynImport, wasm, main, uncaught, print, hostBase,
  };
})();
