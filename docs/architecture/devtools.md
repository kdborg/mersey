# DevTools integration

The goal: in every browser, a console whose language is a dropdown —
JavaScript or Mersey — with the debugger and the rest of DevTools working
against Mersey sources. No variable is shared between the two languages.

## Isolation is structural, not enforced

Nothing needs to *police* the "no shared variables" rule. The engine never
holds a JS object: it holds integer handles and calls host hooks
(`web_get/web_set/web_call/…`), so a Mersey binding cannot name a JS
binding and vice versa. A Mersey console turn runs against the page's Mersey
context; a JS turn runs against the JS realm. Both reach the *DOM*, because
both go through the host — which is the same reason two iframes can touch one
document without sharing scopes.

`ReplSession::completions` follows the same line: it offers the session's own
top-level names and the `std:console` prelude, and deliberately excludes
`window`/`document`, because the JS realm is not Mersey's view.

## The dropdown already exists — the work is publishing a second context

The instinct is to add UI. In three of four browsers that is wrong: the
console's context selector is already there, and it is populated by whatever
execution contexts the engine advertises. So the integration is *publish a
second execution context named "Mersey"*, and route evaluation in it to
`msy_context_repl_turn`.

Where each browser stands:

| Browser | Protocol | Front-end | Dropdown route |
|---|---|---|---|
| Chromium | CDP | in-tree (fork owns it) | a dedicated `Mersey` domain — see the correction below |
| Firefox | RDP | **in-tree** (fork owns it) | fork's own `devtools/client/webconsole` — a real language selector, plus the RDP plumbing behind it |
| Servo | RDP | none — Firefox's | server-side only; the *client* that renders the switch is Firefox's front-end |
| Ladybird | RDP | none — Firefox's | same |

That last column is the load-bearing observation: **Servo and Ladybird ship no
DevTools UI**. They are RDP *servers*; the front-end is Firefox's DevTools
connecting over the wire. Neither can grow a dropdown on its own — whatever
Firefox's front-end offers is what their users see.

Consequence for sequencing: the Firefox fork's front-end is the UI for three
of the four targets. Doing Firefox first yields a client that can then drive
Servo and Ladybird as their server halves land; doing Ladybird first yields a
server whose dropdown cannot be demonstrated until that client exists.

## Ladybird: the server half, verified

`ladybird/apply-devtools.sh` wires the console through the fork's three
layers — `ConsoleActor` → `DevToolsDelegate::evaluate_mersey` →
`Application` → the `mersey_console_input` IPC → `PageClient` →
`Web::Mersey::repl_turn` in the realm's engine. Eleven guarded edits, each
aborting its file rather than writing a partial patch.

Language selection: a Mersey-aware front-end sends `"language":"mersey"` on
`evaluateJSAsync`. Because no such client exists yet, a leading `mersey>`
also selects it — which is what makes this half testable from a *stock*
Firefox DevTools console today.

`ladybird/test-devtools-console.mjs` drives a real Ladybird over the real
protocol. Verified:

```
JS      2 + 3            => 5
MERSEY  6 * 7            => "42"
MERSEY  let x: int32 = 5 => undefined      (a declaration)
MERSEY  x * 3            => "15"           (the session persists across turns)
JS      globalThis.jsOnly = 99 => 99
MERSEY  jsOnly           => error[E0301]: cannot find name `jsOnly`
JS      typeof x         => "undefined"
```

The last two lines are the isolation contract, demonstrated in both
directions: a JS global is invisible to Mersey, and a Mersey binding is
invisible to JS.

Known gap: an errored turn comes back as the result *string* rather than a
console exception — `exception`/`exceptionMessage` are still hardcoded null
in `ConsoleActor`'s `evaluationResult`. Autocomplete in Mersey mode is also
unwired; `msy_context_repl_complete` and `Web::Mersey::repl_completions`
exist for it, but the actor's `autocomplete` handler still returns empty (it
does for JS too).

## Firefox: the dropdown, and a build-shaped blocker

