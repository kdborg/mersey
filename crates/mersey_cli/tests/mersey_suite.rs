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
    assert!(
        stdout.contains(" passed, 0 failed"),
        "expected `N passed, 0 failed`, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("0 passed, 0 failed"),
        "the suite ran no cases — check the path {}:\n{stdout}",
        dir.display()
    );
}
