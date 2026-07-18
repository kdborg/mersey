# The debugger

Two halves, one engine surface. Everything a debugger front-end needs — DAP
in an editor, DevTools in a browser — drives the same reporting layer, and
all *policy* (breakpoints, stepping, what to show) lives in the front-end
adapter, never in the engine.

## The engine surface: `DebugHook`

`Interp::set_debug_hook(Box<dyn DebugHook>)` installs the hook and forces the
pure tree-walker (`use_vm = false`) for sync code. From then on the engine
calls `on_stmt` before every executable statement with:

- **position** — statements carry no spans, so `mersey_front::ast::
  stmt_first_pos`/`expr_first_pos` derive one from the first positioned node
  (a name, a literal) under the statement, left to right. Blocks and `try`
  report through their inner statements; `Empty` never reports.
- **the call stack** — `DebugPause::frames`, outermost first. Tree-walked
  calls maintain the diagnostic frame stack only while a debugger is attached
  (the `Frame` RAII guard; the module top-level contributes `<module>`), so
  the undebugged walker pays nothing. Each statement callout also refreshes
  the innermost frame's position — which is exactly what leaves every *outer*
  frame holding its call-site line, the thing a stack view shows.
- **locals, on demand** — a closure `locals(i)` snapshots frame `i` counted
  from the top: the scope chain (innermost first, name-sorted,
  display-formatted). Frames register their environment beside the diagnostic
  frame (`debug_envs`), so any frame can answer, and a hook that never pauses
  never pays for a snapshot.

**Pausing IS blocking inside the callout.** The engine sits mid-statement
until `on_stmt` returns. There is no pause/resume protocol in the engine;
"stopped at a breakpoint" is nothing more than the adapter not returning yet.

## Async and generator bodies: the VM callout

Suspension belongs to the VM (`await`/`yield` capture whole VM states), so
those bodies cannot tree-walk. Instead the VM's dispatch loop — which already
computes `pos_at(pc)` per op for error attribution — reports **line changes**
through the same hook when a debugger is attached (`debug_vm_stmt`; one
local-bool branch per op otherwise). Chunks retain a slot-name table
(`slot_names`, `#`-prefixed compiler temps excluded), and the callout serves
the live frame slots as the innermost scope — slot-resolved locals are
registers the scope chain never sees.

The net effect: sync code gets statement-grained callouts from the walker,
async/generator code gets line-grained callouts from the VM, and one hook
sees both.

## The standalone half: `mersey dap`

`crates/mersey_cli/src/dap.rs` is a Debug Adapter Protocol server on
stdin/stdout (framing shared with the LSP). The interpreter runs on the
session thread; a reader thread feeds parsed requests through a channel that
the hook drains non-blockingly at each statement. A stop (breakpoint hit,
step condition, pause request) blocks inside the hook and services
`stackTrace`/`scopes`/`variables` from the paused statement until a resume
command arrives.

Policy details, all adapter-side:

- **breakpoints** are per-source with DAP's replace semantics, matched to
  executing modules by exact/suffix/basename comparison — an editor's
  absolute path finds a graph-relative spec and vice versa;
- **stepping** is depth arithmetic on `frames.len()`: *over* = stop at the
  same depth or shallower, *in* = stop anywhere, *out* = stop shallower;
- **variables** encode `variablesReference = frame × 64 + scope + 1`, and
  scope snapshots are cached per pause;
- the debuggee's `print` becomes DAP `output` events (stdout is the
  protocol channel).

`editors/vscode-mersey` is a thin wrapper pointing VS Code's DAP client at
`mersey dap`, plus a TextMate grammar; any editor that speaks DAP directly
takes the command as-is. The end-to-end contract is pinned by
`crates/mersey_cli/tests/dap.rs` (a scripted session over the real binary)
and `crates/mersey_interp/tests/debug.rs` (exact callout sequences, stack and
locals at a break line, passive-hook invariance, async-body reporting).

## The browser half: source maps

In a browser the transpiled-JS backend is the default execution path, and
its debugging story is standard web tooling rather than a custom protocol:
the emitter records a mapping per statement *and per dispatch site*
(`$rt.call/get/index` — pointing at the erroring expression, matching the
engine's own error convention), and every emitted module carries an inline
Source Map v3 with `sourcesContent`, named by its real path. DevTools
therefore lists `.mersey` sources, takes breakpoints in them (verified over
CDP: a breakpoint on the generated line mapped from a source line pauses a
served page on a real click), and maps stack frames.

The same maps power the transpiled runtime's rich errors: `$rt`'s uncaught
handler fetches the blob modules a JS stack names, VLQ-decodes their inline
maps, resolves each frame to the nearest mapping at-or-before its column,
and renders the engine-style Mersey stack plus a code frame with a caret
under the erroring expression.

A CDP `Debugger`-domain agent inside a fork (driving `DebugHook` directly,
for the engine-native path) remains possible on the same surface, but the
source-map route covers the browser debugging story for the default path
without any fork-side protocol work.

## Limits, recorded

- VM-frame locals show the slot table and the scope chain; a value living
  purely in an unnamed VM temporary is not shown (temps are excluded by
  design).
- The walker's callouts are per-statement, not per-expression; stepping
  granularity in sync code is the statement.
- Installing a hook forces tree-walking for sync code: debugging trades speed
  for statement-grained reporting, deliberately.
