// Plain-JS counterpart of mersey/worker.mersey — identical workload.
// One echo worker, N sequential postMessage roundtrips via the message
// event; worker startup is inside work(), so both twins time it.
export const name = "worker";
export const N = 1000;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    const w = new Worker("/bench/web/worker-echo.js");
    w.addEventListener("message", (ev) => {
      const data = ev.data;
      sum = sum + data.length;
      i += 1;
      if (i >= n) {
        resolve(sum);
        w.terminate();
        return;
      }
      w.postMessage(`ping-${i}`);
    });
    w.postMessage(`ping-${i}`);
  });
}
