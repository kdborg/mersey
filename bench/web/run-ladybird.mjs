// Stock-Ladybird leg of the web-platform benchmark (js / WASM-polyfill /
// transpiled-JS in UNMODIFIED Ladybird) — NOT YET IMPLEMENTED.
//
// Unlike Chromium/Firefox (Playwright) and Servo (headless servoshell reading
// console.log from stdout), current Ladybird ships no `--headless URL` binary:
// its headless runner is `test-web`, a WPT-style harness that loads pages as
// file:// or via a built-in http echo server and captures output per test.
//
// The native leg (run-native-ladybird.mjs) works around this cleanly: its pages
// are self-contained inline `text/mersey` scripts (file://), and the engine's
// host `print` hook writes RESULT straight to WebContent's stdout, captured in
// each test's `.logs.html`. See ladybird/README.md.
//
// The STOCK pages here (bench/web/pages/{js,poly,tjs}.html) instead `import()`
// the workload/engine over absolute http paths (`/bench/web/js/…`, `/web/…`),
// which test-web's echo server does not serve from this repo. Wiring the stock
// leg therefore needs one of:
//   - inlining each workload into a generated Text test (feasible for `js`;
//     `poly`/`tjs` also need the WASM engine / transpiler inlined), then reading
//     RESULT the same way the native runner does; or
//   - pointing test-web's fixture server at this repo root.
// Until then this leg is intentionally not collected (results.ladybird.json is
// absent, so report.mjs / REPORT.md show "—" for stock Ladybird — never a guess).
console.error(
  "run-ladybird.mjs: stock Ladybird leg not implemented (needs the test-web + http harness).\n" +
  "The native fork leg is measured — run: node bench/web/run-native-ladybird.mjs\n" +
  "See ladybird/README.md, 'Not yet done'.");
process.exit(1);
