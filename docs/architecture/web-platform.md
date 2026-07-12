# Web platform support

Mersey reaches **every standardized web technology** — not through
hand-written glue per API, but through two generated/reflective mechanisms.

## 1. Types: generated from WebIDL

`tools/webidl-gen/generate.mjs` consumes `@webref/idl` — the machine-readable
corpus of the standardized web platform, the same IDL browsers build from —
and emits `crates/mersey_front/src/webapi.gen.mersey`:

| | count |
|---|---|
| interfaces | 1,122 |
| members (attributes + operations) | 8,101 |
| — of which CSS properties (from `@webref/css`) | 743 |
| dictionaries | 903 |
| typedefs / enums | 538 |
| callbacks | 78 |
| ambient globals (Window + interface objects) | 381 |

Interface objects are emitted too, so constants and statics work
(`Node.ELEMENT_NODE`, `Response.error()`), indexed getters become both
`item(i)` and real indexing (`nodeList[0]`), and CSS properties are attached
to `CSSStyleProperties` camelCased, so `el.style.backgroundColor = "…"`
type-checks and applies.

These are ambient **types** only. Per spec §5.4 (no ambient authority) the
*values* are unavailable until imported:

```mersey
import { document, localStorage, fetch, crypto } from "browser:dom";
```

The checker collects the generated module before the user's, so every
standardized interface is a known type, and imported globals get their real
IDL types — including precision the hand-written surface lacked
(`getElementById` returns `Element?`; `value` lives on `HTMLInputElement`,
not `Element`).

Regenerate with: `cd tools/webidl-gen && npm install && node generate.mjs`.

Type mapping (v1): integers → `int32`; 64-bit ints and floats → `float64`;
`DOMString`/`USVString` → `string`; `sequence<T>` → `T[]`; `Promise<T>` →
`Promise<T>`; IDL `any`/`object`/`record<K,V>` → `JsAny` (checker: `any`).
Overloads take their first signature; parameters are renamed `p0..pN`.

## 2. Bindings: generated for every member

The generator also emits `web/mersey-bindings.gen.js` — a **direct thunk for
every standardized member**, so the bridge never falls back on reflective
property lookup:

| table | entries |
|---|---|
| method calls | 2,460 |
| getters | 5,623 |
| setters | 2,806 |
| constructors | 438 |
| **total** | **11,327** |

The bridge resolves `(interface, member) → thunk` once per shape and caches
it (walking the prototype chain, so `Node.textContent` is found on an
`HTMLDivElement`). Reflection remains only as the fallback for objects
outside the IDL corpus. **Stage B swaps this JS backend for native Blink
bindings from the same generator; the ABI does not change.**

### What actually costs time (measured, real Chromium)

20,000 DOM property writes / 20,000 method calls, via `web/test/bench.mjs`:

| configuration | writes | calls |
|---|---|---|
| baseline (reflection + JSON marshalling) | 63 ms | 57 ms |
| + marshalling fast paths | 49 ms | 48 ms (**22% / 16% faster**) |
| + generated bindings (both on) | 49 ms | 51 ms |

The honest finding: **generated bindings do not make Stage A faster** —
V8's inline caches already make `obj[prop]` about as fast as a thunk. The
speed came from killing the marshalling overhead: member names are now
*interned* (a name crosses the ABI once, then it is an integer id — no
`TextDecoder` per call) and scalar arguments/values take dedicated ABI paths
that skip JSON entirely.

What the generated bindings *do* buy is completeness and correctness — every
standardized member is bound, typed, and reviewable — and they are the
artifact the native Stage B integration consumes.

## 3. Values: the universal bridge

`web/mersey-bridge.js` implements five reflective operations that reach any
object in the host realm — so breadth costs nothing per API:

| operation | meaning |
|---|---|
| `global(name)` | resolve an ambient global to a handle |
| `get(handle, prop)` | read a property (methods come back bound) |
| `set(handle, prop, v)` | write a property |
| `call(handle, method, args)` | invoke (`method: ""` calls the handle itself, e.g. `fetch`) |
| `new(ctor, args)` | construct (`new URL(…)`, `new Uint8Array(4)`, `new WebSocket(…)`) |

Wire format is tagged JSON (`crates/mersey_interp/src/webjson.rs`):
primitives pass as scalars, host objects as `{"__ref__": handle}` through a
handle table that preserves object identity, and **Mersey closures cross as
real JS functions** (`{"__cb__": id}` → the loader wraps them so JS can call
them with event objects and resolved promise values).

## 4. async / await

Fully supported. The bytecode VM's state (pc, operand stack, scopes,
handlers) is plain data, so `await` **suspends** by capturing it into a
coroutine and resuming when the promise settles — no CPS transform, no
threads.

