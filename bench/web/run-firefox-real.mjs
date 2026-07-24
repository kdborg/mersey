// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Web-platform benchmark harness — REAL Firefox (no driver, no debugger).
//
// The Playwright legs measured by run.mjs / run-tjs.mjs drive Firefox with
// the JS debugger attached, and SpiderMonkey disables its optimizing wasm
// compiler (Ion) while debugging — every wasm-based leg (poly, and tjs's
// wasm compute tier) runs baseline-only, 5-7× slower than real Firefox
// (microsoft/playwright#11102, closed as inherent; reproducible in stock
// Firefox by opening DevTools). This runner measures the same three stock
// implementations in the SYSTEM Firefox, launched headless with nothing
// attached:
//   - js   : the workload in plain JavaScript
//   - tjs  : Mersey transpiled to JS at load time (+ wasm compute tier)
//   - poly : Mersey interpreted by the engine compiled to WASM
//
// Playwright cannot drive a stock Firefox build, so there is no driver at
// all: one fresh profile + process per sample. The served pages are
// instrumented (via server.mjs's transformHtml hook — the checked-in pages
// are untouched) to POST their RESULT console line back to the harness, and
// each sample starts on a harness-served blank page that navigates itself to
// the workload page when the harness says go — so the memory delta is
// measured on ONE process tree, blank page vs workload page, mirroring the
// Playwright runners. Timing is self-reported by the workload, so process
// startup is excluded. PSS is summed over the spawned tree only (found by
// walking /proc PPids), so a concurrently running user Firefox is never
// counted.
//
// Env: WL=name,…  IMPL=js,tjs,poly  FIREFOX_BIN=…  TIMEOUT_MS=…
// Writes results.firefox-real.json (browser: "firefox-real").
import { readFile, writeFile, readdir, stat, mkdtemp, rm } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import os from "node:os";
import { startServer } from "./server.mjs";
import { treeMemoryByDescendantsOf } from "./host-mem.mjs";
import { tagRows, mergeRows } from "./rows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const PAGE = "bench/web/pages";

const FIREFOX =
  process.env.FIREFOX_BIN ||
  (process.platform === "darwin"
    ? "/Applications/Firefox.app/Contents/MacOS/firefox"
    : "/usr/bin/firefox");
const REPEATS = 3;
const TIMEOUT_MS = Number(process.env.TIMEOUT_MS || 120000);

const WORKLOADS = process.env.WL
  ? process.env.WL.split(",")
  : (await readdir(join(here, "mersey")))
      .filter((f) => f.endsWith(".mersey"))
      .map((f) => f.replace(/\.mersey$/, ""))
      .sort();
const IMPLS = process.env.IMPL ? process.env.IMPL.split(",") : ["js", "tjs", "poly"];

// Snap-confined Firefox cannot read /tmp or dot-directories in $HOME; its
// common dir is the one throwaway-profile location every install can see.
const snapCommon = join(os.homedir(), "snap", "firefox", "common");
const profileBase = await stat(snapCommon).then(() => snapCommon).catch(() => os.tmpdir());

// Quiet first-run/update machinery so a fresh profile goes straight to work.
const USER_JS = `
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);
user_pref("app.update.disabledForTesting", true);
user_pref("toolkit.telemetry.reporting.policy.firstRun", false);
user_pref("dom.max_script_run_time", 0);
`;

// Console hook injected into every served workload page: forward RESULT
// lines (and page errors, for diagnosis) to the harness. Installed before
// any page script runs, so it also catches the loader's console.
const HOOK = `<script>
(() => {
  const post = (kind, text) =>
    fetch("/__bench", { method: "POST", body: JSON.stringify({ kind, text }) }).catch(() => {});
  const orig = console.log.bind(console);
  console.log = (...a) => {
    const line = a.map(String).join(" ");
    if (line.startsWith("RESULT ")) post("result", line);
    orig(...a);
  };
  addEventListener("error", (e) => post("error", String(e.message)));
  addEventListener("unhandledrejection", (e) => post("error", String(e.reason)));
})();
</script>`;

// The launch page: reports readiness (the blank-page memory baseline is
// taken here), then polls until the harness flips `go` and navigates to the
// workload page — one process tree, blank vs workload, like a driver would.
const BLANK = `<!doctype html><meta charset="utf-8"><title>bench blank</title><body><script>
const next = new URLSearchParams(location.search).get("next");
fetch("/__bench", { method: "POST", body: JSON.stringify({ kind: "ready" }) });
const poll = async () => {
  try {
    const r = await (await fetch("/__go")).json();
    if (r.go) { location.href = next; return; }
  } catch {}
  setTimeout(poll, 100);
};
poll();
</script>`;

// Memory (KiB) of the process tree rooted at `rootPid`, plus the pid set the
// caller kills. Descendants are walked rather than name-matched so a
// concurrently running user Firefox is never counted (see the file header).
async function treePss(rootPid) {
  const { kib, tree } = await treeMemoryByDescendantsOf(rootPid);
  return { pss: kib, tree };
}

