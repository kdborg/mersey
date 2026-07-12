# Mersey Language Specification — 5. Security Model

Security is a language property here, not only an engine property. The rules
below are normative for every conforming implementation.

## 5.1 Language-level guarantees

- **No dynamic code evaluation.** No `eval`, no `Function(string)`, no
  string-to-code path of any kind. Every instruction the engine ever executes
  comes from a `.mersey` file that was visible at load time. This eliminates
  the largest XSS amplification class outright.
- **No prototype pollution.** With no prototypes and sealed class shapes,
  the `__proto__` / `Object.prototype` attack family does not exist.
- **Memory safety by construction.** No pointers, bounds-checked array and
  string access (the JIT elides checks it can prove, never ones it can't),
  no uninitialized reads (definite-assignment analysis), no type confusion
  (sound static types + checked casts).
- **Defined arithmetic.** No undefined behavior anywhere in the spec (§3.6);
  a Mersey program's behavior is fully deterministic modulo declared I/O.
- **Access control is real.** `private`/`protected` are enforced at runtime
  at every boundary — reflection, serialization, the embedding API, and
  debugger surfaces cannot read private state without an explicit,
  host-granted debug capability.

## 5.2 Engine requirements

- **W^X JIT.** No memory page is ever writable and executable
  simultaneously; code pages are remapped read-execute before first run.
- **Sandbox-friendly.** The engine makes no syscalls of its own beyond
  memory management; all I/O flows through the host interface, so it runs
  inside Chromium's renderer sandbox / seccomp unchanged.
- **Heap isolation.** Each execution context (browser origin, or embedder
  isolate) has its own heap; cross-context references are impossible by
  construction, mirroring V8 isolates.
- **Hardening:** pointer compression with heap-base randomization, guard
  pages around the heap and JIT regions, CFI-compatible codegen on
  platforms that support it.

## 5.3 Standalone runtime: capabilities

The standalone `mersey` runtime is deny-by-default (the Deno model, which is
the proven design for this):

```
mersey run app.mersey                       # no fs, no net, no env
mersey run --allow-read=./data --allow-net=api.example.com app.mersey
```

Capabilities are also queryable and droppable from inside the program
(`std:caps`), so a program can shed privileges after initialization.

## 5.4 Browser profile

- **CSP integration.** `.mersey` scripts are governed by Content-Security-
  Policy exactly as scripts (`script-src` applies; a `'mersey-eval'`-style
  escape hatch deliberately does not exist because the language has no eval).
- **Same-origin + CORS** for module fetches, identical to ES modules
  (`crossorigin` attribute honored; modules always fetched with CORS mode).
- **Subresource integrity** (`integrity=`) supported on `<script>` tags and
  on `import` manifests.
- **No ambient authority.** DOM and Web APIs are imported explicitly
  (`import { document } from "browser:dom";`) — a module's capabilities are
  visible in its import list, which makes supply-chain review tractable.
- **Permissions-Policy** is honored for powerful APIs, as for JS.

## 5.5 Supply chain

- Lockfiles with content hashes for all module resolution outside the
  standard library.
- `mersey audit` reports each dependency's imported capability surface
  (which `std:`/`browser:` modules it touches) — computable exactly because
  imports are static.
