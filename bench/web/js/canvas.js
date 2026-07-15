// Plain-JS counterpart of mersey/canvas.mersey — identical workload.
export const name = "canvas";
export const N = 20000;
let ctx = null;
export function setup() {
  const canvas = document.createElement("canvas");
  canvas.width = 200;
  canvas.height = 200;
  ctx = canvas.getContext("2d");
}
export function work(n) {
  for (let i = 0; i < n; i++) {
    ctx.fillRect(i % 100, 0, 10, 10);
  }
  return n;
}
