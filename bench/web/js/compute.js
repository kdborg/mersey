// Plain-JS counterpart of mersey/compute.mersey. int32 semantics via |0.
export const name = "compute";
export const N = 2000;
function mix(n, seed) {
  let h = seed | 0;
  for (let i = 0; i < n; i++) {
    h = ((h ^ (h << 13)) + i) | 0;
    h = (h ^ (h >> 7)) | 0;
    h = (Math.imul(h, 31) + 1) | 0;
  }
  return h | 0;
}
export function work(rounds) {
  let acc = 0;
  for (let r = 0; r < rounds; r++) acc = (acc ^ mix(20000, r)) | 0;
  return acc | 0;
}
