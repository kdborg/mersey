// CLI twin of bench/web/mersey/encoding.mersey — TextEncoder / TextDecoder are
// globals in node, bun and deno. Checksum bit-identical to the engine's
// std:bytes UTF-8 (437780).
const enc = new TextEncoder();
const dec = new TextDecoder();

function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const bytes = enc.encode(`payload ${i} — encdec`);
    sum += bytes.length;
    const str = dec.decode(bytes);
    sum += str.length;
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(10000);
const t1 = performance.now();
console.log(`RESULT encoding ${t1 - t0} ${c}`);
