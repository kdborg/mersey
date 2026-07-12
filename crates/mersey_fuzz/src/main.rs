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
    fn dom_on_click(&mut self, _: &str, _: u32) {}
}

// ---- pipeline under test ---------------------------------------------------------

/// Frontend only; returns whether the program is clean.
fn frontend(bytes: &[u8]) -> bool {
    let Ok(src) = source::decode("<fuzz>", bytes) else {
        return false;
    };
    let parsed = parser::parse(&src);
    if !parsed.diagnostics.is_empty() {
        return false;
    }
    if !bind::bind(&parsed.module).diagnostics.is_empty() {
        return false;
    }
    check::check(&parsed.module).diagnostics.is_empty()
}

/// Execute on one engine; returns output + error line.
fn execute(src_text: &str, use_vm: bool) -> String {
    let src = source::decode("<fuzz>", src_text.as_bytes()).expect("checked");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let buf = Rc::new(RefCell::new(String::new()));
    let mut i = new_interp(Box::new(BufHost(buf.clone())));
    i.use_vm = use_vm;
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
            frontend(&input);
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
fn gen_program(rng: &mut Rng) -> String {
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
        let r = catch_unwind(AssertUnwindSafe(|| {
            if !frontend(prog.as_bytes()) {
                return None; // generator produced something the checker rejects
            }
            Some((execute(&prog, true), execute(&prog, false)))
        }));
        match r {
            Err(_) => {
                findings += 1;
                let path = format!("fuzz-crash-diff-{i}.mersey");
                let _ = std::fs::write(&path, &prog);
                eprintln!("PANIC; program saved to {path}");
            }
            Ok(Some((vm, tree))) if vm != tree => {
                findings += 1;
                let path = format!("fuzz-diverge-{i}.mersey");
                let _ = std::fs::write(&path, &prog);
                eprintln!("ENGINE DIVERGENCE; program saved to {path}");
            }
            _ => {}
        }
    }
    findings
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("all");
    let iters: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0xC0FFEE);
    let mut rng = Rng(seed | 1);

    let mut findings = 0;
    if mode == "mutate" || mode == "all" {
        println!("mutation fuzzing: {iters} iterations…");
        findings += run_mutation(iters, &mut rng);
    }
    if mode == "diff" || mode == "all" {
        println!("differential fuzzing: {iters} iterations…");
        findings += run_differential(iters, &mut rng);
    }
    if findings > 0 {
        eprintln!("{findings} finding(s)");
        std::process::exit(1);
    }
    println!("no findings");
}
