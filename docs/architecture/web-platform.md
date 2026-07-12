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

## 2. Values: the universal bridge

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

Because Promises come back as live objects, promise-based APIs work today:

```mersey
fetch("/api").then(resp => console.log(resp.status));
```

`async`/`await` sugar is still on the deferred list — it needs the engine's
event loop, not the bindings.

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

## Known limits

- **Record field order** is not preserved across the bridge (Mersey records
  are unordered maps), so `JSON.stringify({a, b})` may emit keys in a
  different order.
- **`iterable<>` / `maplike` declarations** are not expanded, so
  `for (const x of someWebIterable)` needs an explicit index loop.
- Callbacks are retained for the page's lifetime (no handle release yet).
- In Stage B the same generator targets Blink's own IDL and the bridge is
  replaced by direct native bindings; the ABI is unchanged.
