//! The engine-side debugger foundation (`DebugHook`): statement callouts
//! carry positions, the call stack, and on-demand locals — the layer a DAP
//! adapter (standalone) or CDP agent (browser) drives. These tests are the
//! behavioral contract: which lines report, what the stack and locals show
//! at a "breakpoint", and that an attached-but-passive hook changes nothing
//! about program output.

use std::cell::RefCell;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, DebugHook, DebugPause, Host};

struct TestHost {
    out: Rc<RefCell<String>>,
}

impl Host for TestHost {
    fn print(&mut self, s: &str) {
        let mut out = self.out.borrow_mut();
        out.push_str(s);
        out.push('\n');
    }
    fn dom_set_text(&mut self, _id: &str, _text: &str) {}
    fn dom_get_text(&mut self, _id: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}
}

/// Record every callout; at `break_line`, capture the stack and locals.
struct Recorder {
    lines: Rc<RefCell<Vec<u32>>>,
    break_line: u32,
    hit: Rc<RefCell<Vec<(Vec<String>, Vec<Vec<(String, String)>>)>>>,
}

impl DebugHook for Recorder {
    fn on_stmt(
        &mut self,
        pause: &DebugPause,
        locals: &mut dyn FnMut() -> Vec<Vec<(String, String)>>,
    ) {
        self.lines.borrow_mut().push(pause.pos.line);
        if pause.pos.line == self.break_line {
            let stack: Vec<String> = pause.frames.iter().map(|f| f.name.to_string()).collect();
            self.hit.borrow_mut().push((stack, locals()));
        }
    }
}

fn run_with_hook(src_text: &str, break_line: u32) -> (String, Vec<u32>, Vec<(Vec<String>, Vec<Vec<(String, String)>>)>) {
    let src = source::decode("debug.mersey", src_text.as_bytes()).expect("decodes");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let b = bind::bind(module);
    assert!(b.diagnostics.is_empty(), "bind: {:?}", b.diagnostics);
    let c = check::check(module);
    assert!(c.diagnostics.is_empty(), "check: {:?}", c.diagnostics);

    let buffer = Rc::new(RefCell::new(String::new()));
    let mut interp = new_interp(Box::new(TestHost { out: buffer.clone() }));
    let lines = Rc::new(RefCell::new(Vec::new()));
    let hit = Rc::new(RefCell::new(Vec::new()));
    interp.set_debug_hook(Box::new(Recorder {
        lines: lines.clone(),
        break_line,
        hit: hit.clone(),
    }));
    assert!(interp.run_module(module).is_ok(), "program runs");
    let out = buffer.borrow().clone();
    let result = (out, lines.borrow().clone(), hit.borrow().clone());
    result
}

const PROGRAM: &str = "\
import { console } from \"std:console\";

function double(x: int32): int32 {
    const twice = x * 2;
    return twice;
}

let total = 0;
let i = 0;
while (i < 3) {
    total += double(i);
    i += 1;
}
console.log(`total ${total}`);
";

#[test]
fn statements_report_their_lines() {
    let (out, lines, _) = run_with_hook(PROGRAM, 0);
    assert_eq!(out, "total 6\n");
    // Top-level: let(8), let(9), while(10); each iteration: the body stmts
    // (11, then inside double 4 and 5, then 12). Three iterations, then the
    // final log on 14. The import is module-graph plumbing, not a statement.
    let expected: Vec<u32> = {
        let mut v = vec![8, 9, 10];
        for _ in 0..3 {
            v.extend([11, 4, 5, 12]);
        }
        v.push(14);
        v
    };
    assert_eq!(lines, expected, "callout line sequence");
}

#[test]
fn breakpoint_sees_stack_and_locals() {
    let (_, _, hits) = run_with_hook(PROGRAM, 5);
    assert_eq!(hits.len(), 3, "line 5 (the return) runs once per call");
    let (stack, scopes) = &hits[0];
    // The innermost frame is the function; the module frame is below it.
    assert_eq!(stack.last().map(|s| s.as_str()), Some("double"));
    assert!(stack.len() >= 2, "module frame below the call: {stack:?}");
    // Innermost scope: the function body's `twice`; a parent holds `x`.
    let flat: Vec<(String, String)> =
        scopes.iter().flatten().cloned().collect();
    let get = |name: &str| {
        flat.iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("twice").as_deref(), Some("0"), "locals at first hit");
    assert_eq!(get("x").as_deref(), Some("0"));
}

#[test]
fn passive_hook_changes_nothing() {
    struct Passive;
    impl DebugHook for Passive {
        fn on_stmt(
            &mut self,
            _pause: &DebugPause,
            _locals: &mut dyn FnMut() -> Vec<Vec<(String, String)>>,
        ) {
        }
    }
    let src = source::decode("p.mersey", PROGRAM.as_bytes()).expect("decodes");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let _ = bind::bind(module);
    let _ = check::check(module);
    let buffer = Rc::new(RefCell::new(String::new()));
    let mut interp = new_interp(Box::new(TestHost { out: buffer.clone() }));
    interp.set_debug_hook(Box::new(Passive));
    assert!(interp.run_module(module).is_ok(), "program runs");
    assert_eq!(buffer.borrow().as_str(), "total 6\n");
}