The fork now carries the whole path, client to engine:

- `devtools/client/webconsole/components/Input/EvaluationContextSelector.js`
  grows a **Language section** — JavaScript / Mersey — and the button label
  gains a `· Mersey` suffix so the mode is visible without opening the menu.
  It belongs in *this* menu rather than a toolbar of its own precisely because
  a Mersey realm is a second evaluation context, not a display preference.
- `evalLanguage` in the webconsole UI reducer, with `EVAL_LANGUAGE_SET` and
  `setEvalLanguage`.
- `actions/input.js` threads the language into `scriptCommand.execute`, and
  **suppresses eager evaluation in Mersey mode** — every turn appends to the
  session's growing module, so evaluating as the user types would mutate the
  program being written. Eager evaluation is a JS-only affordance.
- `devtools/shared/specs/webconsole.js` declares `language` (the RDP request
  is typed — an undeclared field is dropped on the floor).
- `devtools/server/actors/webconsole.js` routes a Mersey turn to
  `_evaluateMersey`, which never touches the JS evaluator: `!`-prefixed
  diagnostics and `runtime error:` both come back as console exceptions, an
  echo as a plain string, a declaration as `undefined`.
- `ChromeUtils.merseyReplTurn(window, source)` (chrome-only) is the door from
  the privileged actor into `MerseyScriptRunner::For(doc)->ReplTurn(...)`.

All eight JS files are `mach lint` clean.

**Verified end to end** (`firefox/test-devtools-console.mjs`, real Firefox,
headless, over RDP):

```
JS      2 + 3                  => 5
MERSEY  6 * 7                  => "42"
MERSEY  let x: int32 = 5;      => undefined
MERSEY  x * 3                  => "15"      (the session persists)
JS      globalThis.jsOnly = 99 => 99
MERSEY  jsOnly                 => EXCEPTION: error[E0301]: cannot find name `jsOnly`
JS      typeof x               => "undefined"
```

Isolation holds in both directions, and — unlike the Ladybird path — a failed
turn arrives as a real console *exception*, because the actor fills
`exception`/`exceptionMessage` rather than returning a result string.

**Two latent fork bugs this uncovered**, neither caused by the console work:

- `dom/mersey/rust/Cargo.toml` pointed at
  `../../../../Work/mersey/crates/mersey_capi`, a path that only resolves if
  the fork sits at `$HOME/firefox`. It has moved under `$HOME/Work/mersey/`,
  so the doubled prefix made the crate unresolvable — **the fork had not been
  buildable since the layout moved**, which is also why the checkout was
  sitting on an artifact build.
- The vendored `dom/mersey/include/mersey.h` was pinned at ABI v8 while the
  crate moved to v9, which `MOZ_RELEASE_ASSERT(msy_abi_version() ==
  MSY_ABI_VERSION)` would have caught at runtime. `dom/mersey/refresh.sh`
  re-syncs it; run it whenever the ABI changes.

Building the fork requires turning OFF `--enable-artifact-builds` in
`mozconfig` (artifact builds download Mozilla's prebuilt C++, which contains
no `dom/mersey`). A full build is ~35 minutes on an M-series Mac.

## Chromium: a dedicated CDP domain

**A correction to the table above.** The original claim here — that publishing
an execution context named "Mersey" would light up the console's context
selector for free — is **false for Chromium**. The `Runtime` domain is
implemented by *v8_inspector*, not Blink, and its execution contexts are real
V8 contexts; Blink cannot inject a non-V8 one. Nor was the cheap alternative
available: unlike Gecko and Ladybird, the Chromium fork never installed a
page-visible `mersey()` global — its REPL ran through DOM attributes
(`data-mersey-repl-src` / `data-mersey-repl`), a bench-harness shim with no
callable API.

So Chromium gets its own domain, which is the cleaner answer anyway and the
shape the debugger agent will want later:

```
Mersey.evaluate(expression) -> {result, isError, isCompileError}
Mersey.completions()        -> {names}
```

