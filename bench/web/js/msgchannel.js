// Plain-JS counterpart of mersey/msgchannel.mersey — identical workload.
// One MessageChannel, N postMessage roundtrips port1 → port2, chained
// through port2's message event (started explicitly).
export const name = "msgchannel";
export const N = 1000;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    const mc = new MessageChannel();
    mc.port2.addEventListener("message", (ev) => {
      const data = ev.data;
      sum = sum + data.length;
      i += 1;
      if (i >= n) {
        resolve(sum);
        mc.port1.close();
        return;
      }
      mc.port1.postMessage(`mc-${i}`);
    });
    mc.port2.start();
    mc.port1.postMessage(`mc-${i}`);
  });
}
