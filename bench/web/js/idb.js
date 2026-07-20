// Plain-JS counterpart of mersey/idb.mersey — identical workload.
// A put + get roundtrip in a fresh readwrite transaction per iteration,
// chained through the request success events.
export const name = "idb";
export const N = 500;
export function work(n) {
  let sum = 0;
  let i = 0;
  return new Promise((resolve) => {
    function step(db) {
      if (i >= n) {
        resolve(sum);
        db.close();
        return;
      }
      const tx = db.transaction("kv", "readwrite");
      const store = tx.objectStore("kv");
      store.put(`value-${i}`, "k");
      const g = store.get("k");
      g.addEventListener("success", () => {
        sum = sum + g.result.length;
        i += 1;
        step(db);
      });
    }
    const open = indexedDB.open("mersey-bench", 1);
    open.addEventListener("upgradeneeded", () => {
      const db = open.result;
      db.createObjectStore("kv");
    });
    open.addEventListener("success", () => {
      const db = open.result;
      step(db);
    });
  });
}
