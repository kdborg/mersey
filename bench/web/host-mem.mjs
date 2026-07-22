// Process-tree memory sampling, per host platform.
//
// The benchmark legs report one memory number per workload: the memory used by
// the whole browser process tree. Getting that right needs a metric that counts
// a SHARED page once, not once per process — a browser maps its huge core
// library (libxul, liblagom-web) into every content process, so naively summing
// RSS overcounts wildly.
//
// Linux and macOS each have such a metric, but they are NOT the same metric and
// their numbers must never be compared across platforms:
//
//   linux  pss        /proc/PID/smaps_rollup "Pss:" summed over the tree.
//                     Proportional set size divides each shared page by the
//                     number of processes mapping it.
//   macos  footprint  `footprint -f bytes` over the whole pid set at once.
//                     Given multiple pids, footprint DE-DUPLICATES multiply
//                     mapped objects and prints "Summary Footprint:" — the
//                     tree total with shared objects counted once. This is
//                     phys_footprint accounting (dirty + compressed memory the
//                     kernel charges to the process), which is a different
//                     model from PSS even though it serves the same purpose.
//
// Every row records which metric produced it (see MEM_METRIC), and rows are
// keyed by platform, so a macOS number never silently lands in a Linux slot.
import { readdir, readFile } from "node:fs/promises";
import { readdirSync, readFileSync, readlinkSync } from "node:fs";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

/** Canonical platform tag stored in every result row. */
export const PLATFORM =
  process.platform === "linux" ? "linux"
  : process.platform === "darwin" ? "macos"
  : process.platform;

/** Which memory metric this platform's numbers are expressed in. */
export const MEM_METRIC =
  PLATFORM === "linux" ? "pss"
  : PLATFORM === "macos" ? "footprint"
  : null;

/** True when this host can measure tree memory at all. */
export const MEM_SUPPORTED = MEM_METRIC !== null;

// --------------------------------------------------------------------------
// Linux: /proc
// --------------------------------------------------------------------------

async function linuxPidsMatchingCmdline(match) {
  const pids = (await readdir("/proc")).filter((n) => /^\d+$/.test(n)).map(Number);
  const hits = [];
  for (const pid of pids) {
    try {
      const cmd = await readFile(`/proc/${pid}/cmdline`, "utf8");
      if (cmd.includes(match)) hits.push(pid);
    } catch {}
  }
  return hits;
}

function linuxPssKiB(pids) {
  let total = 0;
  for (const pid of pids) {
    try {
      const rollup = readFileSync(`/proc/${pid}/smaps_rollup`, "utf8");
      const m = /^Pss:\s+(\d+) kB/m.exec(rollup);
      if (m) total += Number(m[1]);
    } catch { /* process may have exited mid-scan */ }
  }
  return total;
}

function linuxPidsMatchingExeSync(prefix) {
  let pids;
  try { pids = readdirSync("/proc"); } catch { return []; }
  const hits = [];
  for (const pid of pids) {
    if (!/^\d+$/.test(pid)) continue;
    let exe;
    try { exe = readlinkSync(`/proc/${pid}/exe`); } catch { continue; }
    if (exe.startsWith(prefix)) hits.push(Number(pid));
  }
  return hits;
}

// --------------------------------------------------------------------------
// macOS: ps for discovery, footprint for the de-duplicated total
// --------------------------------------------------------------------------

// `ps` is cheap (~20ms) and gives the full argv, so it serves both the
// cmdline-substring and executable-prefix match shapes.
async function macPidsMatching(pred) {
  let out;
  try {
    ({ stdout: out } = await execFileAsync("ps", ["-Axo", "pid=,command="], { maxBuffer: 8 << 20 }));
  } catch { return []; }
  return parsePs(out, pred);
}

function parsePs(out, pred) {
  const hits = [];
  for (const line of out.split("\n")) {
    const m = /^\s*(\d+)\s+(.*)$/.exec(line);
    if (!m) continue;
    if (pred(m[2])) hits.push(Number(m[1]));
  }
  return hits;
}

// Ask footprint for the whole pid set in ONE call so it can de-duplicate shared
// objects. With several pids it prints "Summary Footprint: N B"; with a single
// pid there is no summary line, so fall back to that process's own footprint.
async function macFootprintKiB(pids) {
  if (!pids.length) return 0;
  const args = ["-f", "bytes", "--noCategories"];
  for (const p of pids) args.push("-p", String(p));
  let out;
  try {
    ({ stdout: out } = await execFileAsync("footprint", args, { maxBuffer: 8 << 20 }));
  } catch (e) {
    // footprint exits non-zero if every target died mid-sample; it still prints
    // what it managed to collect, so use that rather than losing the sample.
    out = e?.stdout || "";
  }
  const summary = /^Summary Footprint:\s+(\d+)\s*B/m.exec(out);
  if (summary) return Math.round(Number(summary[1]) / 1024);
  let total = 0;
  for (const m of out.matchAll(/^\s*phys_footprint:\s+(\d+)\s*B/gm)) total += Number(m[1]);
  return Math.round(total / 1024);
}

// --------------------------------------------------------------------------
// Public API — both return KiB, 0 when nothing matched or unsupported.
// --------------------------------------------------------------------------

