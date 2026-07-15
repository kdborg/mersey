// Plain-JS counterpart of mersey/json.mersey — identical workload.
export const name = "json";
export const N = 20000;
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const s = JSON.stringify({ lang: "mersey", version: i, ok: true });
    sum += s.length;
  }
  return sum;
}
