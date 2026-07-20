// Plain-JS counterpart of mersey/locks.mersey — identical workload.
// N sequential exclusive acquisitions of one lock, chained through the
// request promise; the granted callback reads the lock's name.
export const name = "locks";
export const N = 500;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    function step() {
      if (i >= n) {
        resolve(sum);
        return;
      }
      navigator.locks.request("mersey-lock", (l) => {
        sum = sum + l.name.length;
      }).then((v) => {
        i += 1;
        step();
      });
    }
    step();
  });
}
