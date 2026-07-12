//! What the bytecode compiler refuses to compile falls back to the AST
//! tree-walker: correct, but the slow tier, and silently so. These tests pin
//! the constructs that must *not* fall back.

use mersey_front::ast::{Decl, Item, Module};
use mersey_front::{parser, source};

/// Compile the first function in `src`; `None` means the compiler gave up.
fn compiles(src: &str) -> bool {
    let decoded = source::decode("<t>", src.as_bytes()).expect("decode");
    let parsed = parser::parse(&decoded);
    assert!(
        parsed.diagnostics.is_empty(),
        "{:?}",
        parsed.diagnostics.first().map(|d| d.to_string())
    );
    let module: &'static Module = Box::leak(Box::new(parsed.module));
    for item in &module.items {
        if let Item::Decl(Decl::Function(f)) = item {
            return mersey_interp::vm::compile_fn_public(&f.body).is_some();
        }
    }
    panic!("no function in source");
}

/// `return`, `break` and `continue` inside a `try … finally` have to run the
/// finally on their way out. The compiler used to give up on exactly this and
/// hand the whole function to the tree-walker.
#[test]
fn abrupt_exits_through_finally_compile() {
    assert!(
        compiles(
            r#"
function f(): int32 {
    try { return 1; } finally { }
}
"#
        ),
        "`return` through a finally fell back to the AST tier"
    );

    assert!(
        compiles(
            r#"
function f(): int32 {
    for (let i = 0; i < 3; i++) {
        try {
            if (i == 1) { continue; }
            if (i == 2) { break; }
        } finally { }
    }
    return 0;
}
"#
        ),
        "`break`/`continue` through a finally fell back to the AST tier"
    );

    assert!(
        compiles(
            r#"
function f(): int32 {
    try {
        try { return 1; } finally { }
    } finally { }
}
"#
        ),
        "nested finallys fell back to the AST tier"
    );

    assert!(
        compiles(
            r#"
function f(): int32 {
    try {
        throw new Error("x");
    } catch (e: Error) {
        return 2;
    } finally { }
}
"#
        ),
        "`return` out of a catch through a finally fell back to the AST tier"
    );
}
