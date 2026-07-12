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
    fn dom_on_click(&mut self, _: &str, _: u32) {}
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
    let _ = Rc::new(RefCell::new(0)); // keep imports honest
}
