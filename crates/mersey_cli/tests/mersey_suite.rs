// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! The `*.test.mersey` suite — the standard library, tested in Mersey itself —
//! run under `cargo test` so CI gates it on every platform in the build matrix.
//!
//! `mersey test` loads each file through the real module-graph loader (the same
//! path a program takes) and reports one TAP line per case. Before this, that
//! suite ran only when someone typed `mersey test` by hand, so a regression in a
//! `std:` module written in Mersey — semver, path, base64, hex, uuid, mime, csv,
//! cookie, jwt, the language tests — could pass CI. Now it cannot.

use std::path::Path;
use std::process::Command;

#[test]
fn the_mersey_test_suite_passes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/mersey");
    let out = Command::new(env!("CARGO_BIN_EXE_mersey"))
        .arg("test")
        .arg(&dir)
        .output()
        .expect("run `mersey test`");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`mersey test {}` failed:\n{stdout}\n{stderr}",
        dir.display()
    );
    // Non-vacuous: the run must contain passing cases and report zero failures,
    // so a wrong path (which would find no files) cannot make this pass silently.
    //
    // Parsed, not matched as a substring. `!stdout.contains("0 passed, 0 failed")`
    // was the obvious way to write "it ran something" and it is wrong for every
    // total ending in a zero — the suite reaching 90 cases made a green run fail.
    let (passed, failed) = stdout
        .lines()
        .rev()
        .find_map(|l| {
            let (p, rest) = l.trim().split_once(" passed, ")?;
            let f = rest.strip_suffix(" failed")?;
            Some((p.parse::<u32>().ok()?, f.parse::<u32>().ok()?))
        })
        .unwrap_or_else(|| panic!("no `N passed, M failed` summary in:\n{stdout}"));
    assert_eq!(failed, 0, "the suite reported failures:\n{stdout}");
    assert!(
        passed > 0,
        "the suite ran no cases — check the path {}:\n{stdout}",
        dir.display()
    );
}
