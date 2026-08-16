// Plain-JS counterpart of `bench/cli/mersey/floatfmt.mersey` — identical
// workload. JavaScript is the reference here in a way it is not for the other
// twins: `Number::toString` (ECMA-262 §6.1.6.1.20) is what the engine is
// *defined* to reproduce (spec §3.6), so a checksum mismatch on this row is the
// engine being wrong, not the two disagreeing.
const N = 20000;

function corpus() {
  const xs = [];
  xs.push(1e20);
  xs.push(1e21);
  xs.push(1.25e22);
  xs.push(1e-6);
  xs.push(1e-7);
  xs.push(1.5e-7);
  xs.push(5e-324);
  xs.push(1e300);
  xs.push(0.1);
  xs.push(1.0 / 3.0);
  xs.push(1234.5);
  xs.push(1.0);
  xs.push(123456789.0);
  xs.push(0.0);
  xs.push(-0.0);
  xs.push(-1.5);
  xs.push(-1e21);
  xs.push(1.0 / 0.0);
  xs.push(-1.0 / 0.0);
  xs.push(0.0 / 0.0);
  return xs;
}

function work(n) {
  const xs = corpus();
  let sum = 0;
  for (let r = 0; r < n; r++) {
    for (let k = 0; k < xs.length; k++) {
      const s = xs[k].toString();
      sum = (sum + s.length) | 0;
      for (let i = 0; i < s.length; i++) {
        const c = s.codePointAt(i);
        sum = ((Math.imul(sum, 31)) + c) | 0;
      }
    }
  }
  return sum | 0;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(N);
const t1 = performance.now();
console.log(`RESULT floatfmt ${t1 - t0} ${c}`);
