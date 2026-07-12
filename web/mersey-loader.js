/* Mersey Stage A loader polyfill (docs/architecture/browser-integration.md).
 *
 * Executes <script type="text/mersey"> tags via the Mersey engine compiled
 * to WebAssembly. This is the only JavaScript the page author needs, and it
 * disappears entirely at Stage B when Chromium hosts the engine natively.
 *
 * Usage:
 *   <script type="module" src="mersey-loader.js" data-engine="mersey_wasm.wasm"></script>
 *   <script type="text/mersey" src="app.mersey"></script>
 */
import { startEngine } from "./mersey-engine.js";

// `document.currentScript` is null in module scripts; find our own tag.
const selfTag = document.querySelector('script[src$="mersey-loader.js"]');
const engineUrl = (selfTag && selfTag.dataset.engine) || "mersey_wasm.wasm";

async function boot() {
  const engine = await startEngine({ engineUrl, realm: globalThis });
  for (const tag of document.querySelectorAll('script[type="text/mersey"]')) {
    const spec = tag.getAttribute("src");
    const source = spec ? await (await fetch(spec)).text() : tag.textContent;
    // A src'd script may import other .mersey modules: load the graph.
    const status = spec
      ? await engine.runGraph(spec, source)
      : engine.run(source);
    if (status !== 0) {
      console.error(`[mersey] ${spec || "<inline>"}: exited with status ${status}`);
    }
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