`InspectorMerseyAgent` (`core/inspector/`) decodes the ABI's reply prefixes —
`!` = rejected by the checker, `runtime error:` = threw — into **typed
protocol fields**, so the front-end never parses strings. `MerseyScriptRunner`
grew a callable `ReplTurn(String)` / `ReplCompletionsJson()`, and the
attribute shim now sits on top of those rather than duplicating the FFI.

**Adding a Blink CDP domain takes FIVE registration points**, not four — the
fifth is easy to miss because it fails only at link time:

1. `public/devtools_protocol/domains/<Name>.pdl`
2. an `include` line in `browser_protocol.pdl`
3. `core/inspector/inspector_protocol_config.json`
4. the agent's sources in `core/inspector/build.gni`
5. **the generated `<name>.cc` in `core/inspector/BUILD.gn`'s `outputs` list**
   — that list is enumerated by hand. Codegen emits the file either way and
   the header gets picked up, so the agent *compiles*; the `.cc` is simply
   never linked, and you get undefined `Metainfo::domainName` and
   `Dispatcher::wire`.

Verified end to end (`chromium/test-devtools-console.mjs`, real Chromium,
headless, over CDP — a dependency-free WebSocket client, since the repo
carries no `node_modules`):

```
JS      2 + 3                  => 5
MERSEY  6 * 7                  => "42"
MERSEY  let x: int32 = 5;      => undefined
MERSEY  x * 3                  => "15"
JS      globalThis.jsOnly = 99 => 99
MERSEY  jsOnly                 => COMPILE ERROR: error[E0301]: cannot find name `jsOnly`
JS      typeof x               => "undefined"
MERSEY  completions            => ["console","x"]
```

That last line is the completions contract holding in a browser for the first
time: the session's own binding plus the `std:console` prelude, and no
`window`/`document`.

Still to do here: the language selector in
`third_party/devtools-frontend/src/front_end/panels/console` (in-tree, its own
build), and the engine is still ABI **v8** — `scripts/build-macos-arm64.sh
chromium` refreshes the dylib to v9 and with it the debug surface.

## Chromium: debugging through the Sources panel

The Sources panel's *pause chrome* — call-frame sidebar, scope tree, F8 —
is built on `SDK.DebuggerModel` and V8 `RemoteObject`s, so Mersey can never
inhabit it without impersonating a V8 debugger. Everything else Sources
offers, Mersey now uses for real:

- **A "Mersey" project in the Sources navigator** serves each executed script
  as a virtual `.mersey` file — content straight from `Mersey.getScripts`,
  editor lines mapping 1:1 to engine lines. This also sidesteps a Chromium
  reality: the page's own resource content is often unavailable in Sources
  ("Content unavailable. Resource was not cached"), so mapping against the
  HTML file alone would show an empty editor.
- **Gutter clicks set Mersey breakpoints.** A bridge (in the `mersey-meta`
  module) listens to `BreakpointManager`, translates document/virtual lines
  to engine lines (`Mersey.Script.startLine` carries the `<script>` tag's
  position, recorded from the parser's `TextPosition` at the `ScriptLoader`
  hook), and pushes `Mersey.setBreakpoints`.
- **Pauses reveal in Sources**; breakpoints persist with Sources' own storage
  and **survive reload** (re-armed on the `Load` event).
- Stack, scopes, and stepping stay in the **Mersey panel**.

The virtual file is named `<page>.mersey` (a tag-line suffix only
disambiguates a second inline script). On pause the drawer auto-opens with
the Mersey panel — stack, scopes, stepping — because a pause that presents
nothing on screen is indistinguishable from a hung page. The Mersey panel
lives ONLY in the drawer; Sources is the debugging surface. The bridge is
the SOLE writer of engine breakpoints: `setBreakpoints` REPLACES, so a
second writer (the panel once pushed its own set on re-attach) silently
clobbers gutter breakpoints on reload.

