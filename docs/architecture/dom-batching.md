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

### Encoding (as shipped)

A batch is three parallel arrays: a flat **int32 op stream** in groups of four
`(op, a, b, c)`, a **UTF-16 string pool** (`msy_str16` = ptr+len entries — tag
names and texts, so no per-string crossing either), and a **live-node array** of
engine handles. Nodes created earlier in the same batch are named by **temp id**
— a small non-negative index into the batch's own created list — so the
reconciler can create a node and reference it in a later `append`/`insert`
without a round trip. The two namespaces are disjoint by sign, so a single int32
operand suffices:

- `ref >= 0` — a **temp id**: an index into the nodes this batch creates.
- `ref < 0` — a **live node**: `nodes[-ref - 1]` (an engine handle).
- `ref == MSY_DOM_NULL` (`INT32_MIN`) — "no node" (insertBefore's append case).

The op codes (`MSY_DOM_*`, in `mersey.h`):

```
CREATE   (0)  a = str index of tag name   b = temp id assigned   c = document operand
SET_TEXT (1)  a = target node operand      b = str index of text
APPEND   (2)  a = parent operand           b = child operand
INSERT   (3)  a = parent  b = child        c = ref operand (or MSY_DOM_NULL)
REMOVE   (4)  a = parent operand           b = child operand
```

`CREATE` names the document in operand `c` (a live node), so a fork needs no
ambient-document accessor — the reconciler registers `document` once as a live
node and every `create` references it.

### ABI hook (host table, ABI v10)

```c
typedef struct { const uint16_t *ptr; uint32_t len; } msy_str16;

/* Apply a batch of DOM mutations in one crossing. `ops` is 4*nops int32; a node
 * created with temp id i has its live handle written to created_out[i] (up to
 * created_cap, which the engine sizes to the CREATE count). Returns the number
 * created. NULL declines -> std:dom.apply throws (no batched path on this host). */
size_t (*web_apply)(void *data, const int32_t *ops, size_t nops,
                    const int64_t *nodes, size_t nnodes,
                    const msy_str16 *strs, size_t nstrs,
                    int64_t *created_out, size_t created_cap);
```

`MSY_ABI_VERSION` -> `10`. Every host bumps its constant and either implements
`web_apply` or leaves it `NULL`. Because the version is checked strictly, all
hosts that link a v10 engine must be rebuilt at v10. When `web_apply` is NULL the
engine **replays** the batch one op at a time through the reflective web bridge
(`createElement` / `textContent` / `appendChild` / `insertBefore` / `removeChild`)
— identical result, no crossing-collapse — so `std:dom.apply` works on any host,
not just the four forks + wasm host that implement the batched path. (Verified on
the engine leg: forcing the fallback checksums bit-for-bit against the batched
path via the `__MERSEY_NO_WEB_APPLY` test hook.)

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
