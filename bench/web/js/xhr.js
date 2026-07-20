// Plain-JS counterpart of mersey/xhr.mersey — identical workload.
// One XMLHttpRequest per iteration, chained through the load event.
export const name = "xhr";
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
      const xhr = new XMLHttpRequest();
      xhr.open("GET", `/bench/echo?i=${i}`);
      xhr.addEventListener("load", () => {
        sum = sum + xhr.status + xhr.responseText.length;
        i += 1;
        step();
      });
      xhr.send();
    }
    step();
  });
}
