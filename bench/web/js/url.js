// Plain-JS counterpart of mersey/url.mersey — identical workload.
export const name = "url";
export const N = 20000;
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const u = new URL(`https://example.com/path/${i}?q=mersey&n=${i}`);
    sum += u.pathname.length + u.search.length;
  }
  return sum;
}
