// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! The cycle collector must actually reclaim cycles that refcounting cannot.

use std::cell::RefCell;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host};

struct Silent;
impl Host for Silent {
    fn print(&mut self, _s: &str) {}
    fn dom_set_text(&mut self, _: &str, _: &str) {}
    fn dom_get_text(&mut self, _: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _: &str, _: &str, _: u32) {}
}

/// Collects what the program printed.
struct Capture(Rc<RefCell<String>>);
impl Host for Capture {
    fn print(&mut self, s: &str) {
        self.0.borrow_mut().push_str(s);
        self.0.borrow_mut().push('\n');
    }
    fn dom_set_text(&mut self, _: &str, _: &str) {}
    fn dom_get_text(&mut self, _: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _: &str, _: &str, _: u32) {}
}

const CYCLES: &str = r#"
class Node2 {
    public onTick: (() => int32)? = null;
    private ticks: int32 = 0;
    public arm(): void {
        this.onTick = () => { this.ticks += 1; return this.ticks; };
    }
}
function churn(n: int32): void {
    for (let i = 0; i < n; i++) {
        const node = new Node2();
        node.arm();     // instance -> closure -> scope -> instance
    }
}
churn(5000);
"#;

/// A chain long enough to be a stack overflow if freeing it recursed.
///
/// `Rc` frees a linked structure by recursion — dropping the head drops the
/// next, which drops the next — so `GcCell::drop` moves children onto a queue
/// and the outermost drop drains it in a loop. §5.2 says hostile input must not
/// crash the engine, and a list built by an ordinary loop is not even hostile.
///
/// This covers the recursion, and nothing here covered it before. It does *not*
/// cover the other way that drop can go wrong: handing `take_children` the
/// queue directly — the obvious way to skip a per-drop allocation — panics,
/// because `take_children` can itself drop a value and re-enter a queue that is
/// still borrowed. A chain of `Link`s never does, so this test passes on that
/// broken version; `the_cycle_collector_never_considers_a_reachable_object_garbage`
/// is the one that catches it.
const DEEP: &str = r#"
class Link {
    public next: Link? = null;
    public v: int32 = 0;
    public constructor(v: int32) { this.v = v; }
}
function build(n: int32): Link {
    let head = new Link(0);
    for (let i: int32 = 1; i < n; i += 1) {
        const l = new Link(i);
        l.next = head;
        head = l;
    }
    return head;
}
function walk(h: Link): int32 {
    let n = 0;
    let cur: Link? = h;
    while (cur != null) {
        n += 1;
        cur = (cur as Link).next;
    }
    return n;
}
let total = 0;
for (let round: int32 = 0; round < 3; round += 1) {
    // Each round's chain is freed in one go when `head` goes out of scope.
    const head = build(300000);
    total = total + walk(head);
}
if (total != 900000) {
    throw new Error(`walked ${total}`);
}
"#;

#[test]
fn freeing_a_long_chain_does_not_recurse() {
    let src = source::decode("<deep>", DEEP.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty());
    assert!(bind::bind(module).diagnostics.is_empty());
    assert!(check::check(module).diagnostics.is_empty());

    let mut i = new_interp(Box::new(Silent));
    if let Err(t) = i.run_module(module) {
        panic!("runtime: {}", i.describe_thrown(&t));
    }
    // Reaching here at all is the assertion — a recursive free aborts the
    // process rather than failing a check. The heap being quiescent afterwards
    // says the chains were actually reclaimed and not merely survived.
    let after = i.collect_garbage();
    assert!(
        after.tracked <= 10,
        "{} objects left after three 300k chains",
        after.tracked
    );
}

