// Plain-JS counterpart of mersey/compression.mersey — identical workload.
// A gzip compress + decompress roundtrip per iteration; the checksum is on
// the roundtripped text, since compressed bytes differ across engines.
export const name = "compression";
export const N = 200;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    function step() {
      if (i >= n) {
        resolve(sum);
        return;
      }
      const data = `payload-${i} zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz`;
      const cs = new CompressionStream("gzip");
      const ds = new DecompressionStream("gzip");
      const src = new Response(data).body;
      const out = src.pipeThrough({ readable: cs.readable, writable: cs.writable }).pipeThrough({ readable: ds.readable, writable: ds.writable });
      new Response(out).text().then((t) => {
        sum = sum + t.length;
        i += 1;
        step();
      });
    }
    step();
  });
}
