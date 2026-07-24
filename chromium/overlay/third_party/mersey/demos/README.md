# Mersey native demos

Hand-runnable demo pages for the Mersey Chromium fork. Each is a single HTML
file whose entire program is an inline `<script type="text/mersey">` — no WASM
polyfill, no hand-written JavaScript — so it exercises the real
`MerseyScriptRunner` path end to end.

| page | shows |
|---|---|
| `counter.html` | a class with a constructor, a `readonly` field and a click handler mutating state |
| `todo.html` | `Map`, `document.createElement`/`appendChild`, per-item click-to-remove closures, live re-render |

Open one in the fork's built shell:

```bash
out/mersey-arm64/content_shell third_party/mersey/demos/counter.html
```

or load it in the built `Chromium.app`.

These are demos, not gated tests — they live outside `blink/web_tests/` so
`run_web_tests.py` never sees them. The deterministic, `dumpAsText`-checked
versions of the same programs are the layout tests in
`//third_party/blink/web_tests/mersey/` (`counter.html`, `todo.html`).
