// CLI twin of bench/web/mersey/compute.mersey — self-contained, prints RESULT.
// Compute-bound hot integer kernel, no allocation, no host calls. int32
// semantics via |0 / Math.imul, matching the engine's int32 arithmetic.
// Runs identically under node, bun and deno (performance.now is a global in all
// three). Checksum is bit-identical to the engine's.
function mix(n, seed) {
  let h = seed | 0;
  for (let i = 0; i < n; i++) {
    h = ((h ^ (h << 13)) + i) | 0;
    h = (h ^ (h >> 7)) | 0;
    h = (Math.imul(h, 31) + 1) | 0;
  }
  return h | 0;
}
function work(rounds) {
  let acc = 0;
  for (let r = 0; r < rounds; r++) acc = (acc ^ mix(20000, r)) | 0;
  return acc | 0;
}

work(200); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(2000);
const t1 = performance.now();
console.log(`RESULT compute ${t1 - t0} ${c}`);
