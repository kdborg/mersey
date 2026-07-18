// Plain-JS counterpart of mersey/encoding.mersey — identical workload.
export const name = "encoding";
export const N = 10000;
const enc = new TextEncoder();
const dec = new TextDecoder();
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const bytes = enc.encode(`payload ${i} — encdec`);
    sum += bytes.length;
    const str = dec.decode(bytes);
    sum += str.length;
  }
  return sum;
}
