// Line-for-line twin of bench/cli/mersey/reconcile.mersey: the keyed reconciler
// from bench/web's `frameworkui2` with its one host crossing stubbed out, so the
// engine-side half of a framework render can be measured without a browser.
//
// See the .mersey file for why this exists. In short: `frameworkui2` is 10.6x
// its JavaScript twin on the Chromium fork *while already batching* one crossing
// per render, so the gap is the reconciler or the op buffer — and the fork is
// the one place the engine cannot be introspected.
const N = 40;
const ROWS = 200;
const MID = 100;

const OP_CREATE = 0;
const OP_SET_TEXT = 1;
const OP_APPEND = 2;
const OP_REMOVE = 4;

class Node {
  constructor(id) {
    this.id = id;
  }
}

let nodeSeq = 0;

// The host's obligation and no more: a node per CREATE op, in temp-ref order.
function applyOps(ops, strs) {
  const out = [];
  let i = 0;
  while (i < ops.length) {
    if (ops[i] === OP_CREATE) {
      nodeSeq += 1;
      out.push(new Node(nodeSeq));
    }
    i += 4;
  }
  return out;
}

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
    this.ops.push(OP_CREATE);
    this.ops.push(this.str(tag));
    this.ops.push(t);
    this.ops.push(this.docRef);
    return t;
  }
  setText(ref, text) {
    this.ops.push(OP_SET_TEXT);
    this.ops.push(ref);
    this.ops.push(this.str(text));
    this.ops.push(0);
  }
  append(parent, child) {
    this.ops.push(OP_APPEND);
    this.ops.push(parent);
    this.ops.push(child);
    this.ops.push(0);
  }
  remove(parent, child) {
    this.ops.push(OP_REMOVE);
    this.ops.push(parent);
    this.ops.push(child);
    this.ops.push(0);
  }
  apply() {
    this.created = applyOps(this.ops, this.strs);
  }
  node(ref) {
    return this.created[ref];
  }
}

class Row {
  constructor(id) {
    this.id = id;
    this.v = 0;
  }
}

class Entry {
  constructor(node, v) {
    this.node = node;
    this.v = v;
  }
}

class ListView {
  constructor(container) {
    this.container = container;
    this.nodes = new Map();
  }

  render(rows) {
    const nodes = this.nodes;
    const batch = new Batch(new Node(0));
    const cref = batch.live(this.container);
    const keep = new Set();
    for (let i = 0; i < rows.length; i += 1) {
      keep.add(rows[i].id);
    }
    const drop = [];
    for (const [id, entry] of nodes.entries()) {
      if (!keep.has(id)) {
        drop.push(id);
      }
    }
    for (let i = 0; i < drop.length; i += 1) {
      const entry = nodes.get(drop[i]);
      if (entry != null) {
        batch.remove(cref, batch.live(entry.node));
        nodes.delete(drop[i]);
      }
    }
    const newRows = [];
    const newRefs = [];
    for (let i = 0; i < rows.length; i += 1) {
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
    for (let i = 0; i < newRows.length; i += 1) {
      const r = newRows[i];
      nodes.set(r.id, new Entry(batch.node(newRefs[i]), r.v));
    }
  }
}

function fold(rows, checksum) {
  let s = 0;
  for (let i = 0; i < rows.length; i += 1) {
    const r = rows[i];
    s = (s + (i + 1) * (r.id * 1000 + r.v)) | 0;
  }
  return (Math.imul(checksum, 31) + s) | 0;
}

function work(rounds) {
  const view = new ListView(new Node(0));
  let rows = [];
  let idc = 0;
  let checksum = 0;
  for (let round = 0; round < rounds; round += 1) {
    rows = [];
    for (let i = 0; i < ROWS; i += 1) {
      idc += 1;
      rows.push(new Row(idc));
    }
    view.render(rows);
    checksum = fold(rows, checksum);
    for (let i = 0; i < rows.length; i += 10) {
      rows[i].v += 1;
    }
    view.render(rows);
    checksum = fold(rows, checksum);
    const t = rows[1];
    rows[1] = rows[ROWS - 2];
    rows[ROWS - 2] = t;
    view.render(rows);
    checksum = fold(rows, checksum);
    const next = [];
    for (let i = 0; i < rows.length; i += 1) {
      if (i !== MID) {
        next.push(rows[i]);
      }
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

work(2); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(N);
const t1 = performance.now();
console.log(`RESULT reconcile ${t1 - t0} ${c}`);
