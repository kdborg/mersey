//! The REPL (`mersey repl`) — a host feature over a language with no `eval`:
//! the session is one growing module. Each turn appends the input to the
//! accumulated source, re-parses and re-checks the WHOLE program (so the
//! checker sees every prior declaration and rejects ill-typed input before
//! anything runs), then executes only the new items in a persistent
//! interpreter (`Interp::run_repl_turn`). Rejected input is discarded — the
//! session's program always typechecks.
//!
//! A trailing bare expression echoes its value. `std:console` is pre-imported.
//! Multi-line input: a line with unclosed brackets keeps reading under a
//! `....>` prompt (the scanner respects strings, templates, and comments).

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use mersey_interp as interp;
use mersey_interp::{ReplOutcome, ReplSession};

/// Net bracket depth of a fragment, ignoring brackets inside strings,
/// templates, chars, and comments. Positive means "keep reading".
fn open_depth(text: &str) -> i32 {
    let mut depth = 0i32;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            '"' | '\'' | '`' => {
                // Skip to the matching close, honoring escapes. Template
                // interpolations nest brackets, but the outer scan only
                // needs "still inside something open" — treating the whole
                // template as opaque is safe for that purpose.
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == c {
                        break;
                    }
                }
            }
            '/' => match chars.peek() {
                Some('/') => {
                    while let Some(&n) = chars.peek() {
                        if n == '\n' {
                            break;
                        }
                        chars.next();
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = ' ';
                    for n in chars.by_ref() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    depth
}

pub fn serve(caps: Vec<String>) -> ExitCode {
    struct CapsHost(Vec<String>);
    impl interp::Host for CapsHost {
        fn print(&mut self, s: &str) {
            println!("{s}");
        }
        fn dom_set_text(&mut self, _id: &str, _text: &str) {}
        fn dom_get_text(&mut self, _id: &str) -> Option<String> {
            None
        }
        fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}
        fn caps(&self) -> Vec<String> {
            self.0.clone()
        }
    }

    println!("Mersey REPL — one growing module; :quit or Ctrl-D exits.");
    println!("`console` is pre-imported; a bare expression echoes its value.");

    let mut interp = interp::new_interp(Box::new(CapsHost(caps)));
    let mut session = ReplSession::new();
    // The prelude is an ordinary first turn: `console` exists from turn one.
    if !matches!(
        session.turn(&mut interp, "import { console } from \"std:console\";"),
        ReplOutcome::Ran(_)
    ) {
        eprintln!("mersey repl: prelude failed");
        return ExitCode::FAILURE;
    }

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("mersey> ");
        let _ = io::stdout().flush();
        let Some(Ok(first)) = lines.next() else { break };
        if first.trim() == ":quit" {
            break;
        }
        if first.trim().is_empty() {
            continue;
        }
        let mut fragment = first;
        while open_depth(&fragment) > 0 {
            print!("  ....> ");
            let _ = io::stdout().flush();
            let Some(Ok(more)) = lines.next() else { break };
            fragment.push('\n');
            fragment.push_str(&more);
        }

        match session.turn(&mut interp, &fragment) {
            ReplOutcome::Ran(Some(echo)) => println!("{echo}"),
            ReplOutcome::Ran(None) => {}
            ReplOutcome::Rejected(diags) => eprintln!("{diags}"),
            ReplOutcome::Threw(msg) => eprintln!("{msg}"),
        }
    }
    ExitCode::SUCCESS
}