#[test]
fn cycles_are_reclaimed() {
    let src = source::decode("<gc>", CYCLES.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    // Leaked first: the AST that is checked must be the AST that runs.
    // `check` takes `&'static` precisely so this cannot be got wrong.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty());
    assert!(bind::bind(module).diagnostics.is_empty());
    assert!(check::check(module).diagnostics.is_empty());

    let mut i = new_interp(Box::new(Silent));
    if let Err(t) = i.run_module(module) {
        panic!("runtime: {}", i.describe_thrown(&t));
    }

    // The cycles are gone. Most of them were reclaimed *while the loop was
    // still running* — the collector no longer waits for a host boundary — so
    // this final pass finds only the stragglers since the last one. What must
    // be true is that nothing cyclic survives, which the two assertions below
    // check: the heap is empty, and a second pass has nothing to do.
    let before = i.collect_garbage();
    // Breaking each cycle drops the rest of it by refcount, so the heap is
    // left essentially empty (only the live module scope survives).
    assert!(
        before.tracked <= 10,
        "heap should be quiescent after collection, {} objects left",
        before.tracked
    );
    // A second pass finds nothing new.
    let after = i.collect_garbage();
    assert_eq!(after.collected, 0, "second collection should be a no-op");
}

/// The generational collector's write barrier, under load.
///
/// A minor collection skips the old generation entirely, so it is sound only
/// if every store of a young object into an old one was recorded. The churn
/// runs inside a *callback*, because a finished callback is a host boundary —
/// which is where collection is allowed to happen — so each invocation below
/// really does trigger a minor collection over a heap whose old generation
/// (`kept`) the trace will refuse to walk.
///
/// `set_verify` cross-checks each minor collection against a full trace, so a
/// missed barrier fails here loudly instead of quietly losing live objects.
const OLD_TO_YOUNG: &str = r#"
import { console } from "std:console";
import { document } from "browser:dom";

class Node3 {
    public child: Node3? = null;
    public items: int32[] = [];
}

const kept: Node3[] = [];
for (let i = 0; i < 50; i++) { kept.push(new Node3()); }
let round = 0;
let ok = true;

const btn = document.getElementById("churn");
if (btn != null) {
btn.addEventListener("click", () => {
    for (let j = 0; j < 50; j++) {
        const scratch = new Node3();   // dies immediately: young garbage
        scratch.items.push(j);

        const old = kept[j];           // old: survived an earlier collection
        old.child = new Node3();       // young object stored into an old one
        old.items = [round, j];        // young array stored into an old one
    }
    // Read back what a previous callback stored. If a barrier had been missed,
    // these objects would have been swept while still reachable.
    for (let k = 0; k < 50; k++) {
        const n = kept[k];
        if (n.child == null) { ok = false; }
        if (n.items.length != 2 || n.items[1] != k) { ok = false; }
    }
    round += 1;
});
}

const check = document.getElementById("check");
if (check != null) {
check.addEventListener("click", () => { console.log(ok, round); });
}
"#;

#[test]
fn minor_collections_see_stores_into_old_objects() {
    mersey_interp::gc::set_verify(true);
    mersey_interp::gc::set_threshold(200); // force a collection most callbacks

    let src = source::decode("<gc-gen>", OLD_TO_YOUNG.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(
        parsed.diagnostics.is_empty(),
        "{:?}",
        parsed.diagnostics.first().map(|d| d.to_string())
    );
    assert!(bind::bind(module).diagnostics.is_empty());
    let checked = check::check(module);
    assert!(
        checked.diagnostics.is_empty(),
        "{:?}",
        checked.diagnostics.first().map(|d| d.to_string())
    );

    let out = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(Capture(out.clone())));
    if let Err(t) = i.run_module(module) {
        panic!("runtime: {}", i.describe_thrown(&t));
    }

    // Each callback allocates ~150 objects and ends at a host boundary, so
    // with a threshold of 200 most of them collect.
    for _ in 0..200 {
        if let Err(t) = i.invoke_callback(0) {
            panic!("callback: {}", i.describe_thrown(&t));
        }
    }
    if let Err(t) = i.invoke_callback(1) {
        panic!("report: {}", i.describe_thrown(&t));
    }
    assert_eq!(
        out.borrow().trim(),
        "true 200",
        "objects stored into the old generation did not survive collection"
    );
}

/// A loop that allocates cycles must not grow without bound.
///
/// This is the bug the reference-counting cycle collector exists for. Collection
/// used to happen only at host boundaries, because a tracing collector needs
/// roots and the interpreter cannot enumerate the live values in its own Rust
/// locals. A program that stayed inside one loop therefore never collected at
/// all: refcounting freed the garbage that was not cyclic, but every `for` body
/// that makes a closure builds a cycle (the scope holds the closure, the closure
/// captured the scope), and those accumulated. Two million iterations held 1.7 GB;
/// a long enough loop was killed by the OOM killer rather than finishing.
///
/// The program reports its own heap from *inside* the loop, so this measures the
/// thing that was broken rather than the state left behind afterwards.
const CYCLES_IN_A_LOOP: &str = r#"
import { console } from "std:console";
import { gc } from "std:gc";

class Node2 {
    public onTick: (() => int32)? = null;
    private ticks: int32 = 0;
    public arm(): void {
        this.onTick = () => { this.ticks += 1; return this.ticks; };
    }
}

let live = 0;
for (let i = 0; i < 300000; i++) {
    const node = new Node2();
    node.arm();                 // instance -> closure -> scope -> instance
    if (i == 299999) {
        live = gc.stats().live;
    }
}
console.log(live);
"#;

#[test]
fn a_loop_that_allocates_cycles_does_not_grow_without_bound() {
    let src = source::decode("<gc>", CYCLES_IN_A_LOOP.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty());
    assert!(bind::bind(module).diagnostics.is_empty());
    assert!(check::check(module).diagnostics.is_empty());

    let out = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(Capture(out.clone())));
    if let Err(t) = i.run_module(module) {
        panic!("runtime: {}", i.describe_thrown(&t));
    }
    let live: usize = out.borrow().trim().parse().expect("a heap size");

    // 300,000 iterations each build a cycle of three objects. Uncollected, the
    // heap at the last iteration would be most of a million objects; collected
    // as it goes, it stays near the threshold the collector triggers at.
    assert!(
        live < 100_000,
        "the heap grew with the loop: {live} objects live at the last iteration"
    );
}

/// The cycle collector's soundness, checked against the tracing collector over
/// every conformance program.
///
/// The two decide liveness by completely different means: the tracer walks from
/// a root set, the cycle collector subtracts internal references from strong
/// counts. If they agree on every heap the test suite can build, the internal
/// edge accounting is right — and it has to be exactly right, because
/// overcounting one edge drives an object's external count to zero and sweeps it
/// while it is still in use.
#[test]
fn the_cycle_collector_never_considers_a_reachable_object_garbage() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/runtime");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("conformance programs") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "mersey") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let src = source::decode("<gc>", text.as_bytes()).expect("decode");
        let parsed = parser::parse(&src);
        let module: &'static _ = Box::leak(Box::new(parsed.module));
        if !parsed.diagnostics.is_empty() {
            continue; // a program that is meant not to compile
        }

        let mut i = new_interp(Box::new(Silent));
        if i.run_module(module).is_err() {
            continue; // a program that is meant to throw
        }
        i.verify_cycles()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        checked += 1;
    }
    assert!(checked > 10, "only {checked} programs checked");
}
