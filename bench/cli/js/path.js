// CLI twin of `bench/cli/mersey/path.mersey`. node, bun and deno all ship a
// `path` module, and it is written in JavaScript rather than native code, so
// this compares two engines running comparable POSIX path work — the same
// arrangement as the `url` twin. `posix` explicitly, so the result does not
// depend on which platform the benchmark runs on.
import path from "node:path";

const { normalize, dirname, basename, extname, join, relative, isAbsolute } = path.posix;

function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const p = `/usr/local/lib/mersey/${i}/../pkg/mod.mersey`;
    sum = (sum + normalize(p).length) % 1000003;
    sum = (sum + dirname(p).length) % 1000003;
    sum = (sum + basename(p).length) % 1000003;
    sum = (sum + extname(p).length) % 1000003;
    const parts = ["a", `b${i}`, "..", "c/d", "e.txt"];
    sum = (sum + join(...parts).length) % 1000003;
    sum = (sum + relative("/usr/local", p).length) % 1000003;
    if (isAbsolute(p)) {
      sum = (sum + 1) % 1000003;
    }
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(20000);
const t1 = performance.now();
console.log(`RESULT path ${t1 - t0} ${c}`);
