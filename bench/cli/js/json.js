// CLI twin of bench/web/mersey/json.mersey — JSON is a global in node, bun and
// deno. Checksum bit-identical to the engine's std:json, which emits the same
// compact text (`{"lang":"mersey","version":0,"ok":true}`).
function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const s = JSON.stringify({ lang: "mersey", version: i, ok: true });
    sum += s.length;
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(20000);
const t1 = performance.now();
console.log(`RESULT json ${t1 - t0} ${c}`);