async function killTree(ff, tree) {
  try {
    process.kill(-ff.pid, "SIGKILL"); // whole process group (detached spawn)
  } catch {}
  for (const pid of tree ?? []) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {}
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Shared channel between the HTTP handler and the sampling loop.
let current = { result: null, errors: [], ready: false };
let go = false;

async function waitFor(cond, ff, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (!cond() && Date.now() < deadline) {
    if (ff.exitCode != null) return false; // crashed before reporting
    await sleep(50);
  }
  return cond();
}

// One sample: fresh profile + process → blank baseline → navigate → RESULT.
async function sample(impl, wl, origin) {
  current = { result: null, errors: [], ready: false };
  go = false;
  const next = `/${PAGE}/${impl}.html?wl=${wl}`;
  const profile = await mkdtemp(join(profileBase, "mersey-ffreal-"));
  await writeFile(join(profile, "user.js"), USER_JS);
  const ff = spawn(
    FIREFOX,
    ["--headless", "--new-instance", "--profile", profile,
     `${origin}/__blank.html?next=${encodeURIComponent(next)}`],
    { stdio: "ignore", detached: true },
  );
  let out = { ms: null, checksum: null, rss: null, heap: null, errors: [] };
  try {
    if (!(await waitFor(() => current.ready, ff, 30000))) return out;
    await sleep(1000); // let startup allocation settle before the baseline
    const before = await treePss(ff.pid);
    go = true;
    if (!(await waitFor(() => current.result != null, ff, TIMEOUT_MS))) {
      out.errors = current.errors;
      return out;
    }
    await sleep(300);
    const after = await treePss(ff.pid);
    out = { ...current.result, rss: after.pss - before.pss, errors: current.errors };
  } finally {
    const { tree } = await treePss(ff.pid);
    await killTree(ff, tree);
    await rm(profile, { recursive: true, force: true });
  }
  return out;
}

const { server, port } = await startServer(0, {
  transformHtml: (html) => html.replace("<body>", "<body>" + HOOK),
  handle: async (req, res, url) => {
    if (url.pathname === "/__blank.html") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(BLANK);
      return true;
    }
    if (url.pathname === "/__go") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ go }));
      return true;
    }
    if (url.pathname === "/__bench" && req.method === "POST") {
      let body = "";
      req.on("data", (d) => (body += d));
      await new Promise((r) => req.on("end", r));
      const { kind, text } = JSON.parse(body);
      if (kind === "ready") {
        current.ready = true;
      } else if (kind === "result") {
        const m = /RESULT (\S+) ([\d.]+) (-?\d+)(?: heap=(\d+))?/.exec(text);
        if (m) current.result = { ms: Number(m[2]), checksum: Number(m[3]), heap: Number(m[4] ?? 0) };
      } else {
        current.errors.push(text);
      }
      res.end("ok");
      return true;
    }
    return false;
  },
});
const origin = `http://127.0.0.1:${port}`;
console.log(`serving ${origin}, firefox: ${FIREFOX}`);
console.log(`workloads: ${WORKLOADS.join(", ")}  impls: ${IMPLS.join(", ")}\n`);

const rows = [];
for (const wl of WORKLOADS) {
  for (const impl of IMPLS) {
    if (impl === "js" && !(await stat(join(here, "js", `${wl}.js`)).catch(() => null))) {
      rows.push({ browser: "firefox-real", impl, wl, ms: null });
      continue;
    }
    const samples = [];
    for (let r = 0; r < REPEATS; r++) {
      const s = await sample(impl, wl, origin);
      for (const e of s.errors) console.error(`  [pageerror ${impl}/${wl}] ${e}`);
      if (s.ms != null) samples.push(s);
    }
    if (samples.length === 0) {
      console.log(`  firefox-real ${impl.padEnd(4)} ${wl.padEnd(8)} — no result`);
      rows.push({ browser: "firefox-real", impl, wl, ms: null });
      continue;
    }
    samples.sort((a, b) => a.ms - b.ms);
    const med = samples[Math.floor(samples.length / 2)];
    console.log(
      `  firefox-real ${impl.padEnd(4)} ${wl.padEnd(8)} ${med.ms.toFixed(2).padStart(9)} ms   rss ${String(med.rss).padStart(6)} KiB`,
    );
    rows.push({ browser: "firefox-real", impl, wl, ms: med.ms, rss: med.rss, heap: med.heap, checksum: med.checksum });
  }
}

server.close();
// A filtered run (WL=…) must not clobber rows it did not measure: merge into
// the existing file, replacing only (impl, wl) pairs this run produced.

const merged = await mergeRows(here, "results.firefox-real.json", tagRows(rows));
await writeFile(join(here, "results.firefox-real.json"), JSON.stringify(merged, null, 2));
console.log(`\nwrote ${rows.length} rows to bench/web/results.firefox-real.json`);
