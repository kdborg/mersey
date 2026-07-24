// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Runtime conformance: golden stdout of executing each program (see
//! tests/conformance/README.md). MERSEY_BLESS=1 regenerates goldens.
//! These goldens are the behavioral contract the Phase 2 bytecode VM (and
//! later the JIT) must reproduce exactly.

use std::path::{Path, PathBuf};

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host};

use std::cell::RefCell;
use std::rc::Rc;

struct TestHost {
    out: Rc<RefCell<String>>,
    dom: std::collections::HashMap<String, String>,
}

impl TestHost {
    fn emit(&self, line: String) {
        let mut out = self.out.borrow_mut();
        out.push_str(&line);
        out.push('\n');
    }
}

impl Host for TestHost {
    fn print(&mut self, s: &str) {
        self.emit(s.to_string());
    }
    fn dom_set_text(&mut self, id: &str, text: &str) {
        self.dom.insert(id.to_string(), text.to_string());
        self.emit(format!("[dom #{id}] {text}"));
    }
    fn dom_get_text(&mut self, id: &str) -> Option<String> {
        self.dom.get(id).cloned()
    }
    fn dom_add_listener(&mut self, id: &str, event: &str, cb: u32) {
        self.emit(format!("[dom #{id}] {event} handler #{cb} registered"));
    }
}

fn run_program(bytes: &[u8], name: &str) -> String {
    run_program_with(bytes, name, true)
}

fn run_program_with(bytes: &[u8], name: &str, use_vm: bool) -> String {
    let src = match source::decode(name, bytes) {
        Ok(s) => s,
        Err(d) => return format!("{d}\n"),
    };
    let parsed = parser::parse(&src);
    // Leaked first: the AST that is checked must be the AST that runs.
    // `check` takes `&'static` precisely so this cannot be got wrong.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(module).diagnostics;
    }
    if diags.is_empty() {
        diags = check::check(module).diagnostics;
    }
    if !diags.is_empty() {
        let mut s = String::new();
        for d in &diags {
            s.push_str(&format!("{d}\n"));
        }
        return s;
    }

    let buffer = Rc::new(RefCell::new(String::new()));
    let host = Box::new(TestHost {
        out: buffer.clone(),
        dom: Default::default(),
    });
    let mut interp = new_interp(host);
    interp.use_vm = use_vm;
    let err = match interp.run_module(module) {
        Ok(()) => None,
        Err(t) => Some(format!("runtime error: {}", interp.describe_thrown(&t))),
    };
    let mut out = buffer.borrow().clone();
    if let Some(e) = err {
        out.push_str(&e);
        out.push('\n');
    }
    out
}

/// Differential guarantee: the AST tree-walker and the bytecode VM must
/// produce identical output for the whole runtime suite (ROADMAP Phase 2:
/// goldens are the contract between tiers).
#[test]
fn runtime_conformance_tree_walker() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/runtime");
    for case in std::fs::read_dir(&dir).unwrap() {
        let path = case.unwrap().path();
        if path.extension().is_none_or(|x| x != "mersey") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let vm_out = run_program_with(&bytes, &name, true);
        let tree_out = run_program_with(&bytes, &name, false);
        assert_eq!(vm_out, tree_out, "engines diverge on {name}");
        let expected = std::fs::read_to_string(path.with_extension("expect")).unwrap();
        assert_eq!(tree_out, expected, "tree-walker vs golden on {name}");
    }
}

#[test]
fn runtime_conformance() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/runtime");
    let bless = std::env::var_os("MERSEY_BLESS").is_some();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "mersey"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no runtime cases in {}", dir.display());

    let mut failures = Vec::new();
    for case in &cases {
        let bytes = std::fs::read(case).unwrap();
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let actual = run_program(&bytes, &name);
        let expect_path = case.with_extension("expect");
        if bless {
            std::fs::write(&expect_path, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expect_path)
            .unwrap_or_else(|_| String::from("<missing .expect file>"));
        if actual != expected {
            failures.push(format!(
                "== {name}\n--- expected\n{expected}\n--- actual\n{actual}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `JSON.stringify({literal})` is fused into a template by the bytecode compiler
/// (skipping the record allocation and interpreted serializer). Its output must
/// be byte-identical to what `JSON.stringify` produces — same key/string escaping
/// (`webjson`) and same integer formatting. These run on the VM/JIT fused path
/// (no web bridge needed, since the platform serializer is never called) and are
/// asserted against hand-written JSON, which is the contract. Escaping edge cases
/// (`"`, `\`, control chars, non-ASCII) and every bakeable scalar kind plus a
/// dynamic int are covered.
#[test]
fn json_stringify_fusion_matches_spec() {
    let cases: &[(&str, &str)] = &[
        // The web benchmark's shape: constant string, dynamic int, constant bool.
        (
            r#"const i = 7; console.log(JSON.stringify({ lang: "mersey", version: i, ok: true }));"#,
            r#"{"lang":"mersey","version":7,"ok":true}"#,
        ),
        // Every bakeable scalar kind, in source order (which the fusion preserves).
        (
            r#"console.log(JSON.stringify({ s: "x", n: 100, b: false, z: null }));"#,
            r#"{"s":"x","n":100,"b":false,"z":null}"#,
        ),
        // String escaping must match `webjson::write_char`: quote, backslash,
        // newline and tab.
        (
            r#"console.log(JSON.stringify({ t: "a\"b\\c\n\td" }));"#,
            "{\"t\":\"a\\\"b\\\\c\\n\\td\"}",
        ),
        // Non-ASCII passes through raw (JSON permits it), as `pure_json` does.
        (
            r#"console.log(JSON.stringify({ e: "héllo→" }));"#,
            r#"{"e":"héllo→"}"#,
        ),
        // Multiple dynamic ints interleaved with constants.
        (
            r#"const a = 1; const b = 20000; console.log(JSON.stringify({ p: a, q: "mid", r: b }));"#,
            r#"{"p":1,"q":"mid","r":20000}"#,
        ),
    ];
    for (src, want) in cases {
        let prog = format!("import {{ console }} from \"std:console\";\nimport {{ JSON }} from \"browser:dom\";\n{src}\n");
        let want_line = format!("{want}\n");
        // Both the VM and the AST tree-walker compile through the same bytecode
        // compiler for the record… but only the VM path uses the fused template;
        // run the VM path and hold it to the written contract.
        let vm_out = run_program_with(prog.as_bytes(), "json-fusion.mersey", true);
        assert_eq!(vm_out, want_line, "VM fused JSON for: {src}");
    }
}
