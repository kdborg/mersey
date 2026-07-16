use mersey_front::{ast::*, check, parser, source::SourceFile};
#[test]
fn compute_qualifies() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../bench/web/mersey/compute.mersey"
    ))
    .unwrap();
    let sf = SourceFile { name: "c".into(), text: src };
    let parsed = parser::parse(&sf);
    assert!(parsed.diagnostics.is_empty());
    let module: &'static Module = Box::leak(Box::new(parsed.module));
    let out = check::check(module);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    let fns: Vec<&FnDecl> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Decl(Decl::Function(f)) => Some(f),
            _ => None,
        })
        .collect();
    println!("top-level fns: {:?}", fns.iter().map(|f| &f.name.text).collect::<Vec<_>>());
    let tier = mersey_js::wasmgen::compile(&fns);
    match &tier {
        Some(t) => println!(
            "compiled: {:?} ({} bytes)",
            t.exports.iter().map(|e| &e.name).collect::<Vec<_>>(),
            t.bytes.len()
        ),
        None => println!("NO functions qualified"),
    }
    assert!(tier.is_some());
}
