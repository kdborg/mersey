// Plain-JS counterpart of mersey/blob.mersey — identical workload.
// A two-part Blob per iteration, size read plus a slice's size.
export const name = "blob";
export const N = 10000;
export function work(n) {
  let sum = 0;
  let i = 0;
  while (i < n) {
    const b = new Blob([`part-${i}`, "-suffix"]);
    sum = sum + b.size + b.slice(0, 4).size;
    i += 1;
  }
  return sum;
}
