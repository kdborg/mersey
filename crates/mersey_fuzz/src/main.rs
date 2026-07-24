//! Phase 7 fuzzing harness. Two modes, both panic-catching:
//!
//! 1. **Mutation** (`mutate`): random byte/char edits of the conformance
//!    corpus fed through decode → lex → parse → bind → check. Finding:
//!    any panic. (Diagnostics are fine — that's the job.)
//! 2. **Differential** (`diff`): a grammar-aware generator emits small
//!    well-typed programs (bounded loops only); every program that passes
//!    the checker runs on BOTH engines (bytecode VM and AST tree-walker).
//!    Finding: a panic, or the engines disagreeing on output.
//!
//! Usage: mersey-fuzz [mutate|diff|all] [iterations] [seed]
//! Defaults: all, 2000, 0xC0FFEE (deterministic; CI-friendly).

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host};

// ---- tiny deterministic RNG ---------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

// ---- silent host ----------------------------------------------------------------

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

// ---- pipeline under test ---------------------------------------------------------

/// Parse, bind and typecheck. `Some(module)` if the program is clean.
///
/// The module is leaked, and it is the *checked* one — which is what the two
/// engines then run. Checking one AST and running a different one was a real bug
/// here: the checker's conversions belong to the nodes it checked, and a program
/// that runs a copy of itself gets the conversions of whatever used to live at
/// those addresses. `check` now takes `&'static`, so it cannot be done again.
fn frontend(bytes: &[u8]) -> Option<&'static mersey_front::ast::Module> {
    let src = source::decode("<fuzz>", bytes).ok()?;
    let parsed = parser::parse(&src);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    if !bind::bind(module).diagnostics.is_empty() {
        return None;
    }
    if !check::check(module).diagnostics.is_empty() {
        return None;
    }
    Some(module)
}

/// Execute on one engine; returns output + error line.
/// Which engine ran it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// The AST tree-walker: the oracle. Slow, simple, and the thing the other two
    /// have to agree with.
    Tree,
    /// The bytecode VM.
    Vm,
    /// The VM, plus the Cranelift JIT — with the thresholds dropped to nothing, so
    /// a program that runs once still reaches Tier 1.
    ///
    /// Without that last part this tier was never actually exercised: a generated
    /// program calls its function a handful of times, the threshold is sixty-four,
    /// and the fuzzer would have gone on reporting "no findings" about a compiler
    /// it never invoked.
    Jit,
}

fn execute(module: &'static mersey_front::ast::Module, tier: Tier) -> String {
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = tier != Tier::Tree;
    if tier == Tier::Jit {
        i.jit = Some(mersey_jit::hook);
        // 1, not 0. Compiling on the very first call asks for callees that have
        // never run — they have no bytecode yet, so the group is refused, the
        // refusal is cached, and the "JIT tier" quietly interprets everything.
        // One interpreted pass first gives every callee a body.
        i.jit_threshold = 1;
        i.osr_threshold = 1;
    }
    let err = match i.run_module(module) {
        Ok(()) => String::new(),
        Err(t) => format!("error: {}", i.describe_thrown(&t)),
    };
    let mut out = buf.borrow().clone();
    out.push_str(&err);
    out
}

// ---- mode 1: mutation --------------------------------------------------------------

fn corpus() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for dir in [
        "tests/conformance/runtime",
        "tests/conformance/checker",
        "tests/conformance/parser",
    ] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "mersey") {
                    if let Ok(b) = std::fs::read(&p) {
                        out.push(b);
                    }
                }
            }
        }
    }
    assert!(!out.is_empty(), "run from the repository root");
    out
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    let edits = 1 + rng.below(8);
    for _ in 0..edits {
        if b.is_empty() {
            break;
        }
        match rng.below(4) {
            0 => {
                let i = rng.below(b.len());
                b[i] = (rng.next() & 0xFF) as u8;
            }
            1 => {
                let i = rng.below(b.len());
                b.remove(i);
            }
            2 => {
                let i = rng.below(b.len());
                b.insert(i, (rng.next() & 0xFF) as u8);
            }
            _ => {
                // splice a random slice elsewhere
                let from = rng.below(b.len());
                let len = rng.below(24).min(b.len() - from);
                let to = rng.below(b.len());
                let slice: Vec<u8> = b[from..from + len].to_vec();
                for (k, byte) in slice.into_iter().enumerate() {
                    b.insert((to + k).min(b.len()), byte);
                }
            }
        }
    }
    b
}

