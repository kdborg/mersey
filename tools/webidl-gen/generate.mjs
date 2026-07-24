// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// WebIDL → Mersey ambient declarations (browser-integration.md step 4,
// brought forward to Stage A). Consumes @webref/idl (the machine-readable
// standardized web platform) and emits:
//   crates/mersey_front/src/webapi.gen.mersey
// containing `interface`/`type` declarations for every standardized
// interface, dictionary, typedef, enum and callback, plus `let name: Type;`
// ambient globals harvested from the Window interface.
//
// Type mapping notes (v1, recorded in the file header):
//  - integers (octet..unsigned long) → int32; 64-bit + floats → float64
//  - any/object/unmappable → JsAny (the checker treats it as `unknown`: a
//    sound top type, so it must be narrowed or cast before it can be used)
//  - overloads: first wins; params are renamed p0..pN; statics/ctors skipped
//  - special ops (indexed getters, iterable<>, maplike) skipped
//
// Standards-status filtering: @mdn/browser-compat-data (the dataset behind
// MDN's Deprecated/Non-standard badges) gates every interface, member,
// namespace, global and CSS property. Anything BCD marks deprecated or
// off the standard track is dropped (experimental is kept; a missing BCD
// entry keeps the item — a lookup miss can only over-keep, never over-drop).
// The full drop list is written to dropped.txt beside this script.
import idl from "@webref/idl";
import css from "@webref/css";
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";

const bcd = createRequire(import.meta.url)("@mdn/browser-compat-data");

// ---- BCD status filtering ---------------------------------------------------

const bcdApi = bcd.api ?? {};
const statusOf = (e) => e?.__compat?.status ?? null;
const isDropped = (st) => !!st && (st.deprecated === true || st.standard_track === false);
const reason = (st) => (st.deprecated ? "deprecated" : "non-standard");

// Member lookup on interface/namespace `iface`. Static members check BCD's
// split `<name>_static` key first; IDL `on<foo>` handler attributes have no
// `on<foo>` BCD key (migrated) — fall back to `<foo>_event`. No entry → null
// (→ keep).
function memberStatus(iface, name, { isStatic = false } = {}) {
  const c = bcdApi[iface];
  if (!c) return null;
  if (isStatic && c[`${name}_static`]) return statusOf(c[`${name}_static`]);
  if (c[name]) return statusOf(c[name]);
  const ev = /^on(.+)$/.exec(name);
  if (ev && c[`${ev[1]}_event`]) return statusOf(c[`${ev[1]}_event`]);
  return null;
}
const interfaceStatus = (name) => statusOf(bcdApi[name]); // also namespaces (api.console)
const ctorStatus = (name) => statusOf(bcdApi[name]?.[name]); // bcd.api.<Iface>.<Iface>

// Window/worker-harvested globals: BCD keeps some under the scope interface
// but moved many (setTimeout, fetch, …) to top-level api.<name> entries.
function globalStatus(name) {
  for (const scope of ["Window", "DedicatedWorkerGlobalScope", "WorkerGlobalScope"]) {
    const st = memberStatus(scope, name);
    if (st) return st;
  }
  return statusOf(bcdApi[name]);
}
const cssPropStatus = (p) => statusOf(bcd.css?.properties?.[p]); // raw kebab name

const dropped = new Set(); // report lines for dropped.txt (deduped: merged IDL repeats members)
const recordDrop = (kind, what, st) => dropped.add(`${kind} ${what} (${reason(st)})`);

const files = await idl.parseAll();

// CSS properties are defined by the CSS specs, not by WebIDL: pull them
// from @webref/css and attach them to CSSStyleProperties (camelCased, as
// the CSSOM requires) so `el.style.backgroundColor = …` type-checks.
const cssAll = await css.listAll();
const cssProps = new Set();
for (const spec of Object.values(cssAll)) {
  for (const d of Object.values(spec)) {
    if (d && d.initial && /^[a-z-]+$/.test(d.name) && !d.name.startsWith("--")) {
      cssProps.add(d.name);
    }
  }
}
const camel = (p) => p.replace(/^-/, "").replace(/-([a-z])/g, (_, c) => c.toUpperCase());

const interfaces = new Map(); // name -> { members: [], inheritance, isMixin }
const dictionaries = new Map();
const typedefs = new Map();
const enums = new Set();
const callbacks = new Map();
const includes = []; // {target, mixin}
const namespaces = new Map();

