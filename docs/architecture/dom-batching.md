# Batched DOM mutations — `web_apply` (ABI v10)

## Problem

The engine reaches the DOM one operation at a time, and each op is a crossing of
the C ABI boundary (`web_new` to create, `web_call` to append / set text / insert
/ remove, `web_get` to read back). For compute that is irrelevant, but for a
UI framework — a keyed reconciler doing hundreds of `createElement` /
`appendChild` / `textContent` / `insertBefore` per render — the per-op crossing
cost dominates. The `frameworkui` benchmark shows it: native forks (~50–99 ms)
barely beat the WASM polyfill (~112 ms) and lose ~12–25× to in-process JS
(~4 ms), because JS pays no crossing.

Removing the reads (a smarter reconciler) does not help the native case: on a
fork the reads are cheap C++ calls, and the in-engine bookkeeping to avoid them
costs more than it saves. The native cost is in the **writes**, and the only way
to cut those is to stop crossing per write.

## The primitive

`web_apply` applies a whole render's mutations in **one** crossing. The engine
accumulates a mutation list in-engine (no crossings), then submits it once; the
host decodes and applies it against the real DOM and hands back the handles of
the nodes it created.

### Encoding

A batch is an op stream plus a UTF-16 string pool (texts and tag names, so no
per-string crossing either). Nodes created earlier in the same batch are named
by **temp id** — a small non-negative index into the batch's own created list —
so the reconciler can create a node and reference it in a later `insert` without
a round trip. Existing nodes are named by their normal engine handle. The two
namespaces are disjoint by sign: `handle >= 0` is a temp id, `handle < 0`
(offset) is a live handle — or a dedicated `is_temp` flag per operand.

```c
typedef struct {
    uint32_t op;   /* MSY_DOM_CREATE / SET_TEXT / APPEND / INSERT / REMOVE */
    int64_t  a;    /* CREATE: str-pool offset<<32 | len of the tag name
                    * SET_TEXT: target node    APPEND/REMOVE: parent
                    * INSERT: parent */
    int64_t  b;    /* CREATE: temp id assigned  SET_TEXT: str-pool ref
                    * APPEND/REMOVE: child      INSERT: child */
    int64_t  c;    /* INSERT: ref node (or MSY_DOM_NULL to append) */
} msy_dom_op;
```

Operands that name a node carry a tag bit (temp vs live handle). `str-pool ref`
is `offset<<32 | len` into `strpool` (UTF-16 code units).

### ABI hook (host table, ABI v10)

```c
/* Apply a batch of DOM mutations in one crossing. `created_out[i]` receives the
 * live handle of the node created with temp id i (up to `created_cap`); returns
 * the number of nodes created. NULL declines -> the engine falls back to the
 * per-op web_new_u16 / web_call_u16 path, identical result. */
size_t (*web_apply)(void *data,
                    const msy_dom_op *ops, size_t nops,
                    const uint16_t *strpool, size_t strpool_len,
                    int64_t *created_out, size_t created_cap);
```

`MSY_ABI_VERSION` -> `10`. Every host bumps its constant and either implements
`web_apply` or leaves it `NULL` (safe: per-op fallback). Because the version is
checked strictly, all hosts that link a v10 engine must be rebuilt at v10.

## Engine + language surface

Mersey code drives it through a `browser:dom` batch builder — the reconciler
builds the batch, then commits:

```mersey
const b = document.batch();
const el = b.create("div");          // returns a batch-local node ref
b.setText(el, `row ${id}`);
b.insert(parent, el, ref);           // ref may be null (append)
b.remove(parent, gone);
b.apply();                           // ONE crossing; el is now a live handle
```

`browser:dom` gains a `DomBatch` type (frontend ambient type + interp intrinsic).
The builder accumulates `msy_dom_op`s and a string pool in-engine; `apply()`
calls `web_apply`, then rewrites its batch-local refs to the returned live
handles so they can be used in the next render. When the host's `web_apply` is
NULL, `apply()` replays the ops through the existing per-op hooks — same result,
no batching win, but it always works.

## Fork host implementations

Each fork decodes the op stream and applies it with its own DOM + handle table:

- **Blink** (`mersey_script_runner.cc`) — `createElement` / `appendChild` /
  `setTextContent` / `insertBefore` / `removeChild` on `blink::Node`, allocating
  node handles as today. (This also finally lets Mersey Blink run `frameworkui`,
  which it can't today — its per-op bridge lacks `removeChild`/`insertBefore`.)
- **Gecko** (`dom/mersey/MerseyScriptRunner.cpp`) — the same over `nsINode`.
- **Servo** (`components/script/mersey/mod.rs`) — over Servo's DOM in Rust,
  reusing the handle table.
- **Ladybird** (`Libraries/LibWeb/Mersey/MerseyScriptRunner.cpp`) — over LibWeb.

The engine stub (`bench/web/engine-child.mjs`) and the reflective polyfill bridge
(`web/mersey-bridge.js`) implement it too, so the batched path is verified on the
engine and polyfill legs before any fork is rebuilt.

## Rollout (coordinated, staged)

1. ABI v10 + engine batch builder + stub + polyfill bridge. Verify + measure the
   crossing-collapse on the engine and polyfill legs. Do **not** commit the v10
   engine until the fork overlays are ready (a v10 engine with a v9 fork host
   fails the version check).
2. Fork host `web_apply` in all four overlays; rebuild each (serially,
   machine-safely — Chromium is ~3 h); re-measure `frameworkui` native; commit
   the whole v10 change together.

The `frameworkui` twin keeps its current per-op reconciler as the compatibility
path and gains a batched path used when `document.batch` is available, so the
benchmark shows both.
