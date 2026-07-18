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

/* Subresource Integrity (spec §5.4). Blink enforces this natively for
 * <script> in Stage B; in the polyfill the loader must do it, because a
 * Mersey source is fetched as data, not as a script. */
async function fetchWithIntegrity(url, integrity, kind) {
  const resp = await fetch(url, { credentials: "same-origin" });
  if (!resp.ok) throw new Error(`${kind} ${url}: HTTP ${resp.status}`);
  const buffer = await resp.arrayBuffer();
  if (integrity) {
    const ok = await verifyIntegrity(buffer, integrity);
    if (!ok) {
      throw new Error(
        `${kind} ${url}: integrity check failed (expected ${integrity})`);
    }
  }
  return new TextDecoder().decode(buffer);
}

async function verifyIntegrity(buffer, integrity) {
  // "sha256-… sha384-…" — any one match is sufficient, as in the SRI spec.
  for (const token of integrity.trim().split(/\s+/)) {
    const [alg, expected] = token.split("-");
    const name = { sha256: "SHA-256", sha384: "SHA-384", sha512: "SHA-512" }[alg];
    if (!name || !expected) continue;
    const digest = await crypto.subtle.digest(name, buffer);
    const actual = btoa(String.fromCharCode(...new Uint8Array(digest)));
    if (actual === expected) return true;
  }
  return false;
}

/* Content-Security-Policy (spec §5.4). A `.mersey` source is not a script
 * to the browser, so `script-src` does not govern its fetch. The loader
 * therefore enforces the page's own policy itself: a Mersey source may only
 * be loaded from an origin the page's script-src (or default-src) allows.
 * There is deliberately no eval-style escape hatch — the language has none. */
function cspAllows(url) {
  const meta = document.querySelector('meta[http-equiv="Content-Security-Policy"]');
  const policy = meta && meta.getAttribute("content");
  if (!policy) return true; // no page policy: nothing to enforce
  const directives = Object.fromEntries(
    policy.split(";").map((d) => {
      const [name, ...values] = d.trim().split(/\s+/);
      return [name, values];
    }),
  );
  const sources = directives["script-src"] ?? directives["default-src"];
  if (!sources) return true;
  const target = new URL(url, location.href);
  return sources.some((src) => {
    if (src === "'self'") return target.origin === location.origin;
    if (src === "*") return true;
    if (src === "'none'") return false;
    if (src.startsWith("'")) return false; // 'unsafe-inline' etc: not a source
    try {
      return target.origin === new URL(src).origin;
    } catch {
      return target.host === src || target.host.endsWith(src.replace(/^\*\./, "."));
    }
  });
}

async function boot() {
  // A Mersey-native browser executes `text/mersey` itself and announces it
  // (`globalThis.merseyNative`, set by the fork's script runner before any
  // script runs). The polyfill then disappears entirely — no WASM fetch, no
  // engine, no double execution — which is this loader's Stage B contract.
  if (globalThis.merseyNative) {
    return;
  }
  const engine = await startEngine({ engineUrl, realm: globalThis });
  // Execution backend: "js" (default) transpiles Mersey to JavaScript and the
  // browser's own JIT runs it — the fast polyfill. "wasm" interprets inside
  // the WASM engine — the conformance vehicle, kept for testing.
  const backend = (selfTag && selfTag.dataset.backend) || "js";
  const runJs = async (source, name) => {
    const js = engine.transpile(source, name);
    const url = URL.createObjectURL(new Blob([js], { type: "text/javascript" }));
    try {
      await import(url);
      return 0;
    } catch (e) {
      // A page whose CSP forbids blob: scripts cannot run transpiled modules —
      // fall back to the WASM engine, which executes without a script element
      // and so inside the page's policy. Slower, still correct.
      if (e instanceof TypeError) {
        return engine.run(source);
      }
      throw e;
    } finally {
      URL.revokeObjectURL(url);
    }
  };
  // A module graph as real ES modules: every module becomes a blob URL, and
  // the "mersey-mod:<spec>" placeholder specifiers are rewritten to the blob
  // URL of the module they name. Modules arrive dependency-first, so each
  // rewrite only needs URLs already minted; the entry imports last.
  const runJsGraph = async (spec, source, fetcher) => {
    const { modules } = await engine.transpileGraph(spec, source, fetcher);
    const urls = {};
    const rtJs = engine.runtimeJs() + "\nexport { $rt };";
    urls["%rt%"] = URL.createObjectURL(new Blob([rtJs], { type: "text/javascript" }));
    for (const m of modules) {
      const js = m.js.replace(/"mersey-mod:([^"]+)"/g, (_, dep) => {
        const u = urls[dep];
        if (!u) throw new Error(`unresolved module \`${dep}\``);
        return JSON.stringify(u);
      });
      urls[m.spec] = URL.createObjectURL(new Blob([js], { type: "text/javascript" }));
    }
    // Dynamic import() resolves through this map at run time.
    globalThis.__merseyModUrls = urls;
    await import(urls[spec]);
    return 0;
  };
  for (const tag of document.querySelectorAll('script[type="text/mersey"]')) {
    // Capabilities are granted by the page, per script, and denied otherwise
    // (§5.3): <script type="text/mersey" src="app.mersey" data-allow="random">
    const allow = (tag.getAttribute("data-allow") || "")
      .split(/[\s,]+/)
      .filter(Boolean);
    globalThis.__merseyAllow = new Set(allow);

    const spec = tag.getAttribute("src");
    try {
      let status;
      if (spec) {
        if (!cspAllows(spec)) {
          throw new Error(`refused by Content-Security-Policy: ${spec}`);
        }
        const integrity = tag.getAttribute("integrity");
        const source = await fetchWithIntegrity(spec, integrity, "module");
        // Imported modules are governed by the same policy.
        if (backend === "js") {
          const fetcher = async (url) => {
            if (!cspAllows(url)) {
              throw new Error(`refused by Content-Security-Policy: ${url}`);
            }
            return fetchWithIntegrity(url, null, "import");
          };
          status = /from\s+"\.\.?\/|import\("\.\.?\//.test(source)
            ? await runJsGraph(spec, source, fetcher)
            : await runJs(source, spec);
        } else {
          status = await engine.runGraph(spec, source, async (url) => {
            if (!cspAllows(url)) {
              throw new Error(`refused by Content-Security-Policy: ${url}`);
            }
            return fetchWithIntegrity(url, null, "import");
          });
        }
      } else {
        status = backend === "js" ? await runJs(tag.textContent, "<inline>") : engine.run(tag.textContent);
      }
      if (status !== 0) {
        console.error(`[mersey] ${spec || "<inline>"}: exited with status ${status}`);
      }
    } catch (e) {
      console.error(`[mersey] ${e.message}`);
    }
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}
