//! Runtime conformance: golden stdout of executing each program (see
//! tests/conformance/README.md). MERSEY_BLESS=1 regenerates goldens.
//! These goldens are the behavioral contract the Phase 2 bytecode VM (and
//! later the JIT) must reproduce exactly.

use std::path::{Path, PathBuf};

use mersey_front::{bind, parser, source};
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
    fn dom_on_click(&mut self, id: &str, cb: u32) {
        self.emit(format!("[dom #{id}] click handler #{cb} registered"));
    }
}

fn run_program(bytes: &[u8], name: &str) -> String {
    let src = match source::decode(name, bytes) {
        Ok(s) => s,
        Err(d) => return format!("{d}\n"),
    };
    let parsed = parser::parse(&src);
    let diags = if parsed.diagnostics.is_empty() {
        bind::bind(&parsed.module).diagnostics
    } else {
        parsed.diagnostics
    };
    if !diags.is_empty() {
        let mut s = String::new();
        for d in &diags {
            s.push_str(&format!("{d}\n"));
        }
        return s;
    }
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let buffer = Rc::new(RefCell::new(String::new()));
    let host = Box::new(TestHost { out: buffer.clone(), dom: Default::default() });
    let mut interp = new_interp(host);
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
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}
