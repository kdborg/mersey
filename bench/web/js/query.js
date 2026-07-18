// Plain-JS counterpart of mersey/query.mersey — identical workload.
export const name = "query";
export const N = 2000;
export function setup() {
  const ul = document.createElement("ul");
  for (let j = 0; j < 200; j++) {
    const li = document.createElement("li");
    if (j % 2 == 1) {
      li.className = "odd";
    } else {
      li.className = "even";
    }
    li.textContent = `item ${j}`;
    ul.appendChild(li);
  }
  document.body.appendChild(ul);
}
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const odd = document.querySelectorAll("li.odd");
    sum += odd.length;
    const t = odd[i % 100].textContent;
    if (t != null) sum += t.length;
  }
  return sum;
}
