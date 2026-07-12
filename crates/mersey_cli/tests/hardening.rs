//! §5.2: hostile input must not be able to crash the engine.
//!
//! Each case here aborted the process before it was fixed — a stack overflow is
//! not an exception a program can catch, it is `SIGABRT`, and in a renderer
//! that is a crash reachable from a web page. So each test runs the engine in a
//! *subprocess*: a regression would abort the test runner itself, and an
//! aborting test is exactly what we want to observe rather than mask.

use std::process::Command;

fn run(name: &str, src: &str) -> (bool, String, Option<i32>) {
    let dir = std::env::temp_dir().join(format!("mersey-hard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.mersey"));
    std::fs::write(&file, src).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mersey"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("run mersey");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text, out.status.code())
}

/// A stack overflow aborts (134 = SIGABRT); a thrown error exits 1.
fn assert_no_abort(code: Option<i32>, out: &str) {
    assert_ne!(code, Some(134), "the engine aborted:\n{out}");
    assert!(
        !out.contains("overflowed its stack"),
        "stack overflow:\n{out}"
    );
}

#[test]
fn runaway_recursion_throws_instead_of_aborting() {
    let (ok, out, code) = run(
        "recursion",
        r#"
import { console } from "std:console";
function down(n: int32): int32 {
    if (n == 0) { return 0; }
    return 1 + down(n - 1);
}
console.log(down(1000000));
"#,
    );
    assert_no_abort(code, &out);
    assert!(!ok, "should have thrown");
    assert!(out.contains("maximum call depth exceeded"), "{out}");
    // And the error is catchable, like any other.
    let (ok, out, code) = run(
        "recursion-caught",
        r#"
import { console } from "std:console";
function down(n: int32): int32 {
    if (n == 0) { return 0; }
    return 1 + down(n - 1);
}
try {
    down(1000000);
    console.log("no throw");
} catch (e: Error) {
    console.log("caught:", e.message);
}
"#,
    );
    assert_no_abort(code, &out);
    assert!(ok, "{out}");
    assert!(out.contains("caught: maximum call depth exceeded"), "{out}");
}

/// A deep trace must not itself become the denial of service.
#[test]
fn a_runaway_trace_is_truncated() {
    let (_, out, _) = run(
        "trace",
        r#"
function down(n: int32): int32 {
    if (n == 0) { return 0; }
    return 1 + down(n - 1);
}
down(1000000);
"#,
    );
    assert!(
        out.contains("more frames"),
        "trace should be truncated: {out}"
    );
    assert!(out.len() < 4096, "trace is {} bytes", out.len());
}

/// A long chain is built with an ordinary loop — no deep recursion in the
/// program at all, so a call-depth budget does not help. Freeing it must not
/// recurse either: `Rc` drops a linked structure link by link, on the stack.
#[test]
fn dropping_a_deep_structure_does_not_recurse() {
    let (ok, out, code) = run(
        "deep-drop",
        r#"
import { console } from "std:console";

class Node { public next: Node? = null; }

function build(n: int32): void {
    const head = new Node();
    let cur: Node = head;
    for (let i = 0; i < n; i++) {
        const node = new Node();
        cur.next = node;
        cur = node;
    }
    // `head` dies with this frame: 500,000 links freed by refcounting.
}

build(500000);
console.log("dropped");
"#,
    );
    assert_no_abort(code, &out);
    assert!(ok, "{out}");
    assert!(out.contains("dropped"), "{out}");
}

/// The same graph, but *live* across a collection: the marker walks it. And
/// then garbage, in a cycle, so the sweep has to break it.
#[test]
fn collecting_a_deep_structure_does_not_recurse() {
    let (ok, out, code) = run(
        "deep-gc",
        r#"
import { console } from "std:console";
import { gc } from "std:gc";

class Node {
    public next: Node? = null;
    public self: Node? = null;   // a cycle: refcounting alone cannot free it
}

function build(n: int32): void {
    const head = new Node();
    head.self = head;
    let cur: Node = head;
    for (let i = 0; i < n; i++) {
        const node = new Node();
        node.self = node;
        cur.next = node;
        cur = node;
    }
}

build(300000);
gc.collect();   // marks a 300k-deep graph, then sweeps it
console.log("collected");
"#,
    );
    assert_no_abort(code, &out);
    assert!(ok, "{out}");
    assert!(out.contains("collected"), "{out}");
}
