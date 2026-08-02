#!/usr/bin/env node
// What Tier 1 refused, and where it stopped.
//
// `docs/architecture/jit-coverage.md` describes this as the way the coverage
// work is driven — "over a whole library, a histogram of the last op each
// refusal printed ranks the work: fix the top entry, re-run, look again" — and
// then leaves you to rebuild it by hand every time. This is it.
//
//   node tools/jit-refusals.mjs app.mersey            # histogram
//   node tools/jit-refusals.mjs app.mersey --list     # every function, sorted
//   node tools/jit-refusals.mjs --diff a.txt b.txt    # what changed between runs
//
// Reading the output, per that file's own rules:
//
//   * The op named is the *last one analysed*, which is the one that failed —
//     ops are printed after the decision to look at them.
//   * A refusal with **no** op is a signature failure: `sig_of` declined before
//     the body was read, which is a different bug from anything in the body.
//   * `analysis accepted every op` means codegen or the entry wrapper refused,
//     not the analysis, and those want different investigations.
//
// `--list` writes a stable fingerprint per function (outcome, size, opening
// ops) so two runs can be diffed. That matters more than it sounds: compiled
// and refused *counts* can be identical while the sets differ, and a timing
// change with no set change has no mechanism and is noise. Both of those cost
// an hour to learn the hard way.
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { existsSync } from "node:fs";

const args = process.argv.slice(2);

if (args[0] === "--diff") {
  const [a, b] = [args[1], args[2]].map((f) => readFileSync(f, "utf8").trim().split("\n"));
  const setA = new Set(a);
  const setB = new Set(b);
  const only = (xs, other) => xs.filter((x) => !other.has(x));
  const gone = only(a, setB);
  const came = only(b, setA);
  if (!gone.length && !came.length) {
    console.log("identical sets — same functions, same op sequences");
    console.log("(so the generated code is the same, and a timing difference has no mechanism)");
  } else {
    for (const l of gone) console.log(`- ${l}`);
    for (const l of came) console.log(`+ ${l}`);
  }
  process.exit(0);
}

const program = args.find((a) => !a.startsWith("--"));
if (!program || !existsSync(program)) {
  console.error("usage: node tools/jit-refusals.mjs <program.mersey> [--list] [-- <mersey args…>]");
  console.error("       node tools/jit-refusals.mjs --diff <before.txt> <after.txt>");
  process.exit(2);
}
const list = args.includes("--list");
const passthrough = args.includes("--") ? args.slice(args.indexOf("--") + 1) : [];

const bin = process.env.MERSEY_BIN || "./target/release/mersey";
const child = spawn(bin, ["run", program, ...passthrough], {
  env: { ...process.env, MERSEY_JIT_TRACE: "1" },
});
let trace = "";
child.stderr.on("data", (b) => (trace += b.toString()));
child.stdout.on("data", () => {});

child.on("close", () => {
  const fns = [];
  let ops = [];
  let acceptedAll = false;
  for (const line of trace.split("\n")) {
    let m = /^jit: analyze \d+ (\S+)/.exec(line);
    if (m) {
      ops.push(m[1]);
      // A callee's analysis prints its own "accepted every op" in the middle of
      // the caller's, so the flag has to die the moment analysis resumes.
      // Without this it survives to whichever function refuses next and marks it
      // as a codegen failure it is not — which sent me looking for IR that was
      // never printed.
      acceptedAll = false;
      continue;
    }
    if (line.includes("analysis accepted every op")) {
      acceptedAll = true;
      continue;
    }
    m = /^jit: (COMPILED|refused) (.*?) \((\d+) ops\)/.exec(line);
    if (m) {
      fns.push({
        outcome: m[1],
        who: m[2],
        size: Number(m[3]),
        // The op it stopped on. Empty means `sig_of` declined before the body.
        last: ops.length ? ops[ops.length - 1] : "<signature: no body read>",
        open: ops.slice(0, 4).join(" "),
        acceptedAll,
      });
      ops = [];
      acceptedAll = false;
    }
  }

  if (list) {
    for (const f of fns
      .map((f) => `${f.outcome.padEnd(8)} ${String(f.size).padStart(4)}ops  ${f.open}`)
      .sort())
      console.log(f);
    return;
  }

  const compiled = fns.filter((f) => f.outcome === "COMPILED").length;
  const refused = fns.filter((f) => f.outcome === "refused");
  console.log(`compiled ${compiled}   refused ${refused.length}`);
  if (!refused.length) return;

  const by = new Map();
  for (const f of refused) {
    const e = by.get(f.last) || { n: 0, sizes: [], late: 0 };
    e.n += 1;
    e.sizes.push(f.size);
    if (f.acceptedAll) e.late += 1;
    by.set(f.last, e);
  }
  console.log("\nrefusals by the op they stopped on, largest first:");
  for (const [op, e] of [...by].sort((x, y) => y[1].n - x[1].n || Math.max(...y[1].sizes) - Math.max(...x[1].sizes))) {
    const sizes = e.sizes.sort((a, b) => b - a).slice(0, 5).join(", ");
    // A refusal *after* the analysis accepted everything is codegen or the
    // entry wrapper, which is a different investigation entirely.
    const late = e.late ? `  [${e.late} after analysis passed — codegen or the wrapper]` : "";
    console.log(`  ${String(e.n).padStart(3)}  ${op.padEnd(26)} sizes ${sizes}${late}`);
  }
});
