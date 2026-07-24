use std::fs;
fn load(p: &str) -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/demo");
    fs::read_to_string(format!("{root}/{p}")).unwrap()
}
#[test]
fn probe() {
    // (a) stdclasses alone
    let out = mersey_js::transpile(&load("lib/stdclasses.mersey"), "x", false);
    println!("alone: {:?}", out.diagnostics);
    // (b) entry-first order (CLI's parse order)
    let mods = vec![
        ("demo/modular.mersey".to_string(), load("modular.mersey")),
        (
            "demo/lib/counter.mersey".to_string(),
            load("lib/counter.mersey"),
        ),
        (
            "demo/lib/stdclasses.mersey".to_string(),
            load("lib/stdclasses.mersey"),
        ),
    ];
    let out = mersey_js::transpile_graph(&mods);
    println!("entry-first: {:?}", out.diagnostics);
}
