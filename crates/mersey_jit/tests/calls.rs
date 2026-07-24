// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Tier 1 with calls, and on-stack replacement.
//!
//! The two things a call-count-only, call-free JIT could never do: compile a
//! function that calls another function, and compile a loop inside a function
//! that is only ever called once. Both are the difference between "fast on code
//! shaped like the compiler" and "fast on code".
//!
//! Every case here is checked three ways — JIT, bytecode VM, tree-walker — and
//! all three must agree exactly. A JIT that is fast and wrong is a bug that
//! looks like a feature.

use std::cell::RefCell;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host, Thrown};

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

/// Run a program; `Err` carries the thrown error's message.
fn try_run(src_text: &str, use_vm: bool, jit: bool) -> Result<String, String> {
    let src = source::decode("<test>", src_text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    // Leaked first: the AST that is checked must be the AST that runs.
    // `check` takes `&'static` precisely so this cannot be got wrong.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(
        parsed.diagnostics.is_empty(),
        "parse: {:?}",
        parsed.diagnostics.first().map(|d| d.message.clone())
    );
    assert!(bind::bind(module).diagnostics.is_empty(), "bind errors");
    let diags = check::check(module).diagnostics;
    assert!(
        diags.is_empty(),
        "check: {:?}",
        diags.first().map(|d| &d.message)
    );

    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
    }
    match i.run_module(module) {
        Ok(()) => Ok(buf.borrow().clone()),
        Err(t) => Err(describe(&mut i, &t)),
    }
}

fn describe(i: &mut mersey_interp::Interp, t: &Thrown) -> String {
    i.describe_thrown(t)
}

fn run(src: &str, use_vm: bool, jit: bool) -> String {
    try_run(src, use_vm, jit).unwrap_or_else(|e| panic!("runtime: {e}"))
}

/// The three tiers must produce the same output, character for character.
fn agree(src: &str) -> String {
    let jit = run(src, true, true);
    let vm = run(src, true, false);
    let tree = run(src, false, false);
    assert_eq!(jit, vm, "JIT and bytecode VM disagree");
    assert_eq!(vm, tree, "bytecode VM and tree-walker disagree");
    jit
}

/// Recursion, mutual recursion, and a call whose result feeds arithmetic.
/// `fib` is the case the old subset could never touch: one call, and the whole
/// function fell back to the interpreter.
const CALLS: &str = r#"
import { console } from "std:console";

