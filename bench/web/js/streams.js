// Plain-JS counterpart of mersey/streams.mersey — identical workload.
// A pull-sourced ReadableStream consumed chunk by chunk through the
// reader's read() promise chain.
export const name = "streams";
export const N = 2000;
export function work(n) {
  let produced = 0;
  let sum = 0;
  return new Promise((resolve) => {
    const stream = new ReadableStream({
      pull: (c) => {
        if (produced < n) {
          c.enqueue(`chunk-${produced}`);
          produced += 1;
        } else {
          c.close();
        }
      },
    });
    const reader = stream.getReader();
    function pump() {
      reader.read().then((r) => {
        if (r.done) {
          resolve(sum);
          return;
        }
        sum = sum + r.value.length;
        pump();
      });
    }
    pump();
  });
}
