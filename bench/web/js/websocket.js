// Plain-JS counterpart of mersey/websocket.mersey — identical workload.
// One connection, N sequential echo roundtrips via the message event; the
// connection handshake is inside work(), so both twins time it.
export const name = "websocket";
export const N = 200;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    const ws = new WebSocket(`ws://${location.host}/bench/ws`);
    ws.addEventListener("message", (ev) => {
      const data = ev.data;
      sum = sum + data.length;
      i += 1;
      if (i >= n) {
        resolve(sum);
        ws.close();
        return;
      }
      ws.send(`msg-${i}`);
    });
    ws.addEventListener("open", () => {
      ws.send(`msg-${i}`);
    });
  });
}
