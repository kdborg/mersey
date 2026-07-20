// Plain-JS counterpart of mersey/urlpattern.mersey — identical workload.
// One compiled URLPattern; a test() + exec() per iteration with the matched
// pathname input read back.
export const name = "urlpattern";
export const N = 5000;
const p = new URLPattern("/users/:id/*", "https://example.com");
export function work(n) {
  let sum = 0;
  let i = 0;
  while (i < n) {
    const u = `https://example.com/users/${i}/posts`;
    if (p.test(u)) {
      sum += 1;
    }
    const r = p.exec(u);
    sum = sum + r.pathname.input.length;
    i += 1;
  }
  return sum;
}