fn run_mutation(iters: u64, rng: &mut Rng) -> u64 {
    let corpus = corpus();
    let mut crashes = 0;
    for i in 0..iters {
        let seed = &corpus[rng.below(corpus.len())];
        let input = mutate(rng, seed);
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = frontend(&input);
        }));
        if r.is_err() {
            crashes += 1;
            let path = format!("fuzz-crash-mutate-{i}.bin");
            let _ = std::fs::write(&path, &input);
            eprintln!("PANIC in frontend; input saved to {path}");
        }
    }
    crashes
}

// ---- mode 2: grammar-aware differential ----------------------------------------------

/// Generate a small well-typed program (int32 world, bounded loops).
///
/// Half of them are about the **heap** — classes with fields, arrays, methods —
/// because that is what Tier 1 learned to compile, and a generator that only made
/// arithmetic would have gone on reporting "no findings" about code it never
/// wrote. A differential fuzzer is only worth what it generates.
fn gen_program(rng: &mut Rng) -> String {
    match rng.below(3) {
        0 => gen_object_program(rng),
        1 => gen_string_program(rng),
        _ => gen_numeric_program(rng),
    }
}

/// Strings: literals, templates with integer interpolation, `.length`, and
/// string locals reassigned across a loop — the surface Tier 1 gained this
/// session (const strings from the pool, `str_join` for templates, and the
/// string-slot marshalling that lets a string local survive an OSR). Output is
/// the accumulated code-unit counts, which every tier must agree on.
fn gen_string_program(rng: &mut Rng) -> String {
    let mut s = String::from("import { console } from \"std:console\";\n");
    let n_fns = 1 + rng.below(2);
    for f in 0..n_fns {
        s.push_str(&format!(
            "function g{f}(n: int32): int32 {{\n    let sum = 0;\n    let i = 0;\n    while (i < n) {{\n"
        ));
        let stmts = 1 + rng.below(3);
        for _ in 0..stmts {
            gen_str_stmt(rng, &mut s);
        }
        s.push_str("        i = i + 1;\n    }\n    return sum;\n}\n");
    }
    s.push_str("let acc = 0;\n");
    // Call each function twice at different bounds: the first pass warms/compiles,
    // the second re-enters compiled from the top (lever 4) — both must agree.
    let calls = 2 + rng.below(3);
    for _ in 0..calls {
        let f = rng.below(n_fns);
        s.push_str(&format!(
            "acc = (acc + g{f}({})) | 0;\nconsole.log(acc);\n",
            1 + rng.below(600)
        ));
    }
    s
}

fn gen_str_stmt(rng: &mut Rng, s: &mut String) {
    match rng.below(4) {
        // A template temporary, consumed straight by `.length`.
        0 => s.push_str(&format!(
            "        sum = sum + `p{}-${{i}}`.length;\n",
            rng.below(9)
        )),
        // A template stored in a local (a string slot through the loop/OSR).
        1 => s.push_str(&format!(
            "        const s = `a${{i * {}}}b${{i + {}}}c`;\n        sum = sum + s.length;\n",
            1 + rng.below(5),
            rng.below(9)
        )),
        // A const-string local.
        2 => s.push_str(&format!(
            "        const t = \"lit{}word\";\n        sum = sum + t.length;\n",
            rng.below(99)
        )),
        // A negative interpolation exercises the `-`/digit path of the join.
        _ => s.push_str(&format!(
            "        const u = `n${{0 - i}}m{}`;\n        sum = sum + u.length;\n",
            rng.below(9)
        )),
    }
}

