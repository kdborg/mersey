// CLI twin of bench/web/mersey/url.mersey — self-contained, prints RESULT.
// URL parsing: construct a URL, read its pathname + search lengths. `URL` is a
// global in node, bun and deno (all WHATWG-URL); checksum is bit-identical to
// the engine's std:url (537780).
function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const u = new URL(`https://example.com/path/${i}?q=mersey&n=${i}`);
    sum += u.pathname.length + u.search.length;
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(20000);
const t1 = performance.now();
console.log(`RESULT url ${t1 - t0} ${c}`);
