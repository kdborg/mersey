// CLI twin of bench/web/mersey/crypto.mersey — crypto.getRandomValues is a
// global in node, bun and deno. The checksum is only the buffer length summed
// (the random bytes never enter it), identical to the engine's std:random (320000).
const buf = new Uint8Array(16);

function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    crypto.getRandomValues(buf);
    sum += buf.length;
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(20000);
const t1 = performance.now();
console.log(`RESULT crypto ${t1 - t0} ${c}`);
