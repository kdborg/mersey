// CLI twin of bench/web/mersey/mathk.mersey — self-contained, prints RESULT.
// Math-intrinsic kernel: sqrt / max / abs in a 5M-iteration loop. Checksum is
// a boolean (c > 0), matching the .mersey self-check.
function dist(n) {
  let acc = 0.0, i = 0;
  while (i < n) {
    const x = i % 100;
    acc = acc + Math.sqrt(x * x + 2.0) + Math.max(acc * 0.0, Math.abs(0.0 - x));
    i += 1;
  }
  return acc;
}

dist(1000); // warm up
const t0 = performance.now();
const c = dist(5000000);
const t1 = performance.now();
console.log(`RESULT mathk ${t1 - t0} ${c > 0.0}`);
