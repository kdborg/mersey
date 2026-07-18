// Plain-JS counterpart of mersey/timers.mersey — identical workload.
// Measures arming + disarming a timer (registration cost), not firing:
// a fired setTimeout(0) chain is dominated by the spec's 4ms nesting clamp,
// which would measure the clamp, not the API.
export const name = "timers";
export const N = 20000;
function noop() {}
export function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const id = setTimeout(noop, 1000);
    clearTimeout(id);
    if (id > 0) sum += 1;
  }
  return sum;
}
