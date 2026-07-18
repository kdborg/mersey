// Plain-JS counterpart of mersey/cssom.mersey — identical workload.
export const name = "cssom";
export const N = 10000;
let el = null;
export function setup() {
  el = document.createElement("div");
  document.body.appendChild(el);
}
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    el.className = `row mod${i % 7}`;
    if (el.classList.contains("row")) sum += 1;
    el.style.setProperty("margin-left", `${i % 50}px`);
    sum += el.style.getPropertyValue("margin-left").length;
  }
  return sum;
}
