// Plain-JS counterpart of mersey/storage.mersey — identical workload.
export const name = "storage";
export const N = 20000;
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    localStorage.setItem("mersey.bench", `value-${i}`);
    const got = localStorage.getItem("mersey.bench");
    if (got != null) sum += got.length;
  }
  return sum;
}
