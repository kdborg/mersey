# Ladybird integration (Stage B)

**The fork is built and measured.** `<script type="text/mersey">` in a page is
compiled and executed by the Mersey engine inside Ladybird — no LibJS for the
Mersey code, no WASM in the stack — through the same C ABI
(`crates/mersey_capi/include/mersey.h`) that `native/host_demo.c` and
`crates/mersey_capi/tests/abi.rs` drive here, and that the Gecko, Chromium and
Servo forks drive too. One boundary, now five hosts. All six web workloads plus
compute run, and every checksum matches the other native forks (the engine does
bit-identical work in every host).

Ladybird is C++ (LibWeb for the DOM/HTML, LibJS for JavaScript, built with
CMake), so the integration mirrors the **Chromium** fork, not the Servo one: the
engine is a **prebuilt Rust staticlib** the fork links (`libmersey_capi.a` +
`mersey.h`, the `//third_party/mersey` model), and the glue is a C++ module in
LibWeb — unlike Servo, where the engine is a Cargo crate in Servo's own build.

The web bridge began as **reflective C++→LibJS** (the host table's `web_*` hooks
calling the tested `web/mersey-bridge.js` through LibJS, like the WASM polyfill),
the honest bootstrap — then grew every faster tier the other forks have
(**interned** ids, **wide-string** `web_*_u16`, **`web_bind`**, a **direct-DOM**
tier) — and is now **native C++ end to end: no JS source is evaluated at all**.
The host owns the handle table (a `GC::RootVector` + dedup map, handle 0 = the
global), hot methods classified at intern time dispatch straight to LibWeb C++
(`getRandomValues`, `new URL`/`pathname`/`search`, `createElement`/`textContent`/
`appendChild`, `new Event`/`dispatchEvent`, `className`/`classList`/`contains`,
`style`/`setProperty`/`getPropertyValue`, `querySelectorAll`/`length` + indexed
NodeList access, `TextEncoder.encode`/`TextDecoder.decode`,
`Storage.getItem`/`setItem`/`removeItem`, and `setTimeout`/`clearTimeout` to
`WindowOrWorkerGlobalScopeMixin`), and everything outside the hot set is served
by native LibJS reflection — `Object::get` / `JS::call` / `JS::construct`
against the IDL bindings, the engine's tagged JSON encoded/decoded in C++, and
Mersey closures crossing as `NativeFunction`s (an event listener or timer
callback re-enters the engine with no JS trampoline). Ladybird's LibJS is
**UTF-16** (`PrimitiveString`/`Utf16String`), so strings cross with no
conversion; arguments are a `GC::RootVector`.

The Ladybird tree is not *in* this repo (a checkout is large). It lives beside
it, the way `browsers/chromium`, `browsers/firefox` and `browsers/servo` do:

```
browsers/ladybird
  Libraries/LibWeb/Mersey/            the engine module (native C++ bridge)
  Libraries/LibWeb/CMakeLists.txt     compile the module, link libmersey_capi.a
  Libraries/LibWeb/HTML/HTMLScriptElement.cpp   the text/mersey hook
```

## What this directory ships

| | |
|---|---|
| `mersey/MerseyScriptRunner.{h,cpp}` | one engine context per script realm (a raw thread-local pointer — re-entrant by the ABI's thread rule, like the Servo fork's `Runner`). Host table wired to stdout (`print`), a monotonic clock (`time_ms`), and the native C++ web bridge — the handle table, the reflective ops (tagged-JSON encode/decode included), the wide-string `web_*_u16` tier, `web_bind` (canvas → direct `fill_rect`), and the direct-DOM hot methods, all in C++, with no JS bridge script anywhere. The whole run is wrapped in a `Web::HTML::TemporaryExecutionContext` because `prepare_script()` runs at HTML-parse time with no JS on the VM stack, and every LibJS op (`Object::get`, `JS::call` into a binding, a `NativeFunction` listener firing) needs one. A callback re-enters the engine through `msy_context_invoke_args`, straight from the `NativeFunction`. |
| `mersey/mersey.h` | the C ABI header, copied from `crates/mersey_capi/include/` at apply time. |
| `apply.sh` | idempotent source hook: install the module, wire CMake, patch the `HTMLScriptElement` hook. |

## Results (native leg, this machine: aarch64, 4 cores)

Median of 3 runs, self-timed inside the engine (startup excluded). The three
JIT-native forks side by side, after wiring the wide-string fast paths:

| workload | firefox-fork | servo-fork | **ladybird-fork** |
|---|---|---|---|
| compute | 102.2 | 95.9 | **92.6** |
| json | 3.2 | 1.1 | **1.3** |
| canvas | 4.4 | 18.0 | **10.3** |
| dom | 13.1 | 31.4 | **29.1** |
| crypto | 10.4 | 18.7 | **26.3** |
| url | 41.3 | 36.0 | **54.5** |
| storage | 55.1 | 2110.9 | **9951.1** |

