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
    fn dom_on_click(&mut self, _id: &str, _cb: u32) {}
}

fn run(src_text: &str, use_vm: bool, jit: bool) -> String {
    let src = source::decode("<test>", src_text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics[0].message);
    assert!(bind::bind(&parsed.module).diagnostics.is_empty(), "bind errors");
    assert!(check::check(&parsed.module).diagnostics.is_empty(), "check errors");
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
    }
    i.run_module(module).unwrap_or_else(|t| panic!("runtime: {}", i.describe_thrown(&t)));
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
        assert!(jitted.is_some(), "kernel `{}` fell out of the JIT subset", f.name.text);
        compiled += 1;
    }
    assert_eq!(compiled, 3);
}
