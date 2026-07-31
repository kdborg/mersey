// CLI-only workload: the string work a parser actually does — searching,
// splitting, and taking substrings, over a semver-shaped input. There was no
// such workload, and every other one in this arena leaves these methods
// untouched, so a large body of engine work in exactly this area was invisible
// here and unguarded against regressing.
//
// No twin under bench/web: this needs no browser API, and the point is the
// engine's own string layer.
function parse(s) {
  // Build metadata first — it may contain '-', so strip it before the
  // prerelease. This is the shape `std:semver` has, deliberately.
  let build = "";
  const plus = s.indexOf("+");
  if (plus >= 0) {
    build = s.slice(plus + 1);
    s = s.slice(0, plus);
  }
  let pre = "";
  const dash = s.indexOf("-");
  if (dash >= 0) {
    pre = s.slice(dash + 1);
    s = s.slice(0, dash);
  }
  const parts = s.split(".");
  if (parts.length !== 3) return -1;
  let acc = 0;
  for (let k = 0; k < 3; k++) {
    const p = parts[k];
    if (p.length === 0) return -1;
    for (let i = 0; i < p.length; i++) {
      const c = p.codePointAt(i);
      if (c < 48 || c > 57) return -1;
      acc = ((acc * 10) + (c - 48)) | 0;
    }
  }
  // The prerelease identifiers, checked the way a validator does.
  for (const id of pre.split(".")) {
    if (id.length === 0) continue;
    acc = (acc + id.length) | 0;
    if (id.startsWith("0") && id.length > 1) acc = (acc + 1) | 0;
  }
  if (build.endsWith("5")) acc = (acc + 7) | 0;
  if (build.lastIndexOf(".") >= 0) acc = (acc + 3) | 0;
  return acc;
}

function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    sum = (sum + parse("1.2.3-rc.1+build.5")) | 0;
    sum = (sum + parse("10.20.30")) | 0;
    sum = (sum + parse("nope")) | 0;
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(50000);
const t1 = performance.now();
console.log(`RESULT strings ${t1 - t0} ${c}`);