### What the wide-string fast paths bought (before → after)

The reflective bridge has tiers: fully-reflective (JSON args + JSON reply),
interned scalar (typed args, JSON reply), and **wide-string** — the bridge's
`*Wide` methods return the raw value, so there is no JSON on either side, strings
cross as UTF-16 with no conversion, and object arguments (`appendChild(el)`,
`getRandomValues(buf)`) stay off the JSON path via a refs bitmask. Both forks
originally left the `web_*_u16` hooks `NULL`; wiring them (this change) gave:

| workload | servo before → after | ladybird before → after |
|---|---|---|
| crypto | 38.7 → **18.7** (−52%) | 116.9 → **50.4** (−57%) |
| url | 67.1 → **36.0** (−46%) | 220.0 → **123.5** (−44%) |
| canvas | 27.3 → **18.0** (−34%) | 56.4 → **38.1** (−32%) |
| dom | 35.1 → **31.4** (−11%) | 93.5 → **67.7** (−28%) |
| storage | 2129.9 → 2110.9 (~0%) | 10003.1 → 9951.1 (~0%) |

Checksums are unchanged in every cell — the wide path is a faster transport, not
different work.

Two further honest points:

- **Compute is the fastest of the three forks (92.6 ms).** It never crosses the
  bridge — pure Cranelift Tier-1 JIT — so it measures the engine, and the engine
  is as fast in Ladybird as anywhere. The staticlib is built with the `jit`
  default feature.
- **`web_bind` (typed bindings) is now wired for canvas.** The JIT-compiled canvas
  loop crosses as a compile-time bind id + raw doubles, and the host switches
  straight to the C++ `CanvasRenderingContext2D::fill_rect` — no JS, no
  marshalling. `canvas` fell **38.1 → 10.3 ms** (−73%), on par with the Chromium
  fork (11.2) and closing on Gecko (4.4); the context is unwrapped from its handle
  once and cached across the loop. This is the tier Gecko has that Chromium, Servo,
  and Ladybird all lacked; it is a pure host change (the JIT already emits the
  `web_bind` call — `webbind::numeric` covers the 9 canvas 2D methods).
- **`storage` did not move, in either fork** — the wide path proves the bridge is
  now negligible there: at ~250 µs/op (Ladybird) it is bound by the browser's own
  `localStorage` (synchronous persistence), not the crossing. That is a
  browser-implementation cost, not something the fork can close.
- **`crypto` now has a direct-DOM path too.** `getRandomValues` isn't a numeric
  loop (so no `web_bind`), but the wide-path hook (`web_call_u16`) recognizes its
  interned id, unwraps the `Crypto` receiver and the buffer once (cached by
  handle, invalidated on release), and calls the C++ `Crypto::get_random_values`
  directly — **crypto 50.4 → 26.3 ms (−48%)**. The remaining ~26 ms is the CSPRNG
  itself, not the crossing. The mechanism (intern-time classification →
  type-checked unwrap → direct C++, with a reflective fall-back) is the general
  direct-DOM template.
- **`dom` and `url` now have direct-DOM paths too.** These *allocate* objects, so
  they needed one extra piece: a `register` bridge method that keeps a
  host-created object alive and returns a handle, whose C++ pointer the host
  caches at creation — so the follow-up reads never cross. `new URL` →
  `DOMURL::construct_impl` with `pathname`/`search` read from the cached pointer:
  **url 220 → 55 ms** (−75%), now *ahead of the Chromium fork*, 41 for Gecko.
  `createElement` → `DOM::create_element`, `textContent` → `Node::set_text_content`,
  `appendChild` → `Node::append_child`, all resolving from the cache:
  **dom 94 → 29 ms** (−69%), *ahead of Servo* (31), near Chromium (24). Checksums
  unchanged. The remaining gap to Gecko is its bindings being direct C++ end to
  end (and, for crypto, the CSPRNG cost).
- **The direct-DOM tier, in one shape.** A hot method is classified when the
  engine interns its name; the wide-path hooks (`web_get_u16`/`web_set_u16`/
  `web_call_u16`/`web_new_u16`) switch on the id, unwrap the receiver (and object
  args) to their C++ types via `is<T>`/`as<T>`, call the method directly, and fall
  back to the reflective wide path on any type mismatch — so it is always correct,
  just faster on the hot path. Receivers are cached by handle; created objects are
  registered and cached at creation.