/// Objects: fields read and written, arrays indexed, methods called, subclasses
/// through a base-typed variable, and `null` where a reference can be null.
/// Everything Tier 1 now compiles — and every tier must agree about all of it.
fn gen_object_program(rng: &mut Rng) -> String {
    let mut s = String::from("import { console } from \"std:console\";\n");
    // `unset` has no initializer, so every `Cell` is born holding **null** in a
    // field the type system calls a `float64` — nothing in the language requires a
    // field to be assigned. Compiled code believes the declared type, and this is
    // the one shape that makes it wrong. Leaving it out is how this fuzzer missed a
    // real divergence: every class it used to generate initialized everything.
    s.push_str(
        "class Cell {
    public a: int32 = 0;
    public b: float64 = 0.0;
    public flag: bool = false;
    public unset: float64;
    public next: Cell? = null;
    public constructor(a: int32) { this.a = a; this.b = 0.5; }
    public get(): int32 { return this.a; }
    public scale(k: float64): float64 { return this.b * k; }
    public bump(d: int32): void { this.a = this.a + d; }
    public useUnset(): float64 { return this.unset + 1.0; }
    public setUnset(v: float64): void { this.a = this.a + 1; this.unset = v; }
    public mixUnset(): void { this.a = this.a + 1; this.unset = this.unset * 2.0; }
}
",
    );
    // A subclass that does *not* override `get`, so class hierarchy analysis is
    // allowed to compile the call directly — and must be right when the receiver
    // is one of these.
    s.push_str("class Cell2 extends Cell {\n    public extra: int32 = 7;\n    public constructor(a: int32) { super(a); }\n}\n");

    // `mk` allocates and returns; `work` allocates in its loop, stores returned
    // objects over owned locals (the exact shape of a real use-after-free found
    // in Tier 1: `Dup` handing a borrowed copy to the store while the owned
    // original was released), and reads them afterwards.
    s.push_str(
        "function mk(a: int32): Cell {
    return new Cell(a);
}
function work(cs: Cell[], n: int32, k: int32): int32 {
    let acc = 0;
    let f = 0.0;
    let own = new Cell(k);
    for (let i = 0; i < n; i++) {
        own = mk(own.get() % 97);
        const churn = new Cell(i);
        acc = (acc + own.get() + churn.get()) | 0;
        const c = cs[i];
",
    );
    let stmts = 1 + rng.below(6);
    for _ in 0..stmts {
        match rng.below(9) {
            0 => s.push_str(&format!("        c.bump({});\n", rng.below(9))),
            1 => s.push_str(&format!(
                "        acc = (acc {} c.get()) | 0;\n",
                gen_binop(rng)
            )),
            2 => s.push_str(&format!("        f = f + c.scale({}.5);\n", rng.below(4))),
            3 => s.push_str("        c.flag = !c.flag;\n"),
            4 => s.push_str(&format!("        c.a = (c.a {} k) | 0;\n", gen_binop(rng))),
            5 => s.push_str(
                "        if (c.next != null) { acc = acc + 1; } else { acc = acc ^ 3; }\n",
            ),
            // Reading, writing and read-modify-writing a field that may still hold
            // null. These throw, and the throw is left to escape: a `try` inside the
            // loop would both stop the function compiling and hide the message, and
            // the message is the thing being compared.
            6 => s.push_str("        f = f + c.useUnset();\n"),
            7 => s.push_str(&format!("        c.setUnset({}.25);\n", rng.below(5))),
            _ => s.push_str("        c.mixUnset();\n"),
        }
    }
    // No casts here: `as` is outside the Tier 1 subset, and a cast in the
    // template would quietly keep every generated program interpreted — the
    // exact blind spot this generator exists to not have.
    s.push_str(
        "    }
    if (f > 3.0) {
        acc = acc ^ 5;
    }
    return (acc + own.get()) | 0;
}
",
    );

    s.push_str(&format!(
        "const cs: Cell[] = [];
for (let i = 0; i < {}; i++) {{ cs.push(i % 2 == 0 ? new Cell(i) : new Cell2(i)); }}
cs[0].next = cs[1];
let out = 0;
",
        2 + rng.below(30)
    ));
    let calls = 2 + rng.below(3);
    for _ in 0..calls {
        s.push_str(&format!(
            "try {{ out = (out {} work(cs, cs.length, {})) | 0; }} catch (e: Error) {{ console.log(\"caught:\", e.message); }}\nconsole.log(out, cs[0].a, cs[0].b, cs[0].flag, cs[0].unset);\n",
            gen_binop(rng),
            rng.below(20),
        ));
    }
    // And an index that is out of bounds, which must throw the same error on
    // every tier — the one thing compiled code has to raise for itself.
    if rng.chance(25) {
        s.push_str("try { console.log(cs[cs.length]); } catch (e: Error) { console.log(\"caught:\", e.message); }\n");
    }
    s
}

fn gen_numeric_program(rng: &mut Rng) -> String {
    let mut s = String::from("import { console } from \"std:console\";\n");
    let n_fns = 1 + rng.below(3);
    for f in 0..n_fns {
        s.push_str(&format!(
            "function f{f}(a: int32, b: int32): int32 {{\n    let x = a;\n    let y = b;\n"
        ));
        let stmts = 1 + rng.below(5);
        for _ in 0..stmts {
            gen_stmt(rng, &mut s, f);
        }
        s.push_str(&format!("    return x {} y;\n}}\n", gen_binop(rng)));
    }
    s.push_str("let acc = 0;\n");
    let calls = 1 + rng.below(4);
    for _ in 0..calls {
        let f = rng.below(n_fns);
        s.push_str(&format!(
            "acc = (acc {} f{f}({}, {})) | 0;\nconsole.log(acc);\n",
            gen_binop(rng),
            gen_expr(rng),
            gen_expr(rng),
        ));
    }
    s
}