/** Tree memory for every process whose command line contains `match`. */
export async function treeMemoryByCmdline(match) {
  if (PLATFORM === "linux") return linuxPssKiB(await linuxPidsMatchingCmdline(match));
  if (PLATFORM === "macos") return macFootprintKiB(await macPidsMatching((cmd) => cmd.includes(match)));
  return 0;
}

/** Tree memory for every process whose executable lives under `prefix`. */
export async function treeMemoryByExePrefix(prefix) {
  if (PLATFORM === "linux") return linuxPssKiB(linuxPidsMatchingExeSync(prefix));
  if (PLATFORM === "macos") return macFootprintKiB(await macPidsMatching((cmd) => cmd.startsWith(prefix)));
  return 0;
}

/**
 * Tree memory for `rootPid` and every process descended from it, plus the pid
 * set itself (callers use it to kill exactly that tree).
 *
 * Descendants — not a name match — because a stock-browser leg must not pick up
 * the user's own browser running alongside it.
 */
export async function treeMemoryByDescendantsOf(rootPid) {
  const ppid = await parentMap();
  const tree = new Set([rootPid]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const [pid, parent] of ppid) {
      if (tree.has(parent) && !tree.has(pid)) { tree.add(pid); grew = true; }
    }
  }
  const pids = [...tree];
  const kib = PLATFORM === "linux" ? linuxPssKiB(pids)
            : PLATFORM === "macos" ? await macFootprintKiB(pids)
            : 0;
  return { kib, tree };
}

async function parentMap() {
  const ppid = new Map();
  if (PLATFORM === "linux") {
    let pids = [];
    try { pids = (await readdir("/proc")).filter((n) => /^\d+$/.test(n)).map(Number); } catch { return ppid; }
    await Promise.all(pids.map(async (pid) => {
      try {
        const st = await readFile(`/proc/${pid}/status`, "utf8");
        const m = /^PPid:\s+(\d+)/m.exec(st);
        if (m) ppid.set(pid, Number(m[1]));
      } catch {}
    }));
    return ppid;
  }
  if (PLATFORM === "macos") {
    try {
      const { stdout } = await execFileAsync("ps", ["-Axo", "pid=,ppid="], { maxBuffer: 8 << 20 });
      for (const line of stdout.split("\n")) {
        const m = /^\s*(\d+)\s+(\d+)\s*$/.exec(line);
        if (m) ppid.set(Number(m[1]), Number(m[2]));
      }
    } catch {}
  }
  return ppid;
}

/**
 * Peak memory of THIS process (the engine child), in KiB.
 *
 * Linux reads VmHWM (peak RSS). macOS reports phys_footprint_peak, which the
 * kernel tracks for free — deliberately not process.resourceUsage().maxRSS,
 * whose unit differs between the platforms (bytes on macOS, KiB on Linux).
 */
export async function selfPeakMemoryKiB() {
  if (PLATFORM === "linux") {
    try {
      const st = await readFile("/proc/self/status", "utf8");
      return Number(/^VmHWM:\s+(\d+) kB/m.exec(st)?.[1] ?? 0);
    } catch { return 0; }
  }
  if (PLATFORM === "macos") {
    try {
      const { stdout } = await execFileAsync(
        "footprint", ["-f", "bytes", "--noCategories", "-p", String(process.pid)], { maxBuffer: 8 << 20 });
      const m = /^\s*phys_footprint_peak:\s+(\d+)\s*B/m.exec(stdout);
      return m ? Math.round(Number(m[1]) / 1024) : 0;
    } catch { return 0; }
  }
  return 0;
}

/**
 * Peak sampler for browsers that spawn short-lived helper processes (Ladybird's
 * test-web), where there is no persistent tree to settle and sample.
 *
 * Resolution differs by platform, and that is inherent, not an oversight:
 *   linux  /proc reads are cheap and synchronous, so it polls at `intervalMs`
 *          (single-digit ms) and keeps the max of the summed PSS.
 *   macos  each footprint call costs ~100ms, so it polls an order of magnitude
 *          slower and can miss a spike between samples. The number is a
 *          best-effort peak, floored by whatever samples landed.
 * Both are already best-effort against sub-second process lifetimes; neither is
 * comparable to the other, nor to a settle-then-sample figure.
 */
export function createPeakSampler(exePrefix, { intervalMs } = {}) {
  const period = intervalMs ?? (PLATFORM === "linux" ? 5 : 120);
  let peak = 0;
  let timer = null;
  let inFlight = false;

  const sampleLinux = () => {
    const kib = linuxPssKiB(linuxPidsMatchingExeSync(exePrefix));
    if (kib > peak) peak = kib;
  };

  const sampleMac = async () => {
    if (inFlight) return;            // never overlap footprint calls
    inFlight = true;
    try {
      const kib = await treeMemoryByExePrefix(exePrefix);
      if (kib > peak) peak = kib;
    } finally { inFlight = false; }
  };

  return {
    start() {
      if (timer) return;
      const tick = PLATFORM === "linux" ? sampleLinux : sampleMac;
      timer = setInterval(tick, period);
      if (timer.unref) timer.unref();
    },
    stop() { if (timer) { clearInterval(timer); timer = null; } },
    get peakKiB() { return peak; },
    reset() { peak = 0; },
  };
}