for (const ast of Object.values(files)) {
  for (const def of ast) {
    switch (def.type) {
      case "interface":
      case "interface mixin": {
        const key = def.name;
        if (!interfaces.has(key)) {
          interfaces.set(key, {
            members: [],
            inheritance: def.inheritance ?? null,
            isMixin: def.type === "interface mixin",
          });
        }
        const rec = interfaces.get(key);
        rec.inheritance ??= def.inheritance ?? null;
        rec.members.push(...def.members);
        break;
      }
      case "dictionary": {
        if (!dictionaries.has(def.name)) {
          dictionaries.set(def.name, { fields: [], inheritance: def.inheritance ?? null });
        }
        const rec = dictionaries.get(def.name);
        rec.inheritance ??= def.inheritance ?? null;
        rec.fields.push(...def.members);
        break;
      }
      case "typedef":
        typedefs.set(def.name, def.idlType);
        break;
      case "enum":
        enums.add(def.name);
        break;
      case "callback":
        callbacks.set(def.name, def);
        break;
      case "callback interface": {
        // e.g. EventListener: single-op callback — model as function type.
        const op = def.members.find((m) => m.type === "operation");
        if (op) callbacks.set(def.name, { arguments: op.arguments, idlType: op.idlType });
        break;
      }
      case "includes":
        includes.push({ target: def.target, mixin: def.includes });
        break;
      case "namespace": {
        if (!namespaces.has(def.name)) namespaces.set(def.name, []);
        namespaces.get(def.name).push(...def.members);
        break;
      }
    }
  }
}

// Whole-interface drops (BCD deprecated / non-standard). Mixins are never
// status-checked directly (BCD has no mixin entries; their members are
// checked under the concrete target after folding, matching BCD's layout).
const droppedInterfaces = new Set();
for (const [name, rec] of interfaces) {
  if (rec.isMixin) continue;
  const st = interfaceStatus(name);
  if (isDropped(st)) {
    droppedInterfaces.add(name);
    recordDrop("interface", name, st);
  }
}
// Inheritance guard: never break an `extends` — if a kept interface inherits
// from a dropped one, un-drop the base (zero cases in the current corpus;
// this protects against BCD/webref drift).
for (const [name, rec] of interfaces) {
  if (rec.isMixin || droppedInterfaces.has(name)) continue;
  for (let base = rec.inheritance; base; base = interfaces.get(base)?.inheritance) {
    if (droppedInterfaces.has(base)) {
      droppedInterfaces.delete(base);
      for (const l of dropped) {
        if (l.startsWith(`interface ${base} (`)) dropped.delete(l);
      }
      dropped.add(`kept-as-base ${base} (required by ${name})`);
    }
  }
}

// Fold mixins into their targets.
const mixinInto = new Map();
for (const { target, mixin } of includes) {
  if (!mixinInto.has(target)) mixinInto.set(target, []);
  mixinInto.get(target).push(mixin);
}
function allMembers(name, seen = new Set()) {
  if (seen.has(name)) return [];
  seen.add(name);
  const rec = interfaces.get(name);
  if (!rec) return [];
  let out = [...rec.members];
  for (const m of mixinInto.get(name) ?? []) out = out.concat(allMembers(m, seen));
  return out;
}

// ---- type mapping -----------------------------------------------------------

const INT = new Set(["octet", "byte", "short", "unsigned short", "long", "unsigned long"]);
const FLOAT = new Set([
  "long long",
  "unsigned long long",
  "float",
  "double",
  "unrestricted float",
  "unrestricted double",
]);
const STR = new Set(["DOMString", "USVString", "ByteString", "CSSOMString"]);

