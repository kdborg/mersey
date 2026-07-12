//! Tier 1 correctness: JIT-compiled kernels must match the bytecode VM and
//! the AST tree-walker exactly (three-way differential).

use std::cell::RefCell;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host};

struct BufHost(Rc<RefCell<String>>);

impl Host for BufHost {
    fn print(&mut self, s: &str) {
        let mut b = self.0.borrow_mut();
        b.push_str(s);
        b.push('\n');
    }
    fn dom_set_text(&mut self, _id: &str, _t: &str) {}
    fn dom_get_text(&mut self, _id: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}
}

fn run(src_text: &str, use_vm: bool, jit: bool) -> String {
    let src = source::decode("<test>", src_text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    assert!(
        parsed.diagnostics.is_empty(),
        "parse: {:?}",
        parsed.diagnostics[0].message
    );
    assert!(
        bind::bind(&parsed.module).diagnostics.is_empty(),
        "bind errors"
    );
    assert!(
        check::check(&parsed.module).diagnostics.is_empty(),
        "check errors"
    );
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
    }
    i.run_module(module)
        .unwrap_or_else(|t| panic!("runtime: {}", i.describe_thrown(&t)));
    let out = buf.borrow().clone();
    out
}

const KERNELS: &str = r#"
import { console } from "std:console";

function mix(n: int32, seed: int32): int32 {
    let h = seed;
    let i = 0;
    while (i < n) {
        h = (h ^ (h << 13)) + i;
        h = h ^ (h >> 7);
        h = h * 31 + 1;
        i += 1;
    }
    return h;
}

function collatzSteps(start: int32): int32 {
    let n = start;
    let steps = 0;
    while (n != 1 && steps < 10000) {
        if ((n & 1) == 0) {
            n = n >> 1;
        } else {
            n = n * 3 + 1;
        }
        steps += 1;
    }
    return steps;
}

function gauss(n: int32): int32 {
    let acc = 0;
    for (let i = 1; i <= n; i++) { acc += i; }
    return acc;
}

let a = 0;
let b = 0;
let c = 0;
for (let round = 0; round < 200; round++) {
    a = a ^ mix(50, round);
    b = b + collatzSteps(round + 2);
    c = c ^ gauss(round);
}
console.log(a, b, c);
console.log(mix(1000, 42), collatzSteps(27), gauss(1000));
"#;

/// Float kernels and trapping division (the Tier-1 subset beyond int math).
const FLOAT_AND_DIV: &str = r#"
import { console } from "std:console";

function mandelIters(cx: float64, cy: float64): float64 {
    let x = 0.0;
    let y = 0.0;
    let i = 0.0;
    while (i < 50.0) {
        const xx = x * x - y * y + cx;
        const yy = 2.0 * x * y + cy;
        x = xx;
        y = yy;
        if (x * x + y * y > 4.0) { return i; }
        i = i + 1.0;
    }
    return i;
}

function harmonic(n: float64): float64 {
    let sum = 0.0;
    let k = 1.0;
    while (k <= n) {
        sum = sum + 1.0 / k;   // float division: IEEE, never traps
        k = k + 1.0;
    }
    return sum;
}

function safeDiv(a: int32, b: int32): int32 {
    return a / b;              // integer division: TRAPS on 0 and INT_MIN/-1
}

let acc = 0.0;
for (let i = 0; i < 300; i++) {
    acc = acc + mandelIters(0.001 * (i as float64), 0.002 * (i as float64));
}
console.log(acc);
console.log(harmonic(100.0));
console.log(mandelIters(-0.5, 0.5), harmonic(3.0));

// Division: hot enough to be JIT-compiled, then made to trap.
let total = 0;
for (let i = 1; i < 300; i++) { total = total + safeDiv(1000, i); }
console.log(total);
try {
    safeDiv(1, 0);
} catch (e: RangeError) {
    console.log("trapped:", e.message);
}
"#;

