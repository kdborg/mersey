// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Run the WasmGC probes in a real browser, driverless.
//
//   node run.mjs chrome [scale]
//   node run.mjs firefox [scale]
//   node run.mjs safari [scale]
//
// No WebDriver and no CDP, on purpose — the same reason `bench/web/
// run-firefox-real.mjs` exists. Attaching a debugger makes SpiderMonkey
// baseline-compile every wasm module (5-7x), and this measurement is *all*
// wasm, so a driven browser would report fiction.
//
// `scale` multiplies the iteration counts. Release Firefox and Safari clamp
// `performance.now()` to 1ms, which quantizes every row under ~50ms into
// uselessness — at scale 1 it made three rows appear to flip sign against
// Chrome. Firefox gets the clamp turned off in its throwaway profile; Safari
// has no profile to configure, so it needs scale 4 or more. Chrome is honest at
// scale 1.
import { createServer } from "node:http";
import { readFile, writeFile, mkdtemp, rm } from "node:fs/promises";
import { extname, join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { spawn } from "node:child_process";

const ROOT = dirname(fileURLToPath(import.meta.url));
const TYPES = { ".html": "text/html", ".wasm": "application/wasm", ".js": "text/javascript" };

const BROWSERS = {
  chrome: {
    bin: process.env.CHROME_BIN ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    args: (url, prof) => ["--headless=new", "--disable-gpu", `--user-data-dir=${prof}`,
      "--no-first-run", "--no-default-browser-check", url],
  },
  firefox: {
    bin: process.env.FIREFOX_BIN ?? "/Applications/Firefox.app/Contents/MacOS/firefox",
    args: (url, prof) => ["--headless", "--profile", prof, "--new-instance", url],
    // Release Firefox rounds performance.now() to 1ms. Off for this profile only.
    prefs: 'user_pref("privacy.reduceTimerPrecision", false);\n' +
           'user_pref("privacy.resistFingerprinting", false);\n',
  },
  // Safari has no headless mode and no throwaway profile: it is driven by
  // AppleScript into a window of the user's own Safari, which means we must
  // never kill the process. We close the one window we opened and leave the
  // rest of their session alone.
  safari: { applescript: true },
};

const which = process.argv[2] ?? "chrome";
const scale = process.argv[3] ? Number(process.argv[3]) : (which === "chrome" ? 1 : 4);
const b = BROWSERS[which];
if (!b) {
  console.error(`unknown browser ${which} (chrome | firefox | safari)`);
  process.exit(1);
}

let received = null;
const server = createServer(async (req, res) => {
  if (req.method === "POST" && req.url === "/__result") {
    let body = "";
    for await (const c of req) body += c;
    try { received = JSON.parse(body); } catch (e) { received = { error: String(e) }; }
    res.writeHead(204); res.end(); return;
  }
  try {
    // Strip the query before resolving: `?scale=4` made `/` miss its own
    // special case and serve a 404 body, which showed up as a browser window
    // reading "nope" rather than as an error anywhere.
    const rel = req.url.split("?")[0];
    const p = join(ROOT, rel === "/" ? "page.html" : rel);
    const data = await readFile(p);
    res.writeHead(200, { "content-type": TYPES[extname(p)] ?? "application/octet-stream" });
    res.end(data);
  } catch { res.writeHead(404); res.end("not found"); }
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const url = `http://127.0.0.1:${server.address().port}/?scale=${scale}`;

const settle = (ms) => new Promise((r) => setTimeout(r, ms));
const waitForResult = async (deadlineMs) => {
  const end = Date.now() + deadlineMs;
  while (!received && Date.now() < end) await settle(200);
};

let prof = null;
try {
if (b.applescript) {
  const osa = (s) => spawn("osascript", ["-e", s], { stdio: "ignore" });
  osa(`tell application "Safari" to make new document with properties {URL:"${url}"}`);
  setTimeout(() => osa('tell application "Safari" to activate'), 500);
  await waitForResult(1_500_000);
  osa('tell application "Safari" to close (every window whose name contains "WasmGC probes")');
} else {
  prof = await mkdtemp(join(tmpdir(), "wgc-prof-"));
  if (b.prefs) await writeFile(join(prof, "user.js"), b.prefs);
  const child = spawn(b.bin, b.args(url, prof), { detached: true, stdio: "ignore" });
  // `spawn` reports a missing binary as an asynchronous `error` event, not a
  // throw. Unhandled, it terminates the process — skipping even a `finally`,
  // which is how a bad browser path used to leave its profile directory behind.
  let spawnErr = null;
  child.on("error", (e) => { spawnErr = e; });
  await Promise.race([
    waitForResult(1_500_000),
    (async () => { while (!spawnErr && !received) await settle(100); })(),
  ]);
  if (spawnErr) {
    console.error(`cannot launch ${which}: ${spawnErr.message}`);
    console.error(`set ${which.toUpperCase()}_BIN to its path`);
    process.exitCode = 1;
  }

  // Kill the process *group*, then sweep by profile path. The group kill alone
  // is not enough: Firefox's content processes escaped it and one instance sat
  // resident on the developer's machine for an hour after a run finished — the
  // sort of leftover that quietly contends with the next measurement instead of
  // announcing itself. The profile path is unique per run, so this cannot touch
  // a browser the user started.
  try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch {} }
  await settle(300);
  await new Promise((r) => {
    const p = spawn("pkill", ["-9", "-f", prof], { stdio: "ignore" });
    p.on("exit", r); p.on("error", r);
  });
}

} finally {
  // In a `finally` because the failure path leaks otherwise: a bad browser path
  // throws from `spawn` *after* mkdtemp, and the profile directory outlives the
  // run. Small, but this file is about not leaving things behind.
  server.close();
  if (prof) await rm(prof, { recursive: true, force: true });
}

if (!received) {
  console.log(`${which}: no result before timeout`);
  process.exit(1);
}
console.log(`${received.ua}\nscale=${scale}`);
if (!received.strOK) console.log(`!! js-string builtins unavailable: ${received.strErr}`);

const by = new Map();
for (const r of received.results) {
  if (!by.has(r.label)) by.set(r.label, {});
  by.get(r.label)[r.impl] = r;
}
console.log("\nlabel                       JS ms   WasmGC ms   verdict         checksums");
for (const [label, v] of by) {
  if (!v.js || !v.wasmgc) continue;
  const ratio = v.js.ms / v.wasmgc.ms;
  const ok = v.js.check === v.wasmgc.check ? "match" : `MISMATCH ${v.js.check} vs ${v.wasmgc.check}`;
  const verdict = ratio >= 1 ? `${ratio.toFixed(2)}x faster` : `${(1 / ratio).toFixed(2)}x SLOWER`;
  console.log(`${label.padEnd(24)} ${String(v.js.ms).padStart(8)} ${String(v.wasmgc.ms).padStart(11)}   ${verdict.padEnd(15)} ${ok}`);
}
for (const r of received.results.filter((r) => r.label === "string build only")) {
  console.log(`${r.label.padEnd(24)} ${"".padStart(8)} ${String(r.ms).padStart(11)}   (construction alone)`);
}