function mapType(t, depth = 0) {
  if (!t || depth > 6) return "JsAny";
  if (t.union) {
    const parts = t.idlType.map((x) => mapType(x, depth + 1));
    if (parts.includes("JsAny")) return t.nullable ? "JsAny" : "JsAny";
    const u = `(${[...new Set(parts)].join(" | ")})`;
    return t.nullable ? `${u}?` : u;
  }
  if (t.generic === "sequence" || t.generic === "FrozenArray" || t.generic === "ObservableArray") {
    const inner = mapType(t.idlType[0], depth + 1);
    const arr = inner.includes("|") || inner.endsWith("?") ? `(${inner})[]` : `${inner}[]`;
    return t.nullable ? `${arr}?` : arr;
  }
  if (t.generic === "Promise") {
    let inner = mapType(t.idlType[0], depth + 1);
    if (inner === "void") inner = "JsAny";
    return `Promise<${inner}>`;
  }
  if (t.generic === "record") return "JsAny";
  const base = typeof t.idlType === "string" ? t.idlType : null;
  if (!base) return "JsAny";
  let out;
  if (INT.has(base)) out = "int32";
  else if (FLOAT.has(base)) out = "float64";
  else if (STR.has(base)) out = "string";
  else if (base === "boolean") out = "bool";
  else if (base === "undefined" || base === "void") out = "void";
  else if (base === "any" || base === "object") out = "JsAny";
  else if (base === "bigint") out = "bigint";
  else if (interfaces.has(base) && droppedInterfaces.has(base)) {
    // A kept signature referencing a dropped type: erase to JsAny so the
    // generated file still parses/binds with no orphan-type declarations.
    dropped.add(`type-ref ${base} -> JsAny`);
    out = "JsAny";
  } else if (
    interfaces.has(base) ||
    dictionaries.has(base) ||
    typedefs.has(base) ||
    callbacks.has(base) ||
    // Typed arrays, DataView, BufferSource: WebIDL *references* these but does
    // not define them (they are ECMAScript), so they are not in the corpus and
    // used to fall through to `JsAny`. They are declared in ES_AMBIENT below,
    // so the name resolves — `ImageData.data` is a `Uint8ClampedArray`, and the
    // IDL knew that all along.
    ES_DEFINED.has(base)
  )
    out = base;
  else if (enums.has(base)) out = "string";
  else out = "JsAny";
  if (t.nullable && out !== "JsAny" && out !== "void") out = `${out}?`;
  return out;
}

const IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;
const validName = (n) => typeof n === "string" && IDENT.test(n);

function mapArgs(args) {
  const parts = [];
  for (let i = 0; i < (args ?? []).length; i++) {
    const a = args[i];
    let ty = mapType(a.idlType);
    if (ty === "void") ty = "JsAny";
    if (a.variadic) {
      const arr = ty.includes("|") || ty.endsWith("?") ? `(${ty})` : ty;
      parts.push(`...p${i}: ${arr}[]`);
    } else {
      parts.push(`p${i}${a.optional ? "?" : ""}: ${ty}`);
    }
  }
  return parts.join(", ");
}

// ---- emission ----------------------------------------------------------------

const RESERVED_TYPE_NAMES = new Set([
  // collide with Mersey builtins/prelude or ES_AMBIENT — keep ours
  "Map", "Set", "Error", "RangeError", "TypeError", "JSON", "Math",
  "Uint8Array", "Int8Array", "Uint16Array", "Int16Array", "Uint32Array",
  "Int32Array", "Float32Array", "Float64Array", "Uint8ClampedArray",
  "BigInt64Array", "BigUint64Array", "Float16Array", "DataView", "ArrayBufferView",
  "BufferSource", "ArrayBuffer", "Promise", "JsAny",
  // ECMA-402. `new Intl.NumberFormat(…)` is a *namespaced* constructor: the
  // type is written with a dot, and both the checker and the bridge resolve it
  // by walking the path.
  "Intl", "IntlNumberFormat", "IntlDateTimeFormat", "IntlCollator",
]);

// ECMAScript-defined types that WebIDL references but doesn't define
// (typed arrays, JSON, Math…): declared so IDL signatures resolve and the
// bridge can reach them.
/// ECMAScript types that WebIDL references but does not define. Declared in
/// ES_AMBIENT, and therefore mappable by name rather than to `JsAny`.
const ES_DEFINED = new Set([
  "Uint8Array", "Int8Array", "Uint16Array", "Int16Array", "Uint32Array",
  "Int32Array", "Float32Array", "Float64Array", "Uint8ClampedArray",
  "BigInt64Array", "BigUint64Array", "Float16Array", "DataView",
  "ArrayBufferView", "BufferSource", "ArrayBuffer",
]);

