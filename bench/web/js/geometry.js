// Plain-JS counterpart of mersey/geometry.mersey — identical workload.
// A DOMMatrix per iteration, translate + scale chained, three components
// read back; everything stays integral so the checksum is exact.
export const name = "geometry";
export const N = 10000;
export function work(n) {
  let sum = 0;
  let i = 0;
  while (i < n) {
    const m = new DOMMatrix();
    const t = m.translate(i % 10, 3.0).scale(2.0);
    sum = sum + t.m41 + t.m42 + t.a;
    i += 1;
  }
  return sum;
}