**External modules debug identically** (`chromium/test-devtools-external.mjs`):
the native graph loader records every fetched module — full URL, the engine's
module *spec* (what pause frames report), and source — so `todo.html`'s
`demo/todo.mersey` appears in the Sources tree under its real URL, editor
lines are engine lines (no offset), and gutter breakpoints pause and survive
reload. The graph loads asynchronously after the page, so the bridge retries
its script refresh (+1.5s/+4s) rather than adding a notification protocol.
Frame-to-script matching goes spec → URL suffix → basename → first script
(the engine reports an empty module name for the entry frame).

**The JS-identical flow works too** (`chromium/test-devtools-html-flow.mjs`):
breakpoints in the page's *own HTML file* at real document lines, and stepping
via the **standard** Sources debugger buttons and shortcuts. Two conditions
made this possible:

- The document's content is servable once the page has been (re)loaded with
  DevTools open — "Resource was not cached" only afflicts loads DevTools
  never saw. The bridge maps HTML-file gutter lines through
  `Script.startLine`, and reveals into the HTML file when the user's
  breakpoints live there.
- The fork owns `SourcesPanel`'s action delegates: `debugger.toggle-pause` /
  `step-over` / `step-into` / `step-out` (and their F8/F10/F11 shortcuts)
  route to the Mersey engine **only while `__merseyDebug.lastPause` is set**
  — V8 is not paused then, so JS debugging is untouched. The bridge enables
  the stepping actions on a Mersey pause (they are otherwise greyed out,
  since no V8 pause exists).

Pinned by `chromium/test-devtools-sources.mjs`, which drives the real UI:
navigator → open `todo-native.mersey` → gutter click on line 20 →
`lastLines:[19]` pushed → real page click pauses → drawer opens showing
`paused (breakpoint)` with locals → Resume button → reload → pause again.

Three hard-won front-end lessons, recorded because each failed *silently*:

1. **Meta modules evaluate before `MainImpl` boots, and boot replaces app
   singletons.** A bridge constructed at module load observed an orphan
   `TargetManager` and never heard about any target. The blessed hook is
   `Common.Runnable.registerLateInitializationRunnable`.
2. **A module-load-time throw in a meta file kills the whole DevTools boot**
   (observed as an empty window, no tabs). Construct nothing eager; wrap
   everything; surface errors into a `window.__merseyDebug` handle — release
   DevTools inlines modules, so app singletons are unreachable from outside
   and an in-page diagnostic is the only ground truth.
3. **A workspace project without target/frame attribution is filtered out of
   the Page navigator** — files exist but never render. Use
   `NetworkProject.setTargetForProject` + `setInitialFrameAttribution`.

## The debugger half

Already built engine-side and shared by every fork
(`docs/architecture/debugger.md`): `mersey_interp::debug::DebugController`
holds breakpoint/step policy once, and `msy_context_debug_*` (ABI v9) exposes
it across the C boundary. A fork's agent translates wire format and nothing
more:

- CDP `Debugger.setBreakpointByUrl` → `msy_context_debug_set_breakpoints`
- CDP `Debugger.paused` ← the blocking `on_paused` callback's JSON snapshot
- `stepOver`/`stepInto`/`stepOut`/`resume` → the resume family

Pausing is blocking by design, so a fork pauses by running a nested message
loop inside `on_paused` — exactly what V8 does. Nothing else in the engine
needs a state machine.

## Recorded limits

- **The polyfill path has no pause surface.** The WASM engine runs on the
  page's main thread, where JS cannot spin a nested event loop; a blocking
  hook would freeze the page rather than pause it. Polyfill pages debug
  through the transpiler's source maps instead.
- **Evaluate-in-frame is not wired.** A console turn while paused runs
  against the session's module top level, not the paused frame's scope. The
  pause snapshot *reports* every frame's locals, so the variables view is
  complete; typing an expression that reads a paused local is the gap.
- Async and generator bodies run on the VM and report line changes rather
  than statements (see the debugger doc).
