// Regenerate report.html's hand-baked DATA rows from the results JSONs.
// Prints one JS object literal per workload; paste into report.html's DATA.
// Key scheme (report.html): cjs/cpoly/ctjs (Chromium), fjs/fpoly/ftjs (Firefox),
// frjs/frpoly/frtjs (real driverless Firefox — run-firefox-real.mjs),
// sjs/spoly/stjs (Servo stock), native (Gecko fork), cnative (Chromium fork),
// snative (Servo fork), lbnative (Ladybird fork); m-prefixed = memory (MB).
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const load = async (f) => {
  try { return JSON.parse(await readFile(join(here, f), "utf8")); }
  catch { return []; }
};

const rows = [
  ...(await load("results.stock.json")),
  ...(await load("results.tjs.json")),
  ...(await load("results.firefox-real.json")),
  ...(await load("results.servo.json")),
  ...(await load("results.ladybird.json")),
  ...(await load("results.native.json")),
  ...(await load("results.native.servo.json")),
  ...(await load("results.native.ladybird.json")),
  ...(await load("results.native.chromium.json")),
];

const KEY = {
  "chromium/js": "cjs", "chromium/poly": "cpoly", "chromium/tjs": "ctjs",
  "firefox/js": "fjs", "firefox/poly": "fpoly", "firefox/tjs": "ftjs",
  "firefox-real/js": "frjs", "firefox-real/poly": "frpoly", "firefox-real/tjs": "frtjs",
  "servo/js": "sjs", "servo/poly": "spoly", "servo/tjs": "stjs",
  "ladybird/js": "lbjs", "ladybird/poly": "lbpoly", "ladybird/tjs": "lbtjs",
  "firefox-fork/native": "native", "chromium-fork/native": "cnative",
  "servo-fork/native": "snative", "ladybird-fork/native": "lbnative",
};

const wls = [...new Set(rows.map((r) => r.wl))].sort();
for (const wl of wls) {
  const parts = [];
  for (const r of rows.filter((r) => r.wl === wl && r.ms != null)) {
    const k = KEY[`${r.browser}/${r.impl}`];
    if (!k) continue;
    parts.push(`${k}:${r.ms.toFixed(1)}`);
    if (r.rss != null) parts.push(`m${k}:${(r.rss / 1024).toFixed(1)}`);
  }
  console.log(`    ${wl}: { ${parts.join(", ")} },`);
}
