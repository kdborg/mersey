// CLI twin of bench/web/mersey/fcompute.mersey — self-contained, prints RESULT.
// Float64 kernel: a leapfrog orbit integrator. Checksum is a boolean
// (acc > 0) — the same weak self-check the .mersey uses, because float bit
// parity across two independent codegens is not guaranteed.
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

orbit(10000, 0.001); // warm up
const t0 = performance.now();
let acc = 0.0;
let r = 0;
while (r < 400) { acc = acc + orbit(20000, 0.001); r += 1; }
const t1 = performance.now();
console.log(`RESULT fcompute ${t1 - t0} ${acc > 0.0}`);
