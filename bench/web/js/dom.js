// Plain-JS counterpart of mersey/dom.mersey — identical workload.
export const name = "dom";
export const N = 10000;
export function work(n) {
  const body = document.body;
  for (let i = 0; i < n; i++) {
    const el = document.createElement("div");
    el.textContent = `item ${i}`;
    body.appendChild(el);
  }
  return n;
}
