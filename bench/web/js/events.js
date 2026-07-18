// Plain-JS counterpart of mersey/events.mersey — identical workload.
export const name = "events";
export const N = 10000;
let el = null;
let count = 0;
export function setup() {
  el = document.createElement("div");
  document.body.appendChild(el);
  el.addEventListener("bench", () => {
    count += 1;
  });
}
export function work(n) {
  count = 0;
  for (let i = 0; i < n; i++) {
    el.dispatchEvent(new Event("bench"));
  }
  return count;
}
