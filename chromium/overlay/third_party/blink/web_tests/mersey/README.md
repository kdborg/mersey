# Mersey web tests

`<script type="text/mersey">` executed by the Mersey engine inside Blink.
Run with `content_shell` via `run_web_tests.py mersey/`.

The Mersey engine's own conformance suites — the language, three execution
tiers, a differential fuzzer, and an ABI test that drives the exact C symbols
`//third_party/mersey` exports — live in the engine repository. These tests
cover the *Blink* half: that a script element of this type reaches the engine,
that the DOM bindings land on real Elements, and that a diagnostic surfaces on
the console instead of crashing the renderer.

| test | covers |
|---|---|
| `inline-script.html` | an inline `text/mersey` script runs and writes through the DOM |
| `events.html` | a Blink click dispatches into a Mersey closure that mutates state |
| `counter.html` | a class with a constructor + `readonly` field driving a click counter |
| `todo.html` | `Map`, `createElement`/`appendChild`, click-to-remove closures, `HTMLInputElement.value` |

The `demos/` pages under `//third_party/mersey` are the same programs as
hand-runnable, un-gated versions.
