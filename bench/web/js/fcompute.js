// Plain-JS counterpart of mersey/fcompute.mersey. Float64 kernel: a leapfrog
// orbit integrator. The checksum is a boolean (acc > 0) — the same weak
// self-check the .mersey uses, because float bit parity across two independent
// codegens is not guaranteed.
export const name = "fcompute";
export const N = 400;
function orbit(steps, dt) {
  let x = 1.0, y = 0.0, vx = 0.0, vy = 1.0;
  let i = 0;
  while (i < steps) {
    const r2 = x * x + y * y;
    const inv = 1.0 / (r2 * r2 * 0.5 + 0.5);
    vx = vx - x * inv * dt;
    vy = vy - y * inv * dt;
    x = x + vx * dt;
    y = y + vy * dt;
    i += 1;
  }
  return x * x + y * y;
}
export function work(rounds) {
  let acc = 0.0;
  let r = 0;
  while (r < rounds) { acc = acc + orbit(20000, 0.001); r += 1; }
  return acc > 0.0;
}