function fib(n: int32): int32 {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

function isEven(n: int32): bool {
    if (n == 0) {
        return true;
    }
    return isOdd(n - 1);
}

function isOdd(n: int32): bool {
    if (n == 0) {
        return false;
    }
    return isEven(n - 1);
}

function square(x: int32): int32 {
    return x * x;
}

function hypotSq(a: int32, b: int32): int32 {
    return square(a) + square(b);
}

// Hot enough to be compiled, and deep enough that the compiled body is doing
// the recursing rather than the interpreter.
let acc = 0;
for (let i = 0; i < 200; i++) {
    acc = acc + fib(i % 20);
    acc = acc + hypotSq(i % 7, i % 5);
    if (isEven(i % 13)) {
        acc = acc + 1;
    }
}
console.log(acc);
console.log(fib(24));
console.log(hypotSq(3, 4));
console.log(isEven(10), isOdd(10));
"#;

#[test]
fn calls_recursion_and_mutual_recursion_agree_across_tiers() {
    let out = agree(CALLS);
    assert!(out.contains("46368"), "fib(24): {out}"); // fib(24)
    assert!(out.contains("25"), "hypotSq(3,4): {out}");
    assert!(out.contains("true false"), "parity: {out}");
}

/// A loop whose function is called exactly once. Its call count reaches 1 and
/// stops, so only the loop's own back edge can make it hot — and without OSR the
/// compiled code, if it were ever produced, could not be entered until the
/// function it belongs to had already finished.
const OSR: &str = r#"
import { console } from "std:console";

function work(n: int32): int32 {
    let acc = 0;
    for (let i = 0; i < n; i++) {
        acc = (acc + i * 3) ^ (i >> 2);
    }
    // Everything after the loop runs in compiled code too: OSR hands over the
    // rest of the function, not just the rest of the loop.
    return acc + 1;
}

function nested(n: int32): int32 {
    let total = 0;
    for (let i = 0; i < n; i++) {
        let j = 0;
        while (j < n) {
            total = total + (i ^ j);
            j = j + 1;
        }
    }
    return total;
}

console.log(work(50000));
console.log(nested(300));
"#;

#[test]
fn a_loop_in_a_once_called_function_is_compiled_and_resumed() {
    agree(OSR);
}

/// OSR must resume with the *interpreter's* values, not fresh ones. If the
/// locals were not transferred, the loop would restart from zero and the answer
/// would be wrong rather than slow — which is why this asserts the value and not
/// just that the tiers agree with each other.
#[test]
fn osr_resumes_with_the_live_locals() {
    let src = r#"
import { console } from "std:console";
function counted(n: int32): int32 {
    let sum = 0;
    for (let i = 0; i < n; i++) {
        sum = sum + i;
    }
    return sum;
}
console.log(counted(100000));
"#;
    // 0 + 1 + ... + 99999
    let expected = (0i64..100_000).sum::<i64>() as i32;
    assert_eq!(agree(src).trim(), expected.to_string());
}

/// Runaway recursion must still raise the language's error, not run the native
/// stack into its guard page. Compiled code counts its own depth and hands the
/// call back; the interpreter then throws with a position and a stack trace.
#[test]
fn compiled_recursion_still_throws_instead_of_crashing() {
    let src = r#"
import { console } from "std:console";
function down(n: int32): int32 {
    return down(n + 1) + 1;
}
// Hot first, so the runaway call is made by *compiled* code.
function warm(n: int32): int32 {
    if (n < 2) {
        return n;
    }
    return warm(n - 1) + warm(n - 2);
}
for (let i = 0; i < 100; i++) {
    warm(10);
}
console.log(down(0));
"#;
    let err = try_run(src, true, true).expect_err("runaway recursion must throw");
    assert!(
        err.contains("recursion") || err.contains("stack") || err.contains("RangeError"),
        "expected a stack-depth error, got: {err}"
    );
    // And the same error without the JIT — the tier must not change what a
    // program *means*, only how fast it gets there.
    let interpreted = try_run(src, true, false).expect_err("must throw interpreted too");
    assert_eq!(
        err, interpreted,
        "JIT and interpreter report it differently"
    );
}

/// Division by zero inside a *called* function must still throw. The trap is
/// raised in the callee, several native frames deep; it has to propagate out
/// through the compiled callers and reach the interpreter, which re-runs the
/// call to raise it properly.
#[test]
fn a_trap_inside_a_compiled_callee_propagates() {
    let src = r#"
import { console } from "std:console";
function divide(a: int32, b: int32): int32 {
    return a / b;
}
function outer(a: int32, b: int32): int32 {
    return divide(a, b) + 1;
}
let acc = 0;
for (let i = 1; i < 200; i++) {
    acc = acc + outer(100, i);
}
console.log(acc);
console.log(outer(1, 0));
"#;
    let err = try_run(src, true, true).expect_err("x / 0 must throw");
    assert!(
        err.contains("RangeError") || err.contains("zero"),
        "got: {err}"
    );
    assert_eq!(
        err,
        try_run(src, true, false).expect_err("must throw interpreted too"),
        "the JIT changed how division by zero is reported"
    );
}

/// Compiled code calls a callee's chunk *directly*, which is only correct while
/// the global name still refers to the function it named at compile time. The
/// language now guarantees that: a declaration is not a variable, and `f = g`
/// does not compile (E0304).
///
/// The JIT keeps its own entry check anyway — it verifies each callee binding is
/// the one it compiled against, and throws the code away otherwise. A compiler
/// that turns a program into machine code should not take the frontend's word
/// for the one fact its output depends on, and the check costs a pointer compare
/// per entry, not per call.
#[test]
fn a_called_function_cannot_be_reassigned_underneath_compiled_code() {
    let src = r#"
function one(): int32 {
    return 1;
}
function two(): int32 {
    return 2;
}
one = two;
"#;
    let parsed = parser::parse(&source::decode("<test>", src.as_bytes()).expect("decode"));
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let diags = bind::bind(module).diagnostics;
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("function declaration")),
        "reassigning a function must not compile, got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

/// A `bool`-returning function, compiled.
///
/// In an integer kernel a bool *is* an i32 — a comparison yields 0 or 1 and
/// flows through the same slots as everything else. That is fine inside the
/// kernel and wrong as soon as the value leaves it: this printed `1` where
/// every other tier printed `true`, for as long as the JIT has existed. Nothing
/// caught it because nothing had ever compiled a function that returns a bool
/// and then *looked* at the answer.
#[test]
fn a_compiled_bool_is_still_a_bool_when_it_leaves_the_kernel() {
    let src = r#"
import { console } from "std:console";
function isPos(n: int32): bool {
    return n > 0;
}
const isBig = (n: int32): bool => n > 100;
let r = false;
let s = false;
for (let i = 0; i < 200; i++) {
    r = isPos(5);
    s = isBig(5);
}
console.log(r, s, isPos(-1));
"#;
    assert_eq!(agree(src).trim(), "true false false");
}

/// A float kernel written with ordinary integer literals reaches Tier 1.
///
/// `2 * x * y` and `x * x + y * y > 4` is how anyone writes this. Before the
/// bytecode carried types, the `2` and the `4` were int32 constants sitting in a
/// float64 function, the kernel was rejected as mixed, and the whole thing
/// interpreted. The checker always knew they were float64 — that is what §3.3
/// says — and now it says so in the bytecode, so they are float constants and the
/// kernel compiles.
#[test]
fn a_float_kernel_written_with_int_literals_compiles() {
    let src = r#"
import { console } from "std:console";
function iterate(cx: float64, cy: float64, n: float64): float64 {
    let x = 0.0;
    let y = 0.0;
    let i = 0.0;
    while (i < n) {
        const xx = x * x - y * y + cx;
        y = 2 * x * y + cy;
        x = xx;
        if (x * x + y * y > 4) {
            return i;
        }
        i = i + 1;
    }
    return i;
}
let total = 0.0;
for (let p = 0.0; p < 200.0; p = p + 1.0) {
    total = total + iterate(-0.5 + p * 0.001, 0.3, 50.0);
}
console.log(total);
"#;
    // It must give the same answer on all three tiers…
    let out = agree(src);
    assert!(!out.is_empty());

    // …and it must actually be *compiled*, not merely correct. A test that only
    // checks the answer would pass just as happily with the kernel rejected.
    assert!(
        compiled_fns(src).contains(&"iterate".to_string()),
        "the float kernel fell out of the JIT subset"
    );
}

/// The declared return type of a function, as the JIT wants it.
#[allow(dead_code)]
fn ret_num_of(f: &mersey_front::ast::FnDecl) -> Option<mersey_front::check::Num> {
    use mersey_front::check::{IntKind, Num};
    let mersey_front::ast::TypeExpr::Named { name, .. } = f.ret.as_ref()? else {
        return None;
    };
    Some(match name.as_str() {
        "int32" => Num::Int(IntKind::I32),
        "int64" => Num::Int(IntKind::I64),
        "float64" => Num::F64,
        "float32" => Num::F32,
        _ => return None,
    })
}

/// The most ordinary numeric loop there is, compiled.
///
/// ```mersey
/// for (let i = 0; i < n; i++) { acc = acc + 1.0 / (1.0 + acc); }
/// ```
///
/// An `int32` counter and a `float64` accumulator. The old subset was
/// *homogeneous* — a kernel was all-int or all-float — because the engine could
/// not tell one from the other without looking at the values, and a compiler
/// cannot look at the values. So this was refused, and interpreted, at 60× V8.
///
/// Now the bytecode carries the types: `BinNum` says what its operands are,
/// `Convert` says what it produces, and every frame slot has a declared type. A
/// mixed function is just a function.
#[test]
fn a_function_mixing_int_and_float_compiles() {
    let src = r#"
import { console } from "std:console";
function work(n: int32): float64 {
    let acc = 0.0;
    for (let i = 0; i < n; i++) {
        acc = acc + 1.0 / (1.0 + acc);
    }
    return acc;
}
// An int32 counter, an int64 total, a float64 accumulator and a bool: four
// types in one function, and every conversion between them implicit (§3.3).
function widths(n: int32, scale: float64): int64 {
    let total: int64 = 0;
    let acc: float64 = 0.0;
    for (let i = 0; i < n; i++) {
        acc = acc + i * scale;   // the int32 counter widens into float64 arithmetic
        total = total + i;       // and into an int64 accumulator
    }
    if (acc > 0.0) {
        total = total + 1;
    }
    return total;
}
console.log(work(2000));
console.log(widths(2000, 0.5));
"#;
    // All three tiers agree…
    let out = agree(src);
    assert!(!out.is_empty());

    // …and the mixed functions are *compiled*, not merely correct. A test that
    // only checked the answer would pass just as happily with them interpreted,
    // which is the state this exists to leave behind.
    let compiled = compiled_fns(src);
    for want in ["work", "widths"] {
        assert!(
            compiled.contains(&want.to_string()),
            "`{want}` mixes int and float, and fell out of the JIT subset: {compiled:?}"
        );
    }
}

/// Which top-level functions Tier 1 accepts — asked of the engine, by the path the
/// engine uses. A test that assembled the compiler's input by hand would be
/// testing its own assembly.
fn compiled_fns(src_text: &str) -> Vec<String> {
    let src = source::decode("<test>", src_text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty(), "parse errors");
    assert!(bind::bind(module).diagnostics.is_empty(), "bind errors");
    assert!(check::check(module).diagnostics.is_empty(), "check errors");

    let buf = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let mut i = mersey_interp::new_interp(Box::new(BufHost(buf)));
    i.use_vm = true;
    i.jit = Some(mersey_jit::hook);
    i.run_module(module)
        .unwrap_or_else(|t| panic!("runtime: {}", i.describe_thrown(&t)));

    let mut out = Vec::new();
    for item in &module.items {
        if let mersey_front::ast::Item::Decl(mersey_front::ast::Decl::Function(f)) = item {
            if i.jit_accepts(&f.name.text) {
                out.push(f.name.text.clone());
            }
        }
    }
    out
}