const TYPED_ARRAYS = [
  "Uint8Array", "Int8Array", "Uint16Array", "Int16Array", "Uint32Array",
  "Int32Array", "Float32Array", "Float64Array", "Uint8ClampedArray",
  "BigInt64Array", "BigUint64Array", "Float16Array", "DataView",
];
const ES_AMBIENT = `interface ArrayBuffer { readonly byteLength: int32; }
interface ArrayBufferView { readonly byteLength: int32; }
interface BufferSource extends ArrayBufferView {}
${TYPED_ARRAYS.map(
  (t) => `interface ${t} extends BufferSource { readonly length: int32; }`,
).join("\n")}
interface __JSON {
    stringify(p0: JsAny, p1?: JsAny, p2?: JsAny): string;
    parse(p0: string): JsAny;
}
let JSON: __JSON;
interface IntlNumberFormat {
    format(p0: float64): string;
}
interface IntlDateTimeFormat {
    format(p0: float64): string;
}
interface IntlCollator {
    compare(p0: string, p1: string): int32;
}
interface __Intl {
    readonly NumberFormat: JsAny;
    readonly DateTimeFormat: JsAny;
    readonly Collator: JsAny;
}
let Intl: __Intl;
interface __Math {
    readonly PI: float64;
    random(): float64;
    floor(p0: float64): float64;
    abs(p0: float64): float64;
}
let Math: __Math;
// Custom Elements: Mersey classes cannot extend a host class, so the loader
// builds the JS class and forwards the lifecycle into these handlers.
type CustomElementHandlers = {
    connected?: (el: Element) => void,
    disconnected?: (el: Element) => void,
    attributeChanged?: (el: Element, name: string, oldValue: string, newValue: string) => void,
    observed?: string[]
};
let merseyDefineElement: (tag: string, handlers: CustomElementHandlers) => void;
// postMessage differs between Window (message, targetOrigin) and a worker
// scope (message): declare the permissive form so one program runs on both.
let postMessage: (message: JsAny, p1?: JsAny) => void;
// Engine helper (not IDL): hand a host object back, so long-lived pages that
// churn through DOM objects do not retain handles forever.
let release: (obj: JsAny) => void;
// Engine helper (not IDL): bind a Mersey instance of a host-backed class to
// an existing host object (the browser constructs custom elements, so the
// instance is attached rather than constructed).
let attach: (instance: JsAny, host: JsAny) => JsAny;
`;

let out = `// GENERATED by tools/webidl-gen/generate.mjs from @webref/idl — do not edit.
// Ambient web-platform declarations: every standardized interface, typed.
// APIs marked deprecated or non-standard by @mdn/browser-compat-data are
// excluded (experimental kept); see tools/webidl-gen/dropped.txt.
// JsAny stands for IDL any/object (checker maps it to \`any\`).
interface JsAny {}
interface Promise<T> {
    then(p0: (value: T) => void, p1?: (reason: JsAny) => void): Promise<JsAny>;
    catch(p0: (reason: JsAny) => void): Promise<JsAny>;
}
${ES_AMBIENT}`;

let counts = { interfaces: 0, members: 0, dicts: 0, typedefs: 0, callbacks: 0, globals: 0 };
const staticGlobals = [];

// Direct binding thunks (one per member) — emitted as JS so the bridge never
// does reflective property lookup. Stage B feeds the same tables to Blink's
// native bindings; only the emitter's backend changes.
const bindCalls = []; // ["Iface.member", "(t,a)=>t.member(a[0],a[1])"]
const bindGets = [];
const bindSets = [];
const bindCtors = [];
const argList = (n) => Array.from({ length: n }, (_, i) => `a[${i}]`).join(",");

