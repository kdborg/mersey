# Mersey

Mersey is a strictly-typed, class-based language for web browsers and standalone
use. It takes JavaScript/TypeScript as its syntactic starting point, then removes
the historical baggage: there is exactly one mode (strict), no prototype system,
no `var`, no implicit stringly-typed coercion, and no `eval`.

Source files use the `.mersey` extension and are encoded in UTF-8, so any
common editor works; the `string` type is 4-byte code points internally.

## Design pillars

1. **One mode.** Strict semantics always; there is nothing to opt into or out of.
2. **No prototypes.** Classes are nominal and sealed at declaration. Object
   layout is fixed at compile time, which makes property access a constant
   offset load — no hidden classes, no inline-cache misses.
3. **Strict static typing.** Every binding has a static type, inferred or
   declared. `let`/`const` block scoping only.
4. **C/C++-style numeric coercion.** Implicit conversions exist *only between
   numeric types*, following C's promotion and usual-arithmetic-conversion
   rules (with all undefined behavior removed). Nothing else converts
   implicitly — no `"1" + 1`.
5. **Real numeric types.** `int8/16/32/64`, `uint8/16/32/64`, `float32/64`,
   plus arbitrary-precision `bigint` and `bigdec`.
6. **Classes with real access control.** `public`, `protected`, `private` —
   enforced by the type system and the runtime.
7. **Code-point strings.** The default `string` type uses 4-byte code points:
   `s[i]` is O(1) and always a whole character — no surrogate-pair traps.
   Source files are plain UTF-8, decoded to code points at the front door.
8. **Consistent APIs.** The standard library follows a single naming and
   signature convention (see spec §Overview); no JS-style inconsistencies.
9. **Performant.** Tiered execution: bytecode interpreter → optimizing JIT.
   Static types and sealed classes let the JIT emit C++-class-quality code.
10. **Secure by construction.** No dynamic code evaluation, W^X JIT memory,
    capability-scoped I/O in the standalone runtime, CSP-integrated in the
    browser.

## Delivery targets

- **`mersey`** — standalone engine/CLI (run, check, compile, format, convert).
- **Browser integration** — loaded like JavaScript
  (`<script type="text/mersey" src="app.mersey">`) but executed by its own
  engine, *not* inside the JavaScript engine. Chromium is the first target;
  see `docs/architecture/browser-integration.md` for the two-stage plan
  (WASM-hosted shim first, native Blink integration second).

## Repository layout

```
docs/spec/           Language specification
docs/architecture/   Engine, browser integration, embedding API
examples/            Sample .mersey programs
ROADMAP.md           Phased implementation plan
```

## Status

**MVP working end-to-end.** The frontend (lexer → parser → binder) is done;
an MVP interpreter runs Mersey natively (`mersey run app.mersey`) and in the
browser via the Stage A polyfill:

```sh
./web/build-and-test.sh          # build engine to WASM + headless e2e test
cd web && python3 -m http.server # then open http://localhost:8000
```

The type checker, bytecode VM, and JIT replace the MVP internals in later
phases without changing behavior — the conformance suites are the contract.
See `ROADMAP.md`.
