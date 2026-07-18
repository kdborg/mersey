//! End-to-end DAP session against the real `mersey dap` binary: configure,
//! break, inspect the stack (including the caller's call-site line), read
//! locals, step, re-hit the breakpoint, run to completion, disconnect. This
//! is the ROADMAP Phase 6 "set a breakpoint in a `.mersey` file" contract,
//! standalone half.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const PROGRAM: &str = "\
import { console } from \"std:console\";

function add(a: int32, b: int32): int32 {
    const r = a + b;
    return r;
}

let x = add(1, 2);
let y = add(x, 3);
console.log(`y ${y}`);
";

struct Dap {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    seq: u64,
}

impl Dap {
    fn send(&mut self, command: &str, arguments: &str) {
        self.seq += 1;
        let body = format!(
            "{{\"seq\":{},\"type\":\"request\",\"command\":\"{command}\",\"arguments\":{arguments}}}",
            self.seq
        );
        let stdin = self.child.stdin.as_mut().expect("stdin");
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("write");
        stdin.flush().expect("flush");
    }

    /// Read one framed message.
    fn read(&mut self) -> String {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            assert!(
                self.reader.read_line(&mut line).expect("read header") > 0,
                "adapter closed its stdout"
            );
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().expect("length");
            }
        }
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).expect("read body");
        String::from_utf8(buf).expect("utf8")
    }

    /// Read messages until one contains `needle`; returns it. Everything
    /// skipped must be protocol traffic, not an error response.
    fn read_until(&mut self, needle: &str) -> String {
        for _ in 0..50 {
            let msg = self.read();
            if msg.contains(needle) {
                return msg;
            }
        }
        panic!("never saw {needle}");
    }
}

#[test]
fn breakpoint_step_inspect_continue() {
    let dir = std::env::temp_dir();
    let program = dir.join("mersey-dap-test.mersey");
    std::fs::write(&program, PROGRAM).expect("write program");
    let program_path = program.to_string_lossy().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_mersey"))
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mersey dap");
    let reader = BufReader::new(child.stdout.take().expect("stdout"));

    // Watchdog: a wedged protocol must fail the test, not hang the suite.
    let pid = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    });

    let mut dap = Dap { child, reader, seq: 0 };

    dap.send("initialize", "{}");
    dap.read_until("\"event\":\"initialized\"");
    dap.send("launch", &format!("{{\"program\":{:?}}}", program_path));
    dap.read_until("\"command\":\"launch\"");
    dap.send("setBreakpoints", "{\"breakpoints\":[{\"line\":4}]}");
    let bp = dap.read_until("\"command\":\"setBreakpoints\"");
    assert!(bp.contains("\"verified\":true"), "breakpoint verified: {bp}");
    dap.send("configurationDone", "{}");
    dap.read_until("\"command\":\"configurationDone\"");

    // First hit: inside add(1, 2), before `const r` runs.
    let stopped = dap.read_until("\"event\":\"stopped\"");
    assert!(stopped.contains("\"reason\":\"breakpoint\""), "{stopped}");

    dap.send("stackTrace", "{\"threadId\":1}");
    let stack = dap.read_until("\"command\":\"stackTrace\"");
    assert!(stack.contains("\"name\":\"add\""), "top frame: {stack}");
    assert!(stack.contains("\"line\":4"), "paused line: {stack}");
    // The caller's frame shows its call-site line (let x = add(1, 2) is 8).
    assert!(stack.contains("\"line\":8"), "call-site line: {stack}");

    dap.send("scopes", "{\"frameId\":0}");
    let scopes = dap.read_until("\"command\":\"scopes\"");
    assert!(scopes.contains("\"name\":\"Locals\""), "{scopes}");

    dap.send("variables", "{\"variablesReference\":1}");
    let vars = dap.read_until("\"command\":\"variables\"");
    assert!(
        vars.contains("\"name\":\"a\"") && vars.contains("\"value\":\"1\""),
        "param a=1 visible: {vars}"
    );

    // Outer frame (the module, frameId 1): its scope chain resolves too —
    // `add` itself is bound there.
    dap.send("scopes", "{\"frameId\":1}");
    let outer_scopes = dap.read_until("\"command\":\"scopes\"");
    assert!(outer_scopes.contains("\"name\":\"Locals\""), "{outer_scopes}");
    dap.send("variables", "{\"variablesReference\":65}");
    let outer_vars = dap.read_until("\"command\":\"variables\"");
    assert!(outer_vars.contains("\"name\":\"add\""), "module scope shows add: {outer_vars}");

    // Step over: to `return r` on line 5, with r now bound.
    dap.send("next", "{\"threadId\":1}");
    let step = dap.read_until("\"event\":\"stopped\"");
    assert!(step.contains("\"reason\":\"step\""), "{step}");
    dap.send("stackTrace", "{\"threadId\":1}");
    let stack2 = dap.read_until("\"command\":\"stackTrace\"");
    assert!(stack2.contains("\"line\":5"), "stepped to return: {stack2}");
    dap.send("variables", "{\"variablesReference\":1}");
    let vars2 = dap.read_until("\"command\":\"variables\"");
    assert!(
        vars2.contains("\"name\":\"r\"") && vars2.contains("\"value\":\"3\""),
        "r=3 after step: {vars2}"
    );

    // Second call re-hits the same breakpoint.
    dap.send("continue", "{\"threadId\":1}");
    let stopped2 = dap.read_until("\"event\":\"stopped\"");
    assert!(stopped2.contains("\"reason\":\"breakpoint\""), "{stopped2}");

    // Run out: program output arrives as an output event, then exit events.
    dap.send("continue", "{\"threadId\":1}");
    let out = dap.read_until("\"event\":\"output\"");
    assert!(out.contains("y 6"), "program output: {out}");
    dap.read_until("\"event\":\"exited\"");
    dap.read_until("\"event\":\"terminated\"");

    dap.send("disconnect", "{}");
    dap.read_until("\"command\":\"disconnect\"");
    let status = dap.child.wait().expect("wait");
    assert!(status.success(), "adapter exit: {status}");
}