// typedefs first (order-independent anyway, but keep readable)
for (const [name, t] of typedefs) {
  if (RESERVED_TYPE_NAMES.has(name) || name === "Function") continue;
  let ty = mapType(t);
  if (ty === "void") ty = "JsAny";
  if (ty === name) ty = "JsAny"; // self-referential typedef
  out += `type ${name} = ${ty};\n`;
  counts.typedefs++;
}
for (const name of enums) {
  if (!typedefs.has(name) && !RESERVED_TYPE_NAMES.has(name)) {
    out += `type ${name} = string;\n`;
    counts.typedefs++;
  }
}
for (const [name, cb] of callbacks) {
  if (RESERVED_TYPE_NAMES.has(name)) continue;
  let ret = mapType(cb.idlType);
  if (ret !== "void" && ret === name) ret = "JsAny";
  out += `type ${name} = (${mapArgs(cb.arguments)}) => ${ret};\n`;
  counts.callbacks++;
}
for (const [name, d] of dictionaries) {
  if (RESERVED_TYPE_NAMES.has(name)) continue;
  // Flatten inheritance chain.
  const fields = [];
  const seenF = new Set();
  let cur = name;
  const seenD = new Set();
  while (cur && dictionaries.has(cur) && !seenD.has(cur)) {
    seenD.add(cur);
    for (const f of dictionaries.get(cur).fields) {
      if (f.type !== "field" || seenF.has(f.name) || !validName(f.name)) continue;
      seenF.add(f.name);
      let ty = mapType(f.idlType);
      if (ty === "void") ty = "JsAny";
      ty = ty.endsWith("?") ? ty.slice(0, -1) : ty; // optionality via `?:`
      fields.push(`${f.name}?: ${ty}`);
    }
    cur = dictionaries.get(cur).inheritance;
  }
  out += fields.length
    ? `type ${name} = { ${fields.join(", ")} };\n`
    : `type ${name} = JsAny;\n`;
  counts.dicts++;
}

