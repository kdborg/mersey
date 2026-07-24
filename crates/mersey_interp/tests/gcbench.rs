// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Pause-time benchmark (run with --nocapture): does a large *retained* heap
//! make each collection more expensive?

use std::time::Instant;

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

/// `RETAINED` long-lived nodes, then a callback that allocates a little and
/// dies — the shape of an event-driven app with a big heap.
fn program(retained: usize) -> String {
    format!(
        r#"
import {{ document }} from "browser:dom";

class Node4 {{
    public child: Node4? = null;
    public items: int32[] = [];
}}

const kept: Node4[] = [];
for (let i = 0; i < {retained}; i++) {{
    const n = new Node4();
    n.items = [i, i + 1, i + 2];
    n.child = new Node4();
    kept.push(n);
}}

const btn = document.getElementById("churn");
if (btn != null) {{
    btn.addEventListener("click", () => {{
        for (let j = 0; j < 50; j++) {{
            const scratch = new Node4();
            scratch.items = [j];
        }}
        kept[0].child = new Node4(); // an old -> young store, so the
                                     // remembered set is non-empty
    }});
}}
"#
    )
}

/// Per-callback times in milliseconds.
fn run(retained: usize, major_every: usize) -> Vec<f64> {
    mersey_interp::gc::set_threshold(100);
    mersey_interp::gc::set_major_every(major_every);
    mersey_interp::gc::set_verify(false); // it would full-trace every collection

    let text = program(retained);
    let src = source::decode("<bench>", text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    // Leaked first: the AST that is checked must be the AST that runs.
    // `check` takes `&'static` precisely so this cannot be got wrong.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty());
    assert!(bind::bind(module).diagnostics.is_empty());
    assert!(check::check(module).diagnostics.is_empty());

    let mut i = new_interp(Box::new(Silent));
    i.run_module(module).ok().expect("run");

    let mut times = Vec::new();
    for _ in 0..300 {
        let t = Instant::now();
        i.invoke_callback(0).ok().expect("callback");
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times
}

fn median(times: &[f64]) -> f64 {
    let mut v = times.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn total(times: &[f64]) -> f64 {
    times.iter().sum()
}

/// A generational collector exists to stop a *retained* heap from being
/// re-traced on every collection. The typical (median) callback below does a
/// minor collection, and that is the pause a user feels; full traces still
/// happen, but only when the old generation has grown, so they amortize away.
#[test]
#[ignore = "benchmark: cargo test --release --test gcbench -- --ignored --nocapture"]
fn pause_time_does_not_track_heap_size() {
    println!("\n300 callbacks, each allocating ~50 objects and doing one old->young store:\n");
    println!(
        "{:>9}  {:>13}  {:>13}  {:>13}  {:>13}",
        "retained", "full: median", "full: total", "gen: median", "gen: total"
    );

    let mut rows = Vec::new();
    for retained in [1_000usize, 5_000, 20_000] {
        let full = run(retained, 0); // 0 = every collection is a full trace
        let gen = run(retained, 64); // the shipping policy
        println!(
            "{:>9}  {:>10.3} ms  {:>10.1} ms  {:>10.3} ms  {:>10.1} ms",
            retained,
            median(&full),
            total(&full),
            median(&gen),
            total(&gen)
        );
        rows.push((median(&full), median(&gen), total(&full), total(&gen)));
    }

    let (full_small, gen_small, _, _) = rows[0];
    let (full_big, gen_big, _, _) = rows[2];
    let full_growth = full_big / full_small;
    let gen_growth = gen_big / gen_small;
    println!("\nmedian pause, 1k -> 20k retained objects (a 20x bigger heap):");
    println!("  always-full:  {full_growth:.1}x");
    println!("  generational: {gen_growth:.1}x\n");

    // The claim under test: a routine collection costs what you *allocated*,
    // not what you are *keeping*.
    assert!(
        gen_growth < 3.0,
        "a 20x larger retained heap should not make routine collections much \
         more expensive, but the median pause grew {gen_growth:.1}x"
    );
    assert!(
        full_growth > 8.0,
        "sanity check: a full trace really should scale with the heap (grew {full_growth:.1}x)"
    );
}
