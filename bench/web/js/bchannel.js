// Plain-JS counterpart of mersey/bchannel.mersey — identical workload.
// Sender + receiver channel pair, N sequential postMessage roundtrips
// chained through the receiver's message event.
export const name = "bchannel";
export const N = 1000;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    const tx = new BroadcastChannel("mersey-bench");
    const rx = new BroadcastChannel("mersey-bench");
    rx.addEventListener("message", (ev) => {
      const data = ev.data;
      sum = sum + data.length;
      i += 1;
      if (i >= n) {
        resolve(sum);
        tx.close();
        rx.close();
        return;
      }
      tx.postMessage(`bc-${i}`);
    });
    tx.postMessage(`bc-${i}`);
  });
}
