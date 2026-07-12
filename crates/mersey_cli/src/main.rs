//! `mersey` — the standalone toolchain entry point.
//!
//! Implemented so far (Phase 1): `lex`, `check` (lexical checks only),
//! `convert`. `run`, `fmt`, `compile`, `audit` arrive in later phases.

mod lsp;

use std::process::ExitCode;

use mersey_front::{
    astdump, bind, check as tycheck, fmt as mfmt, graph, lexer, parser, source, sourcemap,
};
use mersey_interp as interp;

const USAGE: &str = "\
usage: mersey <command> [args]

commands:
  run [caps] <file>       check, then execute (bytecode VM; AST fallback)
                          caps: --allow-read --allow-env (deny by default, §5.3)
  audit <file.mersey>     report the module's import/capability surface
  lock <file.mersey>      write mersey.lock: content hashes for the graph
  verify <file.mersey>    check the graph against mersey.lock
  fmt [--write] <file>    format (canonical spacing/indentation, NFC, LF)
  compile <file.mersey>   check, then dump MBC bytecode (verified)
  sourcemap <file>        emit a Source Map v3 document on stdout
  lsp                     language server on stdin/stdout (LSP over JSON-RPC)
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
        ("lock", [file]) => lock_cmd(file, false),
        ("verify", [file]) => lock_cmd(file, true),
        ("compile", [file]) => compile_cmd(file),
        ("sourcemap", [file]) => sourcemap_cmd(file),
        ("lsp", []) => lsp::serve(),
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
            // Check the whole graph the entry module pulls in.
            let modules = match load_graph(path) {
                Ok(m) => m,
                Err(code) => return code,
            };
            if check_graph_ok(&modules) {
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
    fn time_ms(&mut self, epoch: bool) -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        if epoch {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0)
        } else {
            std::time::Instant::now().elapsed().as_secs_f64() * 1000.0
        }
    }
}

/// Load the whole module graph from disk (spec §4.5: closed before
/// execution). Returns modules in dependency-first order.
fn load_graph(entry: &str) -> Result<Vec<(String, &'static mersey_front::ast::Module)>, ExitCode> {
    use std::collections::HashMap;
    let mut sources: HashMap<String, &'static mersey_front::ast::Module> = HashMap::new();
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut queue = vec![entry.to_string()];
    let mut failed = false;

    while let Some(spec) = queue.pop() {
        if sources.contains_key(&spec) {
            continue;
        }
        let bytes = read(&spec)?;
        let src = match source::decode(&spec, &bytes) {
            Ok(s) => s,
            Err(d) => {
                eprintln!("{d}");
                return Err(ExitCode::FAILURE);
            }
        };
        let parsed = parser::parse(&src);
        if !parsed.diagnostics.is_empty() {
            for d in &parsed.diagnostics {
                eprintln!("{spec}: {d}");
            }
            failed = true;
        }
        let module: &'static _ = Box::leak(Box::new(parsed.module));
        let mut edges = Vec::new();
        for spec_import in graph::imports(module) {
            if graph::is_relative(&spec_import) {
                let target = graph::resolve(&spec, &spec_import);
                edges.push(target.clone());
                queue.push(target);
            }
        }
        deps.insert(spec.clone(), edges);
        sources.insert(spec, module);
    }
    if failed {
        return Err(ExitCode::FAILURE);
    }
    let order = match graph::topo_order(entry, &deps) {
        Ok(o) => o,
        Err(d) => {
            eprintln!("{d}");
            return Err(ExitCode::FAILURE);
        }
    };
    Ok(order.into_iter().map(|s| (s.clone(), sources[&s])).collect())
}

/// Bind + typecheck the whole graph; returns false if anything failed.
fn check_graph_ok(modules: &[(String, &'static mersey_front::ast::Module)]) -> bool {
    let mut ok = true;
    for (spec, module) in modules {
        let diags = bind::bind(module).diagnostics;
        if !diags.is_empty() {
            for d in &diags {
                eprintln!("{spec}: {d}");
            }
            ok = false;
        }
    }
    if !ok {
        return false;
    }
    let refs: Vec<(String, &mersey_front::ast::Module)> =
        modules.iter().map(|(s, m)| (s.clone(), *m)).collect();
    for (spec, out) in tycheck::check_graph(&refs) {
        for d in &out.diagnostics {
            eprintln!("{spec}: {d}");
            ok = false;
        }
    }
    ok
}

fn run(path: &str, caps: Vec<String>) -> ExitCode {
    let modules = match load_graph(path) {
        Ok(m) => m,
        Err(code) => return code,
    };
    if !check_graph_ok(&modules) {
        return ExitCode::FAILURE;
    }
    let host = CliHost { dom: std::collections::HashMap::new(), caps };
    let mut interp = interp::new_interp(Box::new(host));
    // Tier 1: register the Cranelift backend unless disabled (benchmarks
    // compare tiers via MERSEY_JIT=0).
    if std::env::var("MERSEY_JIT").as_deref() != Ok("0") {
        interp.jit = Some(mersey_jit::hook);
    }
    match interp.run_graph(modules) {
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

fn sourcemap_cmd(path: &str) -> ExitCode {
    let bytes = match read(path) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let src = match source::decode(path, &bytes) {
        Ok(s) => s,
        Err(d) => {
            eprintln!("{d}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", sourcemap::identity_map(path, &src.text));
    ExitCode::SUCCESS
}

/// Content-addressed lockfile for the module graph (spec §5.5). Every module
/// the program can load is hashed; `verify` fails if any of them changed.
fn lock_cmd(entry: &str, verify: bool) -> ExitCode {
    let modules = match load_graph(entry) {
        Ok(m) => m,
        Err(code) => return code,
    };
    let mut lines = vec![format!("# mersey.lock — entry: {entry}")];
    for (spec, _) in &modules {
        let Ok(bytes) = std::fs::read(spec) else {
            eprintln!("mersey: cannot read {spec}");
            return ExitCode::FAILURE;
        };
        lines.push(format!("{}  sha256-{}", spec, sha256_base64(&bytes)));
    }
    let content = lines.join("\n") + "\n";

    if !verify {
        if let Err(e) = std::fs::write("mersey.lock", &content) {
            eprintln!("mersey: cannot write mersey.lock: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote mersey.lock ({} modules)", modules.len());
        return ExitCode::SUCCESS;
    }

    match std::fs::read_to_string("mersey.lock") {
        Ok(existing) if existing == content => {
            println!("mersey.lock: {} modules verified", modules.len());
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("mersey: the module graph does not match mersey.lock");
            eprintln!("        (a dependency changed; re-run `mersey lock` if that was intended)");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("mersey: no mersey.lock (run `mersey lock` first)");
            ExitCode::FAILURE
        }
    }
}

/// SHA-256, base64 — no external crate (the engine ships none).
fn sha256_base64(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[i] = h[i].wrapping_add(v);
        }
    }

    let digest: Vec<u8> = h.iter().flat_map(|v| v.to_be_bytes()).collect();
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in digest.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}