for (const [name, rec] of interfaces) {
  if (rec.isMixin || RESERVED_TYPE_NAMES.has(name) || droppedInterfaces.has(name)) continue;
  const parent =
    rec.inheritance && interfaces.has(rec.inheritance) && !interfaces.get(rec.inheritance).isMixin
      ? ` extends ${rec.inheritance}`
      : "";
  const seen = new Set();
  const lines = [];
  for (const m of allMembers(name)) {
    if (m.type === "attribute" && m.special !== "static") {
      if (seen.has(m.name) || m.name === "constructor" || !validName(m.name)) continue;
      const st = memberStatus(name, m.name);
      if (isDropped(st)) {
        recordDrop("member", `${name}.${m.name}`, st);
        continue;
      }
      seen.add(m.name);
      let ty = mapType(m.idlType);
      if (ty === "void") ty = "JsAny";
      lines.push(`    ${m.readonly ? "readonly " : ""}${m.name}: ${ty};`);
      bindGets.push([`${name}.${m.name}`, `(t)=>t.${m.name}`]);
      if (!m.readonly) {
        bindSets.push([`${name}.${m.name}`, `(t,v)=>{t.${m.name}=v;}`]);
      }
      counts.members++;
      counts.bindings = (counts.bindings ?? 0) + 1;
    } else if (m.type === "operation" && m.name) {
      // Named special operations (getter getItem, setter setItem, …) are
      // ordinary callable members too; only anonymous specials are skipped
      // (no m.name) and statics need a static surface we don't emit yet.
      if (seen.has(m.name) || m.name === "constructor" || !validName(m.name)) continue;
      if (m.special === "static") continue;
      const st = memberStatus(name, m.name);
      if (isDropped(st)) {
        recordDrop("member", `${name}.${m.name}`, st);
        continue;
      }
      seen.add(m.name);

      // Overloads: emit each one. Mersey has no overloading in the source
      // language (§1.3: a name does one thing), so they are emitted as
      // `name`, `name$1`, `name$2` … and the checker SELECTS the first that
      // accepts the call — rather than merging them into one loose signature
      // that would accept calls the IDL rejects.
      const overloads = allMembers(name).filter(
        (o) => o.type === "operation" && o.name === m.name && o.special !== "static",
      );
      overloads.forEach((o, idx) => {
        const suffix = idx === 0 ? "" : `$${idx}`;
        const ret = mapType(o.idlType);
        lines.push(`    ${o.name}${suffix}(${mapArgs(o.arguments ?? [])}): ${ret};`);
        if (idx > 0) counts.members++;
      });
      const args = m.arguments ?? [];
      const ret = mapType(m.idlType);
      const _ = args;
      const _r = ret;
      // One thunk per name: JS dispatches the overload itself.
      bindCalls.push([`${name}.${m.name}`, `(t,a)=>t.${m.name}(...a)`]);
      counts.members++;
      counts.bindings = (counts.bindings ?? 0) + 1;
      counts.overloads = (counts.overloads ?? 0) + Math.max(0, overloads.length - 1);
    }
  }
  // CSS properties (from the CSS specs, not the IDL).
  if (name === "CSSStyleProperties") {
    for (const prop of cssProps) {
      const cssSt = cssPropStatus(prop);
      if (isDropped(cssSt)) {
        recordDrop("css", prop, cssSt);
        continue;
      }
      const jsName = camel(prop);
      if (validName(jsName) && !seen.has(jsName)) {
        seen.add(jsName);
        lines.push(`    ${jsName}: string;`);
        bindGets.push([`CSSStyleProperties.${jsName}`, `(t)=>t.${jsName}`]);
        bindSets.push([`CSSStyleProperties.${jsName}`, `(t,v)=>{t.${jsName}=v;}`]);
        counts.members++;
        counts.bindings = (counts.bindings ?? 0) + 1;
        counts.cssProps = (counts.cssProps ?? 0) + 1;
      }
    }
  }
  // Indexed getters (NodeList[i], HTMLCollection[i], …): expose as item().
  const indexed = allMembers(name).find(
    (m) => m.type === "operation" && m.special === "getter" && !m.name,
  );
  if (indexed && !seen.has("item")) {
    lines.push(`    item(p0: int32): ${mapType(indexed.idlType)};`);
    counts.members++;
  }
  out += `interface ${name}${parent} {\n${lines.join("\n")}\n}\n`;
  counts.interfaces++;

  // Constructors (`new URL(…)`, `new WebSocket(…)`).
  if (allMembers(name).some((m) => m.type === "constructor")) {
    const ctorSt = ctorStatus(name);
    if (isDropped(ctorSt)) {
      recordDrop("constructor", name, ctorSt);
    } else {
      bindCtors.push([name, `(a)=>new ${name}(...a)`]);
    }
  }
  // Statics + constants live on the interface OBJECT: emit a companion
  // interface and an ambient global of that type (e.g. `Node.ELEMENT_NODE`,
  // `Response.error()`), so `Node.ELEMENT_NODE` type-checks.
  const statics = [];
  const seenS = new Set();
  for (const m of allMembers(name)) {
    if (m.type === "const" && validName(m.name) && !seenS.has(m.name)) {
      const st = memberStatus(name, m.name);
      if (isDropped(st)) {
        recordDrop("static", `${name}.${m.name}`, st);
        continue;
      }
      seenS.add(m.name);
      statics.push(`    readonly ${m.name}: ${mapType(m.idlType)};`);
    } else if (
      m.type === "operation" && m.special === "static" && validName(m.name) &&
      !seenS.has(m.name)
    ) {
      const st = memberStatus(name, m.name, { isStatic: true });
      if (isDropped(st)) {
        recordDrop("static", `${name}.${m.name}`, st);
        continue;
      }
      seenS.add(m.name);
      statics.push(`    ${m.name}(${mapArgs(m.arguments)}): ${mapType(m.idlType)};`);
    } else if (
      m.type === "attribute" && m.special === "static" && validName(m.name) &&
      !seenS.has(m.name)
    ) {
      const st = memberStatus(name, m.name, { isStatic: true });
      if (isDropped(st)) {
        recordDrop("static", `${name}.${m.name}`, st);
        continue;
      }
      seenS.add(m.name);
      statics.push(`    readonly ${m.name}: ${mapType(m.idlType)};`);
    }
  }
  // Every interface has an interface OBJECT (window.HTMLElement): emit it as
  // an ambient global so `x instanceof HTMLElement` works, and hang the
  // constants/statics off it.
  out += `interface __static_${name} {\n${statics.join("\n")}\n}\n`;
  staticGlobals.push([name, `__static_${name}`]);
}

// Interface objects as ambient globals (constants + statics).
for (const [name, ty] of staticGlobals) {
  out += `let ${name}: ${ty};\n`;
  counts.globals++;
}

// namespaces (console, CSS, …) → pseudo-interface + global
for (const [name, members] of namespaces) {
  const nsSt = interfaceStatus(name);
  if (isDropped(nsSt)) {
    recordDrop("namespace", name, nsSt);
    continue;
  }
  const seen = new Set();
  const lines = [];
  for (const m of members) {
    if (m.name && isDropped(memberStatus(name, m.name))) {
      recordDrop("member", `${name}.${m.name}`, memberStatus(name, m.name));
      continue;
    }
    if (m.type === "operation" && m.name && validName(m.name) && !seen.has(m.name)) {
      seen.add(m.name);
      lines.push(`    ${m.name}(${mapArgs(m.arguments)}): ${mapType(m.idlType)};`);
    } else if (m.type === "attribute" && !seen.has(m.name)) {
      seen.add(m.name);
      lines.push(`    readonly ${m.name}: ${mapType(m.idlType)};`);
    }
  }
  out += `interface __ns_${name} {\n${lines.join("\n")}\n}\n`;
  out += `let ${name}: __ns_${name};\n`;
  counts.globals++;
}

