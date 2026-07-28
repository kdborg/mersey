// Plain-JS counterpart of mersey/mathk.mersey. Math-intrinsic kernel: sqrt /
// max / abs in a 5M-iteration loop. The checksum is a boolean (c > 0), matching
// the .mersey self-check — see fcompute.js on why floats do not checksum.
export const name = "mathk";
export const N = 5000000;
export function work(n) {
  let acc = 0.0;
  let i = 0;
  while (i < n) {
    const x = i % 100;
    acc = acc + Math.sqrt(x * x + 2.0) + Math.max(acc * 0.0, Math.abs(0.0 - x));
    i += 1;
  }
  return acc > 0.0;
}