#[test]
fn float_kernels_and_trapping_division() {
    let jit_out = run(FLOAT_AND_DIV, true, true);
    let vm_out = run(FLOAT_AND_DIV, true, false);
    let tree_out = run(FLOAT_AND_DIV, false, false);
    assert_eq!(jit_out, vm_out, "JIT vs VM divergence on floats/division");
    assert_eq!(vm_out, tree_out, "VM vs tree divergence");
    // The trap must surface as a real Mersey error, not a wrong answer.
    assert!(
        jit_out.contains("trapped: division by zero"),
        "output was:\n{jit_out}"
    );
}

#[test]
fn float_kernels_actually_compile() {
    let src = source::decode("<test>", FLOAT_AND_DIV.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let mut compiled = Vec::new();
    for item in &module.items {
        let mersey_front::ast::Item::Decl(mersey_front::ast::Decl::Function(f)) = item else {
            continue;
        };
        let chunk = mersey_interp::vm::compile_fn_public(&f.body).expect("bytecode");
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| match &p.target {
                mersey_front::ast::Pattern::Name(n) => n.text.clone(),
                _ => panic!("simple params only"),
            })
            .collect();
        if mersey_jit::hook(&chunk, &params).is_some() {
            compiled.push(f.name.text.clone());
        }
    }
    assert!(
        compiled.contains(&"mandelIters".to_string()),
        "float kernel not compiled: {compiled:?}"
    );
    assert!(
        compiled.contains(&"harmonic".to_string()),
        "float division kernel not compiled: {compiled:?}"
    );
    assert!(
        compiled.contains(&"safeDiv".to_string()),
        "integer division kernel not compiled: {compiled:?}"
    );
}

#[test]
fn jit_matches_vm_and_tree() {
    let jit_out = run(KERNELS, true, true);
    let vm_out = run(KERNELS, true, false);
    let tree_out = run(KERNELS, false, false);
    assert_eq!(jit_out, vm_out, "JIT vs VM divergence");
    assert_eq!(vm_out, tree_out, "VM vs tree divergence");
    assert!(!jit_out.is_empty());
}

#[test]
fn jit_actually_compiles_the_kernels() {
    // Compile the kernels directly through the hook to prove they are in
    // the accepted subset (not silently falling back to the interpreter).
    let src = source::decode("<test>", KERNELS.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let mut compiled = 0;
    for item in &module.items {
        let mersey_front::ast::Item::Decl(mersey_front::ast::Decl::Function(f)) = item else {
            continue;
        };
        let chunk = mersey_interp::vm::compile_fn_public(&f.body).expect("bytecode");
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| match &p.target {
                mersey_front::ast::Pattern::Name(n) => n.text.clone(),
                _ => panic!("simple params only"),
            })
            .collect();
        let jitted = mersey_jit::hook(&chunk, &params);
        assert!(
            jitted.is_some(),
            "kernel `{}` fell out of the JIT subset",
            f.name.text
        );
        compiled += 1;
    }
    assert_eq!(compiled, 3);
}

/// §5.2 asks for guard pages and CFI-compatible codegen. A JIT turns
/// attacker-influenced input into executable memory, so this is the claim most
/// worth checking rather than assuming — and the differential tests above
/// already prove the hardened code still computes the right answers.
#[test]
fn jit_codegen_is_hardened() {
    let hardening = mersey_jit::hardening();
    assert!(!hardening.is_empty(), "no ISA: cannot report hardening");

    for (name, on) in &hardening {
        assert!(on, "hardening is off: {name}");
    }

    let names: Vec<&str> = hardening.iter().map(|(n, _)| *n).collect();
    assert!(names.iter().any(|n| n.contains("W^X")));
    assert!(names.iter().any(|n| n.contains("guard pages")));
    if cfg!(target_arch = "aarch64") {
        // Pointer authentication (backward edge) and BTI (forward edge).
        assert!(names.iter().any(|n| n.contains("backward-edge CFI")));
        assert!(names.iter().any(|n| n.contains("forward-edge CFI")));
    }
}
