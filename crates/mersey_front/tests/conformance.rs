//! Golden-file conformance runner (see tests/conformance/README.md at the
//! repository root). Set MERSEY_BLESS=1 to (re)generate `.expect` files.

use std::path::{Path, PathBuf};

use mersey_front::{
    astdump, bind, check, lexer, parser,
    source::{self, SourceFile},
};

fn conformance_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

fn parse_dump(src: &SourceFile) -> String {
    use std::fmt::Write;
    let out = parser::parse(src);
    let mut s = astdump::dump(&out.module);
    for d in &out.diagnostics {
        let _ = writeln!(s, "{d}");
    }
    s
}

#[test]
fn lexer_conformance() {
    run_dir("lexer", lexer::dump);
}

#[test]
fn parser_conformance() {
    run_dir("parser", parse_dump);
}

/// checker/ goldens hold diagnostics only (`ok` when clean) — the shape
/// `mersey check` output will keep as the type checker grows in.
fn check_dump(src: &SourceFile) -> String {
    use std::fmt::Write;
    let parsed = parser::parse(src);
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        diags = check::check(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        return "ok\n".to_string();
    }
    let mut s = String::new();
    for d in &diags {
        let _ = writeln!(s, "{d}");
    }
    s
}

#[test]
fn checker_conformance() {
    run_dir("checker", check_dump);
}

/// fmt/ goldens hold the formatter's exact output.
fn fmt_dump(src: &SourceFile) -> String {
    use std::fmt::Write;
    match mersey_front::fmt::format(src) {
        Ok(text) => text,
        Err(diags) => {
            let mut s = String::new();
            for d in &diags {
                let _ = writeln!(s, "{d}");
            }
            s
        }
    }
}

#[test]
fn fmt_conformance() {
    run_dir("fmt", fmt_dump);
}

fn run_dir(stage: &str, dump: fn(&SourceFile) -> String) {
    let dir = conformance_root().join(stage);
    let bless = std::env::var_os("MERSEY_BLESS").is_some();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "mersey"))
        .collect();
    cases.sort();
    assert!(
        !cases.is_empty(),
        "no conformance cases found in {}",
        dir.display()
    );

    let mut failures = Vec::new();
    for case in &cases {
        let bytes = std::fs::read(case).unwrap();
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let actual = match source::decode(&name, &bytes) {
            Ok(src) => dump(&src),
            Err(d) => format!("{d}\n"),
        };
        let expect_path = case.with_extension("expect");
        if bless {
            std::fs::write(&expect_path, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expect_path)
            .unwrap_or_else(|_| String::from("<missing .expect file>"));
        if actual != expected {
            failures.push(format!(
                "== {name}: output differs from {}\n--- expected\n{expected}\n--- actual\n{actual}",
                expect_path.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} conformance failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
