// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Plain-JS counterpart of mersey/frameworkui2.mersey — the frameworkui keyed
// reconciler committing each render through a batch. In JS there is no engine
// boundary to cross, so Batch.apply() runs the op stream in-process; the point
// of the twin is that the Mersey side collapses the same ops into one host
// crossing. Same operations, same data model, same checksum as frameworkui.
export const name = "frameworkui2";
export const N = 10;
const ROWS = 200;
const MID = 100;

const OP_CREATE = 0;
const OP_SET_TEXT = 1;
const OP_APPEND = 2;
const OP_INSERT = 3;
const OP_REMOVE = 4;

// A batch of DOM mutations. A node operand is a temp id (>= 0, an index into the
// nodes this batch creates) or a live node (-(i+1), an index into `liveNodes`).
class Batch {
  constructor(doc) {
    this.ops = [];
    this.liveNodes = [];
    this.strs = [];
    this.tempCount = 0;
    this.created = [];
    this.docRef = this.live(doc);
  }
  live(node) {
    this.liveNodes.push(node);
    return -this.liveNodes.length;
  }
  str(s) {
    this.strs.push(s);
    return this.strs.length - 1;
  }
  create(tag) {
    const t = this.tempCount;
    this.tempCount += 1;
    this.ops.push(OP_CREATE, this.str(tag), t, this.docRef);
    return t;
  }
  setText(ref, text) {
    this.ops.push(OP_SET_TEXT, ref, this.str(text), 0);
  }
  append(parent, child) {
    this.ops.push(OP_APPEND, parent, child, 0);
  }
  remove(parent, child) {
    this.ops.push(OP_REMOVE, parent, child, 0);
  }
  apply() {
    const ops = this.ops;
    const strs = this.strs;
    const created = this.created;
    const resolve = (ref) => (ref >= 0 ? created[ref] : this.liveNodes[-ref - 1]);
    for (let i = 0; i < ops.length; i += 4) {
      const op = ops[i], a = ops[i + 1], b = ops[i + 2], c = ops[i + 3];
      switch (op) {
        case OP_CREATE: created[b] = resolve(c).createElement(strs[a]); break;
        case OP_SET_TEXT: resolve(a).textContent = strs[b]; break;
        case OP_APPEND: resolve(a).appendChild(resolve(b)); break;
        case OP_INSERT: resolve(a).insertBefore(resolve(b), c === -2147483648 ? null : resolve(c)); break;
        case OP_REMOVE: resolve(a).removeChild(resolve(b)); break;
      }
    }
  }
  node(ref) {
    return this.created[ref];
  }
}

function makeRow(id) {
  return { id, v: 0 };
}

// Keyed reconciler that batches: never reads the DOM mid-render, re-appends
// every row in order (appendChild moves an existing child), rewrites text only
// on a version bump. Line-for-line twin of the Mersey ListView.
class ListView {
  constructor(container) {
    this.container = container;
    this.nodes = new Map();
  }
  render(rows) {
    const nodes = this.nodes;
    const batch = new Batch(document);
    const cref = batch.live(this.container);
    const keep = new Set();
    for (let i = 0; i < rows.length; i++) keep.add(rows[i].id);
    const drop = [];
    for (const [id, entry] of nodes.entries()) {
      if (!keep.has(id)) drop.push(id);
    }
    for (let i = 0; i < drop.length; i++) {
      const entry = nodes.get(drop[i]);
      if (entry != null) {
        batch.remove(cref, batch.live(entry.node));
        nodes.delete(drop[i]);
      }
    }
    const newRows = [];
    const newRefs = [];
    for (let i = 0; i < rows.length; i++) {
      const r = rows[i];
      const existing = nodes.get(r.id);
      let ref;
      if (existing == null) {
        ref = batch.create("div");
        batch.setText(ref, `row ${r.id} v${r.v}`);
        newRows.push(r);
        newRefs.push(ref);
      } else {
        ref = batch.live(existing.node);
        if (existing.v !== r.v) {
          batch.setText(ref, `row ${r.id} v${r.v}`);
          existing.v = r.v;
        }
      }
      batch.append(cref, ref);
    }
    batch.apply();
    for (let i = 0; i < newRows.length; i++) {
      const r = newRows[i];
      nodes.set(r.id, { node: batch.node(newRefs[i]), v: r.v });
    }
  }
}

function fold(rows, checksum) {
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
    rows = [];
    for (let i = 0; i < ROWS; i++) {
      idc++;
      rows.push(makeRow(idc));
    }
    view.render(rows);
    checksum = fold(rows, checksum);
    for (let i = 0; i < rows.length; i += 10) rows[i].v++;
    view.render(rows);
    checksum = fold(rows, checksum);
    const t = rows[1];
    rows[1] = rows[ROWS - 2];
    rows[ROWS - 2] = t;
    view.render(rows);
    checksum = fold(rows, checksum);
    const next = [];
    for (let i = 0; i < rows.length; i++) {
      if (i !== MID) next.push(rows[i]);
    }
    rows = next;
    view.render(rows);
    checksum = fold(rows, checksum);
    rows = [];
    view.render(rows);
    checksum = fold(rows, checksum);
  }
  return checksum;
}