```mersey
async function load(url: string): string {
    const resp = await fetch(url);          // suspends; the browser resumes us
    if (!resp.ok) { throw new RangeError(`${url} → ${resp.status}`); }
    return await resp.text();
}
```

Awaiting a **host** promise adopts it: the engine hands `resolve`/`reject`
callbacks to its `.then` through the bridge. `Promise.all` / `resolve` /
`reject` come from `std:async`, `.then` chains still work, throws cross back
through `await` into `try`/`catch`, and the engine owns **no event loop** —
it drains its microtask queue before returning to the host, which owns timers
and I/O (embedding-api.md rule 1).

## Proven — in a real browser

`web/test/browser.mjs` launches **headless Chromium (Playwright)**, serves
`web/` over HTTP, loads the real pages, and drives them with real user
input. Nothing is stubbed. It asserts from the browser side that:

- the counter and TODO demos respond to **real clicks and typing**, creating
  and removing real `<li>` elements;
- **Web Storage** really wrote (`localStorage.getItem` from the page);
- **Web Crypto** filled a real `Uint8Array`;
- the **URL** parser, the platform's **JSON**, and **timers** work;
- **Canvas 2D** actually painted — the test reads back pixel `(5,5)` and
  gets `0,170,255,255`, the `#0af` fill Mersey requested;
- **fetch** resolved a promise back into Mersey code;
- **interface constants** (`Node.ELEMENT_NODE`), **indexed collections**
  (`querySelectorAll("p")[0]`), and **CSS/CSSOM** work — the browser's own
  `getComputedStyle` confirms `rgb(102, 51, 153)` after Mersey set
  `style.color = "rebeccapurple"`.

Run: `./web/build-and-test.sh && node web/test/browser.mjs`
(headless-only harness against a stub realm: `node web/test/platform.mjs`).

## 5. Elements, nodes, custom elements, workers

The last pieces of the initial integration, all verified in real Chromium
(`web/elements.html`, `web/demo/elements.mersey`):

- **Node & element iteration.** `for (const n of nodeList)` works — the
  bridge snapshots any host iterable (NodeList, HTMLCollection, Set, …) via
  the iterator protocol, with an array-like fallback. Tree walking
  (`childNodes`, `nodeType`, `Node.ELEMENT_NODE`) and node construction /
  attachment behave as expected.
