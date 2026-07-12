# Browser Integration (Chromium first)

Goal: `.mersey` files load like JavaScript but execute in the Mersey engine —
a separate engine beside V8, not a language hosted inside it.

```html
<script type="text/mersey" src="/app/main.mersey"></script>
<script type="text/mersey" src="/app/main.mersey" integrity="sha384-…"></script>
```

`type="text/mersey"` means unknown script types are ignored by every other
browser — pages degrade cleanly, and the Stage A shim (below) can polyfill on
browsers without native support, which is exactly how ES modules rolled out.
Mersey scripts are always module-goal, deferred, and CSP-governed (spec §5.4).

## Two-stage plan

### Stage A — WASM-hosted shim (no browser fork; the development vehicle)

Compile the engine core to WebAssembly. A small JS loader (the only JS in the
system, ~2 KB) does:

1. Find `script[type="text/mersey"]`, fetch sources (CORS/SRI honored).
2. Instantiate the engine WASM module.
3. Bridge DOM calls through `browser:dom` imports → JS host functions.

Purpose: lets the language, stdlib, and `browser:` API surface be developed
and dogfooded on *every* browser immediately, and gives us conformance tests
that Stage B must also pass. Limitations to be explicit about: it technically
executes inside V8's WASM tier (acceptable only as bootstrap), JIT tier is
unavailable (WASM can't emit code pages), and DOM calls pay a JS bridge cost.
Stage A is scaffolding, not the product.

### Stage B — native Chromium integration (the actual target)

Chromium is designed around one script engine; this is the deep part of the
project. Integration points, in dependency order:

1. **Component:** new `//components/mersey` wrapping `mersey_capi` (Rust is
   acceptable in Chromium behind a C++ API layer; `//build/rust` toolchain).
2. **Loading:** Blink's `HTMLParserScriptRunner` /`ScriptLoader` currently
   routes `<script>` by type. Add a `MerseyScript` kind alongside
   `ClassicScript`/`ModuleScript`; reuse Blink's existing resource fetcher,
   CORS, SRI, and CSP checks (`ScriptResource` is engine-agnostic enough to
   share). Mersey module graphs ride the same ModuleMap-style infrastructure.
3. **Execution contexts:** one Mersey context per Document/origin, created
   lazily on first Mersey script, torn down with the ExecutionContext. Heap
   isolation per context (spec §5.2) matches Chromium's site-isolation model.
4. **DOM bindings:** generate `browser:dom` (and the wider Web API surface)
   from the same **WebIDL** files Blink uses for JS bindings — a second
   code-generator backend (`mersey_bindings_generator`) beside the V8 one.
   This is the key maintainability decision: we never hand-write DOM glue,
   and new Web APIs appear in Mersey when their IDL lands.
5. **Object identity across engines:** a DOM node touched by both JS and
   Mersey must be the same node. Blink's C++ DOM objects are the source of
   truth; each engine holds wrapper objects. Cross-engine traffic
   (`dispatchEvent`, callbacks) marshals through Blink types, never
   engine-to-engine directly.
6. **Event loop:** Mersey has no own event loop; it schedules on Blink's task
   runners (microtask checkpoint integration included), so `async`/promises
   interleave correctly with JS and rendering.
7. **Debugging:** implement the Chrome DevTools Protocol domains
   (Debugger/Runtime/Profiler) against the Mersey engine — CDP is
   engine-agnostic JSON, deliberately.

Build: start as a Chromium fork with a GN flag (`enable_mersey=true`);
upstreaming is a policy question for much later.

## Clean interfaces (the contract list)

The browser work is held together by four interfaces, each specified in its
own document as it firms up:

| Interface | Between | Form |
|---|---|---|
| Embedding API | any host ↔ engine | C ABI (`embedding-api.md`) |
| `browser:*` modules | Mersey code ↔ Web platform | WebIDL-generated Mersey decls |
| Script loading | Blink loader ↔ engine | `MerseyScript` + fetch/CSP hooks |
| JS interop | V8 world ↔ Mersey world | typed, explicit, via Blink (below) |

## JS interop (explicit, not ambient)

Mersey and JS share the DOM but not a heap. Interop is opt-in and typed:

- Mersey exports marked `export extern` become callable from JS as
  `mersey.modules["./main.mersey"].fn(args)` with arguments checked and
  converted at the boundary (numbers, strings — transcoded UTF-16↔UTF-32 —,
  ArrayBuffers zero-copy where alignment allows, DOM handles by identity).
- JS values never enter Mersey as `any`; a JS call that fails the declared
  signature throws a `TypeError` on the JS side.
- No shared mutable objects other than DOM nodes and `SharedArrayBuffer`
  (which follows the platform's COOP/COEP gating).