Memory (PSS) for the Ladybird leg is not measured: `test-web` runs each page in a
sub-second WebContent child, too short to sample the process tree reliably (the
Servo leg's `servoshell` PSS is captured, shown in the full report).

## Building and running

Ladybird has **no `--headless URL` binary** in current trees; its headless
runner is `test-web` (a WPT-style harness). The engine is a **prebuilt
staticlib**, so build it in this repo first:

```bash
cargo build --release -p mersey_capi        # -> target/release/libmersey_capi.a (jit on)

ladybird/apply.sh ~/Work/ladybird                # install the module + wire CMake + hook

cd ~/Work/ladybird
./Meta/ladybird.py build test-web           # builds LibWeb (+ the fork) and the helper processes
```

Then measure, from this repo:

```bash
node bench/web/run-native-ladybird.mjs      # -> results.native.ladybird.json
node bench/web/report.mjs                    # merge into REPORT.md
```

`run-native-ladybird.mjs` writes each workload as an inline `text/mersey` Text
test under `~/Work/ladybird/Tests/LibWeb/Text/input/mersey/`, plus a trailing
`include.js` + `test(() => {})` so the test completes at once. The engine's host
`print` hook writes the `RESULT` line to WebContent's stdout, which `test-web`
captures into a per-test `.logs.html` — that is where the harness reads it
(the mersey script runs at parse time, so `RESULT` is emitted before completion).
Override the checkout and binary with `LADYBIRD_SRC` / `TEST_WEB`.

### Environment notes (aarch64, no root)

Building `test-web` on this host needed several userspace workarounds, none of
which touch the fork itself — they are Ladybird/vcpkg build-environment issues:

- **depot_tools shadows `ninja` and `gn`** with wrappers that only work inside a
  Chromium checkout. Put a real `ninja` and a real `gn` (vcpkg builds one at
  `Build/vcpkg/packages/vcpkg-tool-gn_*/tools/gn/gn`) *ahead* of depot_tools on
  `PATH`.
- **vcpkg ports need `python3 -m venv` and autotools**, which the system Python
  3.14 and a bare box lack. A `uv`-managed Python (venv-capable) and a
  conda-forge `autoconf/automake/libtool/autoconf-archive` env (via micromamba)
  supply them; set `ACLOCAL_PATH` to the conda `share/aclocal`.
- **No Qt6 (`qmake6`)**, and Ladybird gates *both* the Qt UI and the Services
  helper processes behind `ENABLE_GUI_TARGETS`. `test-web` needs the Services,
  not the Qt chrome, so skip only `add_subdirectory(UI)` in the top-level
  `CMakeLists.txt` and stage the runtime resources it would have copied via a
  placeholder `ladybird` target:
  ```cmake
  include(${CMAKE_SOURCE_DIR}/UI/cmake/ResourceFiles.cmake)
  add_custom_target(ladybird)
  copy_resources_to_build("${CMAKE_BINARY_DIR}/${IN_BUILD_PREFIX}${CMAKE_INSTALL_DATADIR}/Lagom" ladybird)
  ```
  On a host with Qt6 this is unnecessary — build normally.

## The text/mersey hook

`apply.sh` copies the module and wires CMake robustly (an append-only block).
The hook it patches into `HTMLScriptElement::prepare_script()` — verified against
the current tree — intercepts an **inline** `text/mersey` script just before the
type is classified as a JavaScript MIME essence:

```cpp
// Mersey fork: an inline `<script type="text/mersey">` runs in the embedded
// engine (native leg of bench/web), bypassing the classic/module machinery.
if (!has_attribute(HTML::AttributeNames::src)
    && script_block_type.equals_ignoring_ascii_case(u"text/mersey"sv)) {
    auto mersey_source = source_text.to_utf8();
    Web::Mersey::run_mersey_script(realm(), mersey_source);
    return;
}
```

Only inline scripts are supported (mirroring the other forks); external `src=`
is the tracked follow-up (`scan_imports`/`run_graph` in the ABI, CORS/CSP/SRI
stay Ladybird's). Method/API names track Ladybird's LibWeb/LibJS at integration
time (this was verified against a tree whose LibJS had migrated to UTF-16); a
much newer tree may need a name nudged.

## The stock leg

`bench/web/run-ladybird.mjs` collects js / transpiled-JS / WASM-polyfill in
(effectively) unmodified Ladybird — the fork's mersey module is dormant for
these pages. Under `test-web`'s `file://` loader every fetch and relative
module import is refused (cors requires http), so each generated Text test is
fully self-contained: workload + bridge + bindings + the base64'd WASM engine
inline. Probed facts that make it work: `WebAssembly.instantiate(bytes)` and
`import(blobURL)` both succeed under `file://` — only fetch is blocked. RESULT
reaches `actual.txt` through a println'd console hook inside `asyncTest`;
memory is peak PSS of the process tree minus a blank baseline. Two honest
exclusions: `fetch` (no http origin) and poly/tjs `compute` (LibWasm is an
interpreter — timing it interpreting the Mersey interpreter measures LibWasm,
which the poly bars already show).

## Not yet done

- **Direct-C++ fast paths** against more of Ladybird's LibWeb bindings (the
  direct-DOM tier covers the hot methods of the twelve workloads today; the
  native reflective floor covers the rest of the WebIDL surface without JS).
