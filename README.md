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
  engine, *not* inside the JavaScript engine. The engine is hosted natively in
  four experimental browser forks — **Mersey Blink** (Chromium), **Mersey Gecko**
  (Firefox), **Mersey Servo**, and **Mersey Ladybird** — plus a WASM polyfill for
  stock browsers. See `docs/architecture/browser-integration.md`.

## Repository layout

```
docs/spec/           Language specification
docs/architecture/   Engine, browser integration, embedding API
examples/            Sample .mersey programs
ROADMAP.md           Phased implementation plan
```

## Debugging

`mersey dap` is a Debug Adapter Protocol server on stdin/stdout: line
breakpoints in `.mersey` files, continue/step over/step in/step out, the call
stack (outer frames show their call-site lines), and locals — from any DAP
editor. In VS Code, a generic adapter configuration is enough:

```jsonc
// .vscode/launch.json
{
  "version": "0.2.0",
  "configurations": [{
    "type": "mersey",             // with a debugAdapter mapping, or use
    "request": "launch",          // an extension that runs: mersey dap
    "name": "Debug app.mersey",
    "program": "${workspaceFolder}/app.mersey"
  }]
}
```

Point the editor's DAP client at the `mersey dap` command — VS Code via the
`editors/vscode-mersey` extension; any editor that speaks DAP directly
(Helix, Zed, nvim-dap) takes the command as-is. Breakpoints are path-matched
across the module graph, every stack frame serves its variables, and async/
generator bodies (which execute on the VM) break and step like sync code.

## Status

The frontend (lexer → parser → binder), the type checker, the typed-bytecode
VM, and the Cranelift Tier-1 JIT are all in place. `mersey run app.mersey`
executes natively; the same engine runs as a WASM polyfill in stock browsers
and natively inside four browser forks (Mersey Blink / Gecko / Servo /
Ladybird), all checksum-verified bit-for-bit against each other. Twenty-five
web-platform workloads are benchmarked across every leg (`bench/web`).

Mersey is **experimental and pre-1.0** — behaviour may change before 1.0.0, and
the browser builds are not for production use. The conformance suites are the
contract; see `ROADMAP.md`.

```sh
./web/build-and-test.sh          # build engine to WASM + headless e2e test
cd web && python3 -m http.server # then open http://localhost:8000
```

## License

Apache-2.0 — see `LICENSE` and `NOTICE`. The browser-fork overlays are
derivative works of their upstream projects and keep those licenses (Chromium
BSD-3-Clause, Firefox/Servo MPL-2.0, Ladybird BSD-2-Clause).
