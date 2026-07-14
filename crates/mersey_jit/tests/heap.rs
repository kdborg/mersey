//! Tier 1 and the heap: fields, array elements, and direct method calls.
//!
//! Two things are checked, and both are needed. That the compiled code gives the
//! same answers as the interpreter — which is the only question that matters,
//! because a wrong answer arrives quietly. And that it *is* compiled: a test that
//! only compared answers would pass just as happily with every function rejected
//! and interpreted, which is the state this work exists to leave behind.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host, Interp};

struct BufHost(Rc<RefCell<String>>);

impl Host for BufHost {
    fn print(&mut self, s: &str) {
        let mut b = self.0.borrow_mut();
        b.push_str(s);
        b.push('\n');
    }
    fn dom_set_text(&mut self, _: &str, _: &str) {}
    fn dom_get_text(&mut self, _: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _: &str, _: &str, _: u32) {}
}

fn frontend(text: &str) -> &'static mersey_front::ast::Module {
    let src = source::decode("<test>", text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    assert!(
        parsed.diagnostics.is_empty(),
        "parse: {}",
        parsed.diagnostics[0].message
    );
    // Leaked first: the AST that is checked must be the AST that runs.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let b = bind::bind(module);
    assert!(b.diagnostics.is_empty(), "bind: {}", b.diagnostics[0].message);
    let c = check::check(module);
    assert!(c.diagnostics.is_empty(), "check: {}", c.diagnostics[0].message);
    module
}

/// `eager` compiles on the first call rather than the sixty-fifth, so a program
/// that runs once still reaches Tier 1. Without it a test can spend its whole
/// life in the interpreter and report on the JIT.
fn run(module: &'static mersey_front::ast::Module, use_vm: bool, jit: bool, eager: bool) -> String {
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
        if eager {
            i.jit_threshold = 0;
            i.osr_threshold = 1;
        }
    }
    let err = match i.run_module(module) {
        Ok(()) => String::new(),
        Err(t) => format!("error: {}", i.describe_thrown(&t)),
    };
    let mut out = buf.borrow().clone();
    out.push_str(&err);
    out
}

/// An engine with the program already run, so its classes and functions exist and
/// can be asked what Tier 1 makes of them.
fn engine(module: &'static mersey_front::ast::Module) -> Interp {
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf)));
    i.use_vm = true;
    i.jit = Some(mersey_jit::hook);
    i.run_module(module)
        .unwrap_or_else(|t| panic!("runtime: {}", i.describe_thrown(&t)));
    i
}

fn heap_conformance_source() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/runtime/jit-heap.mersey");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The three tiers must agree about the heap, exactly — including the errors.
///
/// The conformance suite already runs this file on the tree-walker and the
/// bytecode VM (`mersey_interp` cannot depend on `mersey_jit`, so it cannot run
/// the third). This is the third.
#[test]
fn all_three_tiers_agree_about_the_heap() {
    let text = heap_conformance_source();
    let module = frontend(&text);

    let tree = run(module, false, false, false);
    let vm = run(module, true, false, false);
    let jit = run(module, true, true, false);
    // …and again with Tier 1 entered on the very first call, so nothing gets to
    // stay interpreted just because a loop was short.
    let jit_eager = run(module, true, true, true);

    assert_eq!(vm, tree, "the bytecode VM and the tree-walker disagree");
    assert_eq!(jit, tree, "Tier 1 and the tree-walker disagree");
    assert_eq!(jit_eager, tree, "Tier 1 (eager) and the tree-walker disagree");

    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/conformance/runtime/jit-heap.expect");
    let expect = std::fs::read_to_string(golden).expect("golden");
    assert_eq!(jit, expect, "Tier 1 and the golden disagree");

    // The write that happens before each throw must have happened exactly once.
    // Compiled code that has touched the heap cannot be re-run to produce the
    // error — this is the line that says it was not.
    assert!(
        jit.contains("after throwing: 1 501"),
        "a side effect before a trap was applied twice:\n{jit}"
    );
}

/// And it is really compiled. Every one of these was outside the subset before —
/// a field access, an array element, a method call was enough to refuse a function
/// outright.
#[test]
fn the_heap_functions_are_actually_compiled() {
    let text = heap_conformance_source();
    let mut i = engine(frontend(&text));

    for f in [
        "simulate",   // array of objects, method calls, a nested loop
        "total",      // an array element read
        "scale",      // …and written
        "counts",     // an int32 array, read and written
        "bumpAll",    // a field of an array element, read and written
        "outOfBounds",
        "divByZero",
        "throughNull",
    ] {
        assert!(i.jit_accepts(f), "`{f}` fell out of the Tier 1 subset");
    }

    for (class, m) in [
        ("Body", "step"),        // writes four fields of four different widths
        ("Body", "energy"),      // reads two
        ("Body", "neighbourX"),  // reads a field *through* a field, and null-checks it
        ("Heavy", "energy"),     // inherited, compiled against the subclass
    ] {
        assert!(
            i.jit_accepts_method(class, m),
            "`{class}.{m}` fell out of the Tier 1 subset"
        );
    }
}

/// Allocation is in the subset now: `new`, constructors, functions that return
/// objects they made — and the membership is asserted, because a test that only
/// compared answers would pass just as happily with everything interpreted.
#[test]
fn allocating_functions_are_actually_compiled() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/jit/alloc.mersey"),
    )
    .expect("alloc.mersey");
    let mut i = engine(frontend(Box::leak(text.into_boxed_str())));

    for f in ["run", "churny", "mk", "make", "pick", "allocThenDie"] {
        assert!(i.jit_accepts(f), "`{f}` fell out of the Tier 1 subset");
    }
    for (class, m) in [("Vec2", "add"), ("Vec2", "dot"), ("Cell", "get")] {
        assert!(
            i.jit_accepts_method(class, m),
            "`{class}.{m}` fell out of the Tier 1 subset"
        );
    }
}

/// Class hierarchy analysis is what makes a method call a direct call. It is only
/// sound while nothing below the receiver's class overrides the method — so when
/// something does, the function must be **refused**, not compiled to the wrong
/// body.
///
/// This is the test that a mistake here would otherwise hide: with `Fast` present,
/// a `Shape[]` holding one runs its `area`, and a compiled direct call to
/// `Shape.area` would silently return the base's answer.
#[test]
fn an_overridden_method_is_not_compiled_to_a_direct_call() {
    const SRC: &str = r#"
import { console } from "std:console";

class Shape {
    public size: float64 = 2.0;
    public area(): float64 { return this.size * this.size; }
}

class Fast extends Shape {
    public override area(): float64 { return 0.0; }
}

function totalArea(ss: Shape[]): float64 {
    let t = 0.0;
    for (let i = 0; i < ss.length; i++) { t = t + ss[i].area(); }
    return t;
}

const ss: Shape[] = [new Shape(), new Fast(), new Shape()];
let out = 0.0;
for (let r = 0; r < 300; r++) { out = totalArea(ss); }
console.log(out);
"#;
    let module = frontend(SRC);

    let mut i = engine(module);
    assert!(
        !i.jit_accepts("totalArea"),
        "`area` is overridden by `Fast`, so `ss[i].area()` has no single body to \
         call — compiling it directly would run the wrong one"
    );

    // And the answer is right on every tier, which is what the refusal is for.
    let tree = run(module, false, false, false);
    let jit = run(module, true, true, true);
    assert_eq!(jit, tree);
    assert_eq!(jit.trim(), "8", "8 = 4 + 0 + 4, not 12");
}
