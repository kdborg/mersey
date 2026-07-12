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

#[test]
fn cycles_are_reclaimed() {
    let src = source::decode("<gc>", CYCLES.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    assert!(parsed.diagnostics.is_empty());
    assert!(bind::bind(&parsed.module).diagnostics.is_empty());
    assert!(check::check(&parsed.module).diagnostics.is_empty());
    let module: &'static _ = Box::leak(Box::new(parsed.module));

    let mut i = new_interp(Box::new(Silent));
    if let Err(t) = i.run_module(module) {
        panic!("runtime: {}", i.describe_thrown(&t));
    }

    // Without a collector these 5,000 cyclic instances — plus the closures
    // and scopes in the cycle — would all still be live.
    let before = i.collect_garbage();
    assert!(
        before.collected >= 5000,
        "expected the cycles to be collected, got {}",
        before.collected
    );
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
    assert!(
        parsed.diagnostics.is_empty(),
        "{:?}",
        parsed.diagnostics.first().map(|d| d.to_string())
    );
    assert!(bind::bind(&parsed.module).diagnostics.is_empty());
    let checked = check::check(&parsed.module);
    assert!(
        checked.diagnostics.is_empty(),
        "{:?}",
        checked.diagnostics.first().map(|d| d.to_string())
    );
    let module: &'static _ = Box::leak(Box::new(parsed.module));

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
