//! `mersey` — the standalone toolchain entry point.
//!
//! Implemented so far (Phase 1): `lex`, `check` (lexical checks only),
//! `convert`. `run`, `fmt`, `compile`, `audit` arrive in later phases.

use std::process::ExitCode;

use mersey_front::{astdump, bind, lexer, parser, source};
use mersey_interp as interp;

const USAGE: &str = "\
usage: mersey <command> [args]

commands:
  run <file.mersey>       check, then execute (MVP tree-walking interpreter)
  check <file.mersey>     report diagnostics (currently: encoding + syntax)
  parse <file.mersey>     dump the AST (debugging / conformance)
  lex <file.mersey>       dump the token stream (debugging / conformance)
  convert <file>          transcode UTF-16/UTF-32 source to UTF-8 on stdout
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((cmd, rest)) => (cmd.as_str(), rest),
        None => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match (cmd, rest) {
        ("run", [file]) => run(file),
        ("check", [file]) => check(file, Mode::Check),
        ("lex", [file]) => check(file, Mode::LexDump),
        ("parse", [file]) => check(file, Mode::ParseDump),
        ("convert", [file]) => convert(file),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

enum Mode {
    Check,
    LexDump,
    ParseDump,
}

fn read(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| {
        eprintln!("mersey: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn check(path: &str, mode: Mode) -> ExitCode {
    let bytes = match read(path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let src = match source::decode(path, &bytes) {
        Ok(src) => src,
        Err(d) => {
            eprintln!("{d}");
            return ExitCode::FAILURE;
        }
    };
    match mode {
        Mode::LexDump => {
            print!("{}", lexer::dump(&src));
            ExitCode::SUCCESS
        }
        Mode::ParseDump => {
            let out = parser::parse(&src);
            print!("{}", astdump::dump(&out.module));
            for d in &out.diagnostics {
                println!("{d}");
            }
            ExitCode::SUCCESS
        }
        Mode::Check => {
            let out = parser::parse(&src);
            let mut diags = out.diagnostics;
            // Binding a syntactically broken module would double-report;
            // only bind when the parse is clean.
            if diags.is_empty() {
                diags = bind::bind(&out.module).diagnostics;
            }
            for d in &diags {
                eprintln!("{}: {d}", src.name);
            }
            if diags.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Transcode a UTF-16/UTF-32 source file (BOM required, since that is how
/// `.mersey` files are rejected by `decode`) to UTF-8 on stdout.
fn convert(path: &str) -> ExitCode {
    let bytes = match read(path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let text = match transcode(&bytes) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("mersey: {path}: {msg}");
            return ExitCode::FAILURE;
        }
    };
    print!("{text}");
    ExitCode::SUCCESS
}

fn transcode(bytes: &[u8]) -> Result<String, String> {
    match bytes {
        [0xFF, 0xFE, 0x00, 0x00, rest @ ..] => decode_utf32(rest, u32::from_le_bytes),
        [0x00, 0x00, 0xFE, 0xFF, rest @ ..] => decode_utf32(rest, u32::from_be_bytes),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => match std::str::from_utf8(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)) {
            Ok(t) => Ok(t.to_string()), // already UTF-8; normalize BOM away
            Err(_) => Err("no UTF-16/UTF-32 BOM found and the file is not valid UTF-8".into()),
        },
    }
}

fn decode_utf32(rest: &[u8], from_bytes: fn([u8; 4]) -> u32) -> Result<String, String> {
    if rest.len() % 4 != 0 {
        return Err("truncated UTF-32 data".into());
    }
    rest.chunks_exact(4)
        .map(|c| {
            let unit = from_bytes([c[0], c[1], c[2], c[3]]);
            char::from_u32(unit).ok_or_else(|| format!("invalid code point U+{unit:X}"))
        })
        .collect()
}

fn decode_utf16(rest: &[u8], from_bytes: fn([u8; 2]) -> u16) -> Result<String, String> {
    if rest.len() % 2 != 0 {
        return Err("truncated UTF-16 data".into());
    }
    let units: Vec<u16> = rest.chunks_exact(2).map(|c| from_bytes([c[0], c[1]])).collect();
    String::from_utf16(&units).map_err(|_| "unpaired surrogate in UTF-16 data".into())
}

/// CLI host: console goes to stdout; DOM calls render as text so runtime
/// behavior is testable without a browser.
struct CliHost {
    dom: std::collections::HashMap<String, String>,
}

impl interp::Host for CliHost {
    fn print(&mut self, s: &str) {
        println!("{s}");
    }
    fn dom_set_text(&mut self, id: &str, text: &str) {
        self.dom.insert(id.to_string(), text.to_string());
        println!("[dom #{id}] {text}");
    }
    fn dom_get_text(&mut self, id: &str) -> Option<String> {
        self.dom.get(id).cloned()
    }
    fn dom_on_click(&mut self, id: &str, cb: u32) {
        println!("[dom #{id}] click handler #{cb} registered");
    }
}

fn run(path: &str) -> ExitCode {
    let bytes = match read(path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let src = match source::decode(path, &bytes) {
        Ok(src) => src,
        Err(d) => {
            eprintln!("{d}");
            return ExitCode::FAILURE;
        }
    };
    let parsed = parser::parse(&src);
    let diags = if parsed.diagnostics.is_empty() {
        bind::bind(&parsed.module).diagnostics
    } else {
        parsed.diagnostics
    };
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}: {d}", src.name);
        }
        return ExitCode::FAILURE;
    }
    // The interpreter borrows the AST for the process lifetime.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let host = CliHost { dom: std::collections::HashMap::new() };
    let mut interp = interp::new_interp(Box::new(host));
    match interp.run_module(module) {
        Ok(()) => ExitCode::SUCCESS,
        Err(t) => {
            eprintln!("runtime error: {}", interp.describe_thrown(&t));
            ExitCode::FAILURE
        }
    }
}
