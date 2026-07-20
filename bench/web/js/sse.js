// Plain-JS counterpart of mersey/sse.mersey — identical workload.
// One EventSource, N server-pushed events counted via the message event;
// the client closes the stream when its count is reached.
export const name = "sse";
export const N = 2000;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    const es = new EventSource(`/bench/sse?n=${n}`);
    es.addEventListener("message", (ev) => {
      const data = ev.data;
      sum = sum + data.length;
      i += 1;
      if (i >= n) {
        resolve(sum);
        es.close();
      }
    });
  });
}
