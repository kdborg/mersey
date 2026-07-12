//! `mersey` — the standalone toolchain entry point.
//!
//! Implemented so far (Phase 1): `lex`, `check` (lexical checks only),
//! `convert`. `run`, `fmt`, `compile`, `audit` arrive in later phases.

use std::process::ExitCode;

use mersey_front::{astdump, bind, check as tycheck, fmt as mfmt, lexer, parser, source};
use mersey_interp as interp;

const USAGE: &str = "\
usage: mersey <command> [args]

commands:
  run [caps] <file>       check, then execute (bytecode VM; AST fallback)
                          caps: --allow-read --allow-env (deny by default, §5.3)
  audit <file.mersey>     report the module's import/capability surface
  fmt [--write] <file>    format (canonical spacing/indentation, NFC, LF)
  compile <file.mersey>   check, then dump MBC bytecode (verified)
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
        ("run", rest) if !rest.is_empty() => {
            let mut caps = Vec::new();
            let mut file = None;
            for a in rest {
                match a.as_str() {
                    "--allow-read" => caps.push("read".to_string()),
                    "--allow-env" => caps.push("env".to_string()),
                    other if !other.starts_with("--") && file.is_none() => {
                        file = Some(other.to_string())
                    }
                    other => {
                        eprintln!("mersey: unknown flag `{other}`");
                        return ExitCode::from(2);
                    }
                }
            }
            match file {
                Some(f) => run(&f, caps),
                None => {
                    eprint!("{USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        ("audit", [file]) => audit(file),
        ("compile", [file]) => compile_cmd(file),
        ("fmt", [file]) => fmt_cmd(file, false),
        ("fmt", [flag, file]) if flag == "--write" => fmt_cmd(file, true),
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
            // Each stage runs only when the previous one was clean, so a
            // broken parse doesn't cascade into noise.
            if diags.is_empty() {
                diags = bind::bind(&out.module).diagnostics;
            }
            if diags.is_empty() {
                diags = tycheck::check(&out.module).diagnostics;
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
/// behavior is testable without a browser. I/O is capability-gated
/// (spec §5.3): deny by default, enabled per `--allow-*` flag.
struct CliHost {
    dom: std::collections::HashMap<String, String>,
    caps: Vec<String>,
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
    fn read_text(&mut self, path: &str) -> Result<String, String> {
        if !self.caps.iter().any(|c| c == "read") {
            return Err("no `read` capability (run with --allow-read)".into());
        }
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
    }
    fn env_var(&mut self, name: &str) -> Option<String> {
        if self.caps.iter().any(|c| c == "env") {
            std::env::var(name).ok()
        } else {
            None
        }
    }
    fn caps(&self) -> Vec<String> {
        self.caps.clone()
    }
    fn drop_cap(&mut self, cap: &str) {
        self.caps.retain(|c| c != cap);
    }
}

fn run(path: &str, caps: Vec<String>) -> ExitCode {
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
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        diags = tycheck::check(&parsed.module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}: {d}", src.name);
        }
        return ExitCode::FAILURE;
    }
    // The interpreter borrows the AST for the process lifetime.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    let host = CliHost { dom: std::collections::HashMap::new(), caps };
    let mut interp = interp::new_interp(Box::new(host));
    // Tier 1: register the Cranelift backend unless disabled (benchmarks
    // compare tiers via MERSEY_JIT=0).
    if std::env::var("MERSEY_JIT").as_deref() != Ok("0") {
        interp.jit = Some(mersey_jit::hook);
    }
    match interp.run_module(module) {
        Ok(()) => ExitCode::SUCCESS,
        Err(t) => {
            eprintln!("runtime error: {}", interp.describe_thrown(&t));
            ExitCode::FAILURE
        }
    }
}

fn fmt_cmd(path: &str, write: bool) -> ExitCode {
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
    match mfmt::format(&src) {
        Ok(formatted) => {
            if write {
                if formatted != src.text {
                    if let Err(e) = std::fs::write(path, &formatted) {
                        eprintln!("mersey: cannot write {path}: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{formatted}");
            }
            ExitCode::SUCCESS
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("{}: {d}", src.name);
            }
            ExitCode::FAILURE
        }
    }
}

fn compile_cmd(path: &str) -> ExitCode {
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
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        diags = tycheck::check(&parsed.module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}: {d}", src.name);
        }
        return ExitCode::FAILURE;
    }
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    print!("{}", interp::vm::listing(module));
    ExitCode::SUCCESS
}

/// Static import/capability report (spec §5.5): computable exactly because
/// imports are static — no code runs.
fn audit(path: &str) -> ExitCode {
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
    println!("capability surface of {path}:");
    let mut any = false;
    for item in &parsed.module.items {
        let mersey_front::ast::Item::Import(im) = item else { continue };
        any = true;
        let names = match &im.clause {
            None => "(side effects)".to_string(),
            Some(mersey_front::ast::ImportClause::Namespace(n)) => format!("* as {}", n.text),
            Some(mersey_front::ast::ImportClause::Named(specs)) => specs
                .iter()
                .map(|s| s.name.text.clone())
                .collect::<Vec<_>>()
                .join(", "),
        };
        let cap = match im.from.as_str() {
            "std:fs" => "  [requires --allow-read]",
            "std:env" => "  [requires --allow-env]",
            "browser:dom" => "  [browser: DOM access]",
            _ => "",
        };
        println!("  {} <- \"{}\"{cap}", names, im.from);
    }
    if !any {
        println!("  (no imports: pure computation)");
    }
    ExitCode::SUCCESS
}