- **`instanceof` works against host interfaces.** Every interface has an
  interface *object* (`window.HTMLElement`), so it can be imported as a value
  and used on the right of `instanceof` — including for host-backed Mersey
  instances, which really are elements. It also **narrows**:

  ```mersey
  import { HTMLElement } from "browser:dom";

  if (node instanceof HTMLElement) {
      console.log(node.tagName);   // narrowed to HTMLElement
  }
  ```

  (`instanceof` now narrows for Mersey classes too — it previously didn't.)

- **Host-backed classes.** A Mersey class may `extend` a host interface, and
  its instances then **are** host objects:

  ```mersey
  class Sensor extends EventTarget {
      private readings: int32 = 0;
      public constructor() { super(); }   // constructs the host EventTarget
      public record(): int32 { this.readings += 1; return this.readings; }
  }
  ```

  Members not declared in Mersey resolve against the interface (typed from
  the IDL), `super(…)` constructs the host object, `super.m()` calls the host
  implementation, the instance crosses the bridge *as* its host object, and
  it is assignable anywhere the interface is expected. For objects the
  browser constructs (custom elements), `attach(instance, host)` binds an
  existing host object instead of constructing one.

- **Custom Elements — as ordinary classes.** `web/lib/custom-element.mersey`
  is a *Mersey* library (loaded through the module graph) whose base class
  is host-backed (`CustomElement extends HTMLElement`), so `this` **is** the
  element:

  ```mersey
  import { CustomElement, defineElement } from "../lib/custom-element.mersey";

  class CounterBadge extends CustomElement {
      private count: int32 = 0;
      public override observedAttributes(): string[] { return ["label"]; }
      public override connected(): void {
          this.render();
          this.addEventListener("click", () => { this.count += 1; this.render(); });
      }
      public override attributeChanged(name: string, old: string, now: string): void { … }
      private render(): void { this.textContent = `${this.label}: ${this.count}`; }
  }

  defineElement("mersey-counter", () => new CounterBadge());
  ```

  `this.textContent`, `this.addEventListener`, `this.tagName` are the real
  element's; the instance passes straight into `appendChild`. Verified in
  Chromium: declared elements upgrade, attributes reach the subclass, sibling
  instances keep independent state, `disconnected()` runs on the right
  instance, elements created *from* Mersey upgrade too, and host members read
  and write through.

  The browser still constructs the element, so the loader builds the JS class
  and forwards the lifecycle into Mersey closures:

  ```mersey
  merseyDefineElement("mersey-badge", {
      connected: (el: Element) => { el.textContent = "🌊"; },
      attributeChanged: (el: Element, name: string, old: string, now: string) => { … },
      observed: ["label"],
  });
  ```

  The element is registered with the real `customElements` registry, so the
  browser upgrades it and drives `connectedCallback` /
  `attributeChangedCallback` into Mersey.
- **Web Workers.** `web/mersey-worker.js` boots a second engine instance on
  the worker thread with the bridge pointed at the worker's own global
  scope, so the worker script uses the *same* ambient globals
  (`postMessage`, `addEventListener`, `fetch`):

  ```mersey
  const worker = new Worker("mersey-worker.js?src=demo/worker.mersey",
                            { type: "module" });
  worker.onmessage = (ev: JsAny) => { … };
  worker.postMessage(25);
  ```

  Verified: the worker computes `fib(25) = 75025` on another thread and
  posts it back.
- **Handle release.** `release(obj)` (from `browser:dom`) hands a host object
  back, so long-lived pages that churn through DOM objects don't retain
  handles forever.

## 6. Modules, errors, binary data, security, workers

The initial integration is complete. Beyond §5:

- **Module graph** (spec §4.5). `import { X } from "./lib/x.mersey";` works:
  the engine reports the specifiers it needs (`msy_scan_imports`), the host
  fetches them, and `msy_run_graph` links a dependency-first graph (cycles
  rejected). One `Checker` spans the graph, so **a class declared in one
  module is the same type when imported into another** — `instanceof` holds
  across module boundaries. The CLI loads graphs from disk; the loader,
  workers and service workers all fetch them.
- **Error positions and stack traces.** Chunks carry a pc→position table and
  the engine keeps a frame stack, so a runtime error reports
  `file:line:col` with a trace, readable from Mersey as `e.stack`.
- **Binary data without per-element hops.** `std:bytes` gives a native
  `Bytes` buffer (O(1) access, bounds-checked, uint8 wrapping). A host typed
  array is bulk-copied in once, the loop runs natively, and the result is
  bulk-copied back once: **0.34 µs vs 3.98 µs per element — 11× faster**
  (measured on a 200×200 ImageData fill in real Chromium, canvas contents
  verified).
- **CSP and SRI** (spec §5.4). A `.mersey` source is not a script to the
  browser, so `script-src` does not govern its fetch — the loader therefore
  enforces the page's own policy itself and verifies `integrity="sha384-…"`
  on both entry scripts and imported modules. A tampered hash refuses the
  module. *Honest caveat:* the polyfill needs `'wasm-unsafe-eval'` in the
  page CSP, because **the engine is a WASM module**. Stage B does not — a
  native engine runs under a strict `script-src 'self'` with no eval-ish
  permission at all.
- **Service Workers.** `mersey-sw.js` runs a Mersey program as a service
  worker. The SW spec requires a `fetch` listener registered *synchronously
  during initial evaluation*, which an async WASM boot cannot do — so the
  shim registers the real listener up front, holds each request until the
  engine is ready, and dispatches it to the Mersey handlers. Verified: a
  Mersey service worker intercepts a request and serves the response itself.
- **Mersey promises cross the bridge as real JS promises** (found while
  building the SW: `event.respondWith(promise)` demanded it).
- **String methods**: `indexOf`, `contains`, `startsWith`/`endsWith`,
  `slice`, `split`, `toUpperCase`/`toLowerCase`, `trim` — code-point
  indexed, per §3.4.

## Overload selection

WebIDL overloads are emitted individually (`drawImage`, `drawImage$1`,
`drawImage$2`) and the checker **selects** the first signature that accepts a
call, in IDL order. Mersey itself has no overloading (§1.3: a name does one
thing) — this is purely how the generated host surface is modelled. All three
`drawImage` arities type-check; a two-argument call does not.

## Known limits

- **Host handles are released manually** (`release(obj)`): the engine's
  collector traces Mersey objects, but the host's handle table is the host's.
  Callback *slots* are recycled (`msy_release_callback`), so listener churn
  no longer grows the table forever.
- **Performance** (Stage A only): no JIT in the browser — WASM cannot map code
  pages — and ~2.5 µs per web API call. `std:bytes` is the escape hatch for
  data-heavy loops (11× faster on pixel work). Both costs vanish in Stage B.
- **Debugging**: errors show a Mersey code frame (file, line, caret) and
  `mersey lsp` gives editor diagnostics, but DevTools cannot *step* through
  Mersey bytecode — that needs the native CDP integration (Stage B).

In Stage B the same generator targets Blink's own IDL and the bridge is
replaced by direct native bindings; the ABI is unchanged.