// Globals: the members of the global scopes become ambient globals. Walk
// inheritance (Window extends EventTarget → addEventListener is global) and
// include the worker scope so the same program can run on a worker thread.
function withInherited(name, acc = [], seen = new Set()) {
  if (!name || seen.has(name)) return acc;
  seen.add(name);
  const rec = interfaces.get(name);
  if (!rec) return acc;
  acc.push(...allMembers(name));
  return withInherited(rec.inheritance, acc, seen);
}
const winMembers = [
  ...withInherited("Window"),
  ...withInherited("DedicatedWorkerGlobalScope"),
];
const seenG = new Set(["console", "postMessage"]); // postMessage: ES_AMBIENT
for (const m of winMembers) {
  if (!m.name || seenG.has(m.name) || m.name === "constructor" || !validName(m.name)) continue;
  const gSt = globalStatus(m.name);
  if (isDropped(gSt)) {
    recordDrop("global", m.name, gSt);
    continue;
  }
  if (m.type === "attribute" && !m.special) {
    seenG.add(m.name);
    let ty = mapType(m.idlType);
    if (ty === "void") ty = "JsAny";
    out += `let ${m.name}: ${ty};\n`;
    counts.globals++;
  } else if (m.type === "operation" && !m.special && !m.static) {
    seenG.add(m.name);
    let ret = mapType(m.idlType);
    out += `let ${m.name}: (${mapArgs(m.arguments)}) => ${ret};\n`;
    counts.globals++;
  }
}
out += `let window: Window;\n`;
counts.globals++;

writeFileSync(new URL("../../crates/mersey_front/src/webapi.gen.mersey", import.meta.url), out);

// ---- generated binding tables (JS) ------------------------------------------
const table = (name, entries) =>
  `export const ${name} = new Map([\n${entries
    .map(([k, fn]) => `["${k}",${fn}]`)
    .join(",\n")}\n]);\n`;

const bindingsJs =
  `// GENERATED by tools/webidl-gen/generate.mjs — do not edit.\n` +
  `// Direct binding thunks for every standardized member: the bridge calls\n` +
  `// these instead of doing reflective property lookup, so each web API call\n` +
  `// is a monomorphic JS call the engine can inline. Stage B replaces this\n` +
  `// backend with native Blink bindings from the same generator.\n` +
  `// APIs marked deprecated or non-standard by @mdn/browser-compat-data are\n` +
  `// excluded (experimental kept); see tools/webidl-gen/dropped.txt.\n` +
  table("CALLS", bindCalls) +
  table("GETS", bindGets) +
  table("SETS", bindSets) +
  table("CTORS", bindCtors);
writeFileSync(new URL("../../web/mersey-bindings.gen.js", import.meta.url), bindingsJs);

// The BCD drop report: committed beside the generator so webref/BCD bumps
// show surface changes as a reviewable diff, like the .gen files themselves.
const droppedLines = [...dropped].sort();
writeFileSync(new URL("./dropped.txt", import.meta.url), droppedLines.join("\n") + "\n");
const dropSummary = Object.create(null); // null proto: "constructor" is a real kind
for (const l of droppedLines) {
  const kind = l.split(" ", 1)[0];
  dropSummary[kind] = (dropSummary[kind] ?? 0) + 1;
}

console.log(
  `bindings: ${bindCalls.length} calls, ${bindGets.length} getters, ` +
    `${bindSets.length} setters, ${bindCtors.length} constructors`,
);
console.log(
  `generated: ${counts.interfaces} interfaces (${counts.members} members, ` +
    `${counts.cssProps ?? 0} of them CSS properties), ${counts.dicts} dictionaries, ` +
    `${counts.typedefs} typedefs/enums, ${counts.callbacks} callbacks, ` +
    `${counts.globals} globals`,
);
console.log(
  `bcd-dropped: ` +
    Object.entries(dropSummary)
      .sort()
      .map(([k, n]) => `${n} ${k}`)
      .join(", ") +
    ` (details in dropped.txt)`,
);
