//! The REPL contract, against the real binary: echo, multi-line definitions,
//! ill-typed input rejected without harming the session, runtime errors
//! survived, state persistent across turns.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn session_grows_and_survives_errors() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mersey"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    let script = "\
let x = 21
x * 2
function double(n: int32): int32 {
return n * 2;
}
double(x) + 1
let y: int32 = \"no\"
x
[1][9]
x
console.log(`hi ${x}`)
:quit
";
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(script.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "repl exit: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Echoes, in order; the session keeps working after both error kinds.
    let mut rest = &*stdout;
    for expected in ["42", "43", "21", "21", "hi 21"] {
        let at = rest.find(expected).unwrap_or_else(|| {
            panic!("missing `{expected}` in order; stdout:\n{stdout}\nstderr:\n{stderr}")
        });
        rest = &rest[at + expected.len()..];
    }
    // The ill-typed turn was rejected by the checker…
    assert!(stderr.contains("E0401"), "type error surfaced: {stderr}");
    // …and the out-of-bounds index surfaced as a runtime error.
    assert!(
        stderr.contains("runtime error:"),
        "runtime error surfaced: {stderr}"
    );
    // The rejected binding never entered the session.
    assert!(
        !stdout.contains("no"),
        "discarded input left no trace: {stdout}"
    );
}
