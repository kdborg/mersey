// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Plain-JS counterpart of mersey/frameworkui.mersey — a minimal keyed reactive
// list framework plus the js-framework-benchmark row operations (create /
// keyed-update / swap / remove / clear). Same operations, same checksum: the
// checksum folds the DATA model after each op, so it is identical on every leg
// regardless of how the DOM is reached.
export const name = "frameworkui";
export const N = 10; // rounds; each is create + update + swap + remove + clear
const ROWS = 200;
const MID = 100;

// A row of state. `v` is a version bumped on update, so the reconciler and the
// checksum both see updates without inspecting the DOM.
function makeRow(id) {
  return { id, v: 0 };
}

// A keyed reconciler: syncs a container's children to a rows array (keyed by
// id), doing minimal real-DOM work — create, text-update, move, remove.
class ListView {
  constructor(container) {
    this.container = container;
    this.nodes = new Map(); // id -> { node, v }
  }
  render(rows) {
    const container = this.container;
    const nodes = this.nodes;
    const keep = new Set();
    for (let i = 0; i < rows.length; i++) keep.add(rows[i].id);
    const drop = [];
    for (const [id, entry] of nodes.entries()) {
      if (!keep.has(id)) drop.push(id);
    }
    for (let i = 0; i < drop.length; i++) {
      const entry = nodes.get(drop[i]);
      if (entry != null) {
        container.removeChild(entry.node);
        nodes.delete(drop[i]);
      }
    }
    for (let i = 0; i < rows.length; i++) {
      const r = rows[i];
      const existing = nodes.get(r.id);
      let node;
      if (existing == null) {
        node = document.createElement("div");
        node.textContent = `row ${r.id} v${r.v}`;
        nodes.set(r.id, { node, v: r.v });
      } else {
        node = existing.node;
        if (existing.v !== r.v) {
          node.textContent = `row ${r.id} v${r.v}`;
          existing.v = r.v;
        }
      }
      const cur = i < container.children.length ? container.children[i] : null;
      if (cur !== node) container.insertBefore(node, cur);
    }
  }
}

// Fold the data model into the checksum after each op: order (i), identity
// (id), and update version (v). Deterministic, DOM-independent.
function fold(rows, checksum) {
  // Math.imul + | 0 wrap at every step, exactly as Mersey int32 arithmetic
  // does, so the checksum is identical on both — see mersey/frameworkui.mersey.
  let s = 0;
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    s = (s + Math.imul(i + 1, (Math.imul(r.id, 1000) + r.v) | 0)) | 0;
  }
  return (Math.imul(checksum, 31) + s) | 0;
}

export function work(rounds) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const view = new ListView(container);
  let rows = [];
  let idc = 0;
  let checksum = 0;
  for (let round = 0; round < rounds; round++) {
    // create ROWS rows
    rows = [];
    for (let i = 0; i < ROWS; i++) {
      idc++;
      rows.push(makeRow(idc));
    }
    view.render(rows);
    checksum = fold(rows, checksum);
    // update every 10th row's text
    for (let i = 0; i < rows.length; i += 10) rows[i].v++;
    view.render(rows);
    checksum = fold(rows, checksum);
    // swap two rows
    const t = rows[1];
    rows[1] = rows[ROWS - 2];
    rows[ROWS - 2] = t;
    view.render(rows);
    checksum = fold(rows, checksum);
    // remove one row (index MID)
    const next = [];
    for (let i = 0; i < rows.length; i++) {
      if (i !== MID) next.push(rows[i]);
    }
    rows = next;
    view.render(rows);
    checksum = fold(rows, checksum);
    // clear
    rows = [];
    view.render(rows);
    checksum = fold(rows, checksum);
  }
  return checksum;
}
