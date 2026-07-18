// Plain-JS counterpart of mersey/fetch.mersey — identical workload.
// Sequential fetch chain via nested .then (not await): Mersey's promises work
// via .then today, so the twins stay line-for-line. work() returns a Promise
// of the checksum; the harness awaits it.
export const name = "fetch";
export const N = 100;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    function step() {
      if (i >= n) {
        resolve(sum);
        return;
      }
      fetch(`/bench/echo?i=${i}`).then((resp) => {
        sum += resp.status;
        resp.text().then((body) => {
          sum += body.length;
          i += 1;
          step();
        });
      });
    }
    step();
  });
}
