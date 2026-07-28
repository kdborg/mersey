// Plain-JS counterpart of mersey/calls.mersey. Call-overhead kernel: tiny
// functions called 20M times hot. int32 semantics via |0; `>>` is arithmetic in
// both JS and the engine, so the shifts match without a cast.
export const name = "calls";
export const N = 20000000;
function add3(a, b, c) { return (a + b + c) | 0; }
function step(h, i) { return add3((h ^ (h << 5)) | 0, i, h >> 3); }
export function work(n) {
  let h = 7;
  let i = 0;
  while (i < n) { h = step(h, i); i += 1; }
  return h | 0;
}
