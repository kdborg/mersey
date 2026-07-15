// Plain-JS counterpart of mersey/crypto.mersey — identical workload.
export const name = "crypto";
export const N = 20000;
const buf = new Uint8Array(16);
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    crypto.getRandomValues(buf);
    sum += buf.length;
  }
  return sum;
}