fn gen_stmt(rng: &mut Rng, s: &mut String, f: usize) {
    match rng.below(5) {
        0 => s.push_str(&format!(
            "    x = x {} {};\n",
            gen_binop(rng),
            gen_expr(rng)
        )),
        1 => s.push_str(&format!(
            "    y = (y {} x) ^ {};\n",
            gen_binop(rng),
            rng.below(97)
        )),
        2 => s.push_str(&format!(
            "    if (x {} y) {{ x = x + 1; }} else {{ y = y - 1; }}\n",
            ["<", ">", "==", "!=", "<=", ">="][rng.below(6)]
        )),
        3 => s.push_str(&format!(
            "    for (let i = 0; i < {}; i++) {{ x = (x {} i) + {}; }}\n",
            1 + rng.below(6),
            gen_binop(rng),
            rng.below(13),
        )),
        _ => {
            if f > 0 {
                let callee = rng.below(f); // only call earlier functions: no recursion
                s.push_str(&format!("    y = f{callee}(y, {});\n", rng.below(50)));
            } else {
                s.push_str(&format!("    x = x ^ {};\n", rng.below(255)));
            }
        }
    }
}

fn gen_binop(rng: &mut Rng) -> &'static str {
    ["+", "-", "*", "&", "|", "^", "<<", ">>"][rng.below(8)]
}

fn gen_expr(rng: &mut Rng) -> String {
    if rng.chance(30) {
        format!("-{}", rng.below(1000))
    } else {
        format!("{}", rng.below(100000))
    }
}

fn run_differential(iters: u64, rng: &mut Rng) -> u64 {
    let mut findings = 0;
    for i in 0..iters {
        let prog = gen_program(rng);
        // A SIGSEGV in compiled code cannot be caught by `catch_unwind`; leave the
        // current program on disk so a hard crash names its culprit.
        let _ = std::fs::write("fuzz-current.mersey", &prog);
        let r = catch_unwind(AssertUnwindSafe(|| {
            // The generator produced something the checker rejects.
            let module = frontend(prog.as_bytes())?;
            // All three engines run the *same* checked AST — the one an embedder
            // would run. Anything else compares three programs, not three engines.
            Some((
                execute(module, Tier::Tree),
                execute(module, Tier::Vm),
                execute(module, Tier::Jit),
            ))
        }));
        match r {
            Err(_) => {
                findings += 1;
                let path = format!("fuzz-crash-diff-{i}.mersey");
                let _ = std::fs::write(&path, &prog);
                eprintln!("PANIC; program saved to {path}");
            }
            Ok(Some((tree, vm, jit))) if vm != tree || jit != tree => {
                findings += 1;
                let path = format!("fuzz-diverge-{i}.mersey");
                let _ = std::fs::write(&path, &prog);
                let who = if vm != tree { "VM" } else { "JIT" };
                eprintln!("ENGINE DIVERGENCE ({who} vs tree-walker); program saved to {path}");
            }
            _ => {}
        }
    }
    findings
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("all");
    // `run <file>`: one program on all three tiers (JIT compiled aggressively).
    // For reproducing and minimizing a fuzzer finding.
    if mode == "run" {
        let path = args.get(1).expect("run <file.mersey>");
        let prog = std::fs::read(path).expect("read");
        let module = frontend(&prog).expect("checker rejected the program");
        let tree = execute(module, Tier::Tree);
        let vm = execute(module, Tier::Vm);
        let jit = execute(module, Tier::Jit);
        println!("tree: {tree:?}");
        println!("vm:   {vm:?}");
        println!("jit:  {jit:?}");
        if jit != tree || vm != tree {
            eprintln!("DIVERGENCE");
            std::process::exit(1);
        }
        println!("agree");
        return;
    }
    let iters: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0xC0FFEE);
    let mut rng = Rng(seed | 1);

    let mut findings = 0;
    if mode == "mutate" || mode == "all" {
        println!("mutation fuzzing: {iters} iterations…");
        findings += run_mutation(iters, &mut rng);
    }
    if mode == "diff" || mode == "all" {
        // NOTE: Tier 1 leaks each compiled `JITModule` on purpose (its code pages
        // must outlive every call into them — see mersey_jit). One program
        // compiles a bounded set of functions, so a real embedding leaks a
        // bounded amount; but this harness compiles a *distinct* program every
        // iteration, so a few thousand iterations exhaust the executable-page
        // maps and the process is killed (SIGSEGV) — not a divergence, an
        // out-of-address-space. Keep the default (2000) safe; run many *seeds* at
        // a safe count for more coverage rather than one huge count.
        println!("differential fuzzing: {iters} iterations…");
        findings += run_differential(iters, &mut rng);
    }
    if findings > 0 {
        eprintln!("{findings} finding(s)");
        std::process::exit(1);
    }
    println!("no findings");
}
