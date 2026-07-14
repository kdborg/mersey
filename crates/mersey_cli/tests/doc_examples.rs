//! The library reference's examples are *programs*, and this runs them.
//!
//! A documentation example that is not executed is a claim nobody checks: it
//! rots the first time the API changes, and the only person who finds out is the
//! reader. Each `docs/examples/*.mersey` is run here and its output compared
//! against the `.expect` beside it — which is also what the reference prints
//! under the code. Re-bless with `MERSEY_BLESS=1`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn examples() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("docs/examples")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mersey"))
        .collect();
    v.sort();
    v
}

/// An example declares the capabilities it needs in a `// caps:` header — the
/// same grant it would be launched with. Anything not declared is denied, which
/// is how the `fs` example can honestly show what refusal looks like.
fn caps_of(src: &str) -> Vec<String> {
    src.lines()
        .take_while(|l| l.starts_with("//"))
        .find_map(|l| {
            l.trim_start_matches('/')
                .trim()
                .strip_prefix("caps:")
                .map(str::to_string)
        })
        .map(|s| {
            s.split_whitespace()
                .map(|c| format!("--allow-{c}"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn every_library_example_runs_and_prints_what_the_docs_say() {
    let bless = std::env::var("MERSEY_BLESS").is_ok();
    let mut failures = Vec::new();

    for path in examples() {
        let src = std::fs::read_to_string(&path).unwrap();

        // A browser example cannot run here — there is no DOM to run it against.
        // It is *typechecked* instead, against the real generated web surface, so
        // it still cannot name a global or a member that does not exist; and the
        // page shows it with no output, because none was produced. The web
        // platform is proved end-to-end elsewhere, by `web/test/platform.mjs`
        // driving a live realm.
        if src
            .lines()
            .take_while(|l| l.starts_with("//"))
            .any(|l| l.trim_start_matches('/').trim() == "browser")
        {
            let out = Command::new(env!("CARGO_BIN_EXE_mersey"))
                .arg("check")
                .arg(&path)
                .output()
                .expect("run mersey");
            if !out.status.success() {
                failures.push(format!(
                    "== {} (browser example failed to typecheck)\n{}{}",
                    path.file_name().unwrap().to_string_lossy(),
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                ));
            }
            continue;
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mersey"));
        cmd.arg("run");
        for c in caps_of(&src) {
            cmd.arg(c);
        }
        let out = cmd.arg(&path).output().expect("run mersey");

        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));

        let expect_path = path.with_extension("expect");
        if bless {
            std::fs::write(&expect_path, &text).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expect_path).unwrap_or_default();
        if text != expected {
            failures.push(format!(
                "== {}\n--- expected\n{expected}\n--- actual\n{text}",
                path.file_name().unwrap().to_string_lossy()
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

/// Every group in the reference must have an example. A page of signatures with
/// no usage is a page nobody can learn from.
#[test]
fn every_library_group_has_an_example() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples");
    for g in mersey_front::check::api_reference() {
        let own = dir.join(format!("{}.mersey", g.key));
        // A class exported by a `std:` module may lean on that module's example.
        let inherited = dir.join(format!("{}.mersey", g.parent));
        assert!(
            own.exists() || (!g.parent.is_empty() && inherited.exists()),
            "`{}` is in the library reference but has no example (expected {})",
            g.title,
            own.display()
        );
    }
}
