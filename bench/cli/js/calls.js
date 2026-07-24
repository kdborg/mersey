// CLI twin of bench/web/mersey/calls.mersey — self-contained, prints RESULT.
// Call-overhead kernel: tiny functions called 20M times hot. int32 semantics
// via |0; >> is arithmetic in both JS and the engine, so shifts match.
function add3(a, b, c) { return (a + b + c) | 0; }
function step(h, i) { return add3((h ^ (h << 5)) | 0, i, h >> 3); }
function run(n) {
  let h = 7;
  let i = 0;
  while (i < n) { h = step(h, i); i += 1; }
  return h | 0;
}

run(1000); // warm up
const t0 = performance.now();
const c = run(20000000);
const t1 = performance.now();
console.log(`RESULT calls ${t1 - t0} ${c}`);
