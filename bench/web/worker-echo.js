// Echo worker for the `worker` workload — shared infrastructure (like the
// /bench/ws echo endpoint), not a benchmark twin. Lives outside js/ so the
// runners' workload discovery does not pick it up.
self.onmessage = (ev) => {
  self.postMessage(ev.data);
};
