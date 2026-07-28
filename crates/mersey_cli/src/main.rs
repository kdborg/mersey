// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! `mersey` — the standalone toolchain entry point.
//!
//! Implemented so far (Phase 1): `lex`, `check` (lexical checks only),
//! `convert`. `run`, `fmt`, `compile`, `audit` arrive in later phases.

mod dap;
mod doc;
mod lsp;
mod repl;

use std::io::IsTerminal;
use std::process::ExitCode;

use rand::RngCore;

/// See the note on the dependency: the engine allocates a box per value, so the
/// allocator *is* the inner loop. This is the `mersey` binary's own choice and
/// affects nothing that embeds the engine.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use mersey_front::{
    astdump, bind, check as tycheck, fmt as mfmt, graph, lexer, parser, source, sourcemap,
};
use mersey_interp as interp;

const USAGE: &str = "\
usage: mersey <command> [args]

commands:
  run [caps] <file>       check, then execute (bytecode VM; AST fallback)
                          caps: --allow-read --allow-env --allow-random --allow-net
                          (deny by default, §5.3)
  serve [-jN] [caps] <file>  run an HTTP program (net.serve); -jN worker
                          processes share the port (SO_REUSEPORT). Implies net.
  audit <file.mersey>     report the module's import/capability surface
  lock <file.mersey>      write mersey.lock: content hashes for the graph
  verify <file.mersey>    check the graph against mersey.lock
  fmt [--write] <file>    format (canonical spacing/indentation, NFC, LF)
  compile <file.mersey>   check, then dump MBC bytecode (verified)
  sourcemap <file>        emit a Source Map v3 document on stdout
  lsp                     language server on stdin/stdout (LSP over JSON-RPC)
  dap                     debug adapter on stdin/stdout (DAP; breakpoints, stepping)
  repl [caps]             interactive session (one growing module; no eval)
  check <file.mersey>     report diagnostics (currently: encoding + syntax)
  parse <file.mersey>     dump the AST (debugging / conformance)
  test [path]             run every *.test.mersey (default: ./)
  doc [outdir]            build the documentation site (default: ./site)
  fetch <file.mersey>     download remote imports into .mersey/cache, pin hashes
  lex <file.mersey>       dump the token stream (debugging / conformance)
  convert <file>          transcode UTF-16/UTF-32 source to UTF-8 on stdout

global options:
  --hide-experimental-message   suppress the experimental notice (until 1.0.0)
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Until 1.0.0, an interactive run prints a one-line experimental notice to
    // stderr (never stdout, so piped/redirected output stays clean). It is
    // gated on an interactive stderr so captured runs — tests, CI, scripts that
    // read the output — never see it, and is force-hidden by
    // --hide-experimental-message, which is accepted anywhere on the command
    // line and stripped here so the per-command parsers never see it. This
    // whole block goes away when 1.0.0 ships.
    let hide_experimental = args.iter().any(|a| a == "--hide-experimental-message");
    args.retain(|a| a != "--hide-experimental-message");
    if !hide_experimental && std::io::stderr().is_terminal() {
        eprintln!(
            "mersey {} — EXPERIMENTAL: not for production use; behaviour may change \
             before 1.0.0. Hide this with --hide-experimental-message.",
            env!("CARGO_PKG_VERSION")
        );
    }
    let (cmd, rest) = match args.split_first() {
        Some((cmd, rest)) => (cmd.as_str(), rest),
        None => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match (cmd, rest) {
        // Transpile one module to JavaScript on stdout (the JS-backend
        // polyfill; conformance-gated against the engine).
        ("js", rest2) if matches!(rest2, [_] | [_, _]) => {
            let (with_map, file) = match rest2 {
                [flag, f] if flag == "--map" => (true, f),
                [f] => (false, f),
                _ => {
                    eprintln!("usage: mersey js [--map] <file.mersey>");
                    return ExitCode::from(2);
                }
            };
            let src = match std::fs::read(file) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{file}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let src = String::from_utf8_lossy(&src);
            let out = mersey_js::transpile(&src, file, true);
            if !out.diagnostics.is_empty() {
                for d in &out.diagnostics {
                    eprintln!("{d}");
                }
                return ExitCode::FAILURE;
            }
            print!("{}", out.js);
            if with_map {
                print!("{}", out.map);
            }
            ExitCode::SUCCESS
        }
        ("run", rest) if !rest.is_empty() => {
            let mut caps = Vec::new();
            let mut file = None;
            for a in rest {
                match a.as_str() {
                    "--allow-read" => caps.push("read".to_string()),
                    "--allow-env" => caps.push("env".to_string()),
                    "--allow-random" => caps.push("random".to_string()),
                    "--allow-net" => caps.push("net".to_string()),
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
        // `serve [-jN] [caps] <file>` — run a program that calls `net.serve`,
        // then drive its accept loop. `net` is implied (the command *is* the
        // grant). With -jN>1 it launches N worker processes that share the
        // listening port via SO_REUSEPORT (the Rc-based engine is per-process
        // single-threaded; parallelism is processes, not threads).
        ("serve", rest) if !rest.is_empty() => {
            let mut caps = vec!["net".to_string()];
            let mut file = None;
            let mut jobs: usize = 1;
            for a in rest {
                match a.as_str() {
                    "--allow-read" => caps.push("read".to_string()),
                    "--allow-env" => caps.push("env".to_string()),
                    "--allow-random" => caps.push("random".to_string()),
                    "--allow-net" => {} // implied
                    j if j.starts_with("-j") => match j[2..].parse::<usize>() {
                        Ok(n) if n >= 1 => jobs = n,
                        _ => {
                            eprintln!("mersey: -j needs a positive integer, got `{j}`");
                            return ExitCode::from(2);
                        }
                    },
                    other if !other.starts_with('-') && file.is_none() => {
                        file = Some(other.to_string())
                    }
                    other => {
                        eprintln!("mersey: unknown flag `{other}`");
                        return ExitCode::from(2);
                    }
                }
            }
            match file {
                Some(f) => serve_cmd(&f, caps, jobs),
                None => {
                    eprint!("{USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        ("test", rest) => {
            let path = rest.first().map(|s| s.as_str()).unwrap_or(".");
            test_cmd(path)
        }
        ("doc", rest) => doc::build(rest.first().map(|s| s.as_str()).unwrap_or("site")),
        ("fetch", [file]) => fetch_cmd(file),
        ("audit", [file]) => audit(file),
        ("lock", [file]) => lock_cmd(file, false),
        ("verify", [file]) => lock_cmd(file, true),
        ("compile", [file]) => compile_cmd(file),
        ("sourcemap", [file]) => sourcemap_cmd(file),
        ("lsp", []) => lsp::serve(),
        ("dap", []) => dap::serve(),
        ("repl", rest) => {
            let mut caps = Vec::new();
            for a in rest {
                match a.as_str() {
                    "--allow-read" => caps.push("read".to_string()),
                    "--allow-env" => caps.push("env".to_string()),
                    "--allow-random" => caps.push("random".to_string()),
                    other => {
                        eprintln!("mersey: unknown flag `{other}`");
                        return ExitCode::from(2);
                    }
                }
            }
            repl::serve(caps)
        }
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
                Ok(g) => g.all,
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
    if !rest.len().is_multiple_of(4) {
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
    if !rest.len().is_multiple_of(2) {
        return Err("truncated UTF-16 data".into());
    }
    let units: Vec<u16> = rest
        .chunks_exact(2)
        .map(|c| from_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| "unpaired surrogate in UTF-16 data".into())
}

/// CLI host: console goes to stdout; DOM calls render as text so runtime
/// behavior is testable without a browser. I/O is capability-gated
/// (spec §5.3): deny by default, enabled per `--allow-*` flag.
struct CliHost {
    dom: std::collections::HashMap<String, String>,
    caps: Vec<String>,
    /// (port, callback id) recorded by a `net.serve` call, consumed by the
    /// driver's accept loop after top-level completes.
    pending_server: Option<(u16, u32)>,
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
    fn dom_add_listener(&mut self, id: &str, event: &str, cb: u32) {
        println!("[dom #{id}] {event} handler #{cb} registered");
    }
    fn read_text(&mut self, path: &str) -> Result<String, String> {
        if !self.caps.iter().any(|c| c == "read") {
            return Err("no `read` capability (run with --allow-read)".into());
        }
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
    }
    /// A warning or an error belongs on stderr: it survives `mersey run app > out`,
    /// which is the whole reason a level exists.
    fn print_level(&mut self, level: &str, s: &str) {
        match level {
            "warn" | "error" => eprintln!("{s}"),
            _ => println!("{s}"),
        }
    }

    /// A CSPRNG, and only with the capability.
    ///
    /// The obvious implementation — one `getrandom(2)`/`getentropy`/
    /// `BCryptGenRandom` per call — is a syscall per call, ~1.1us even for 16
    /// bytes, and it made `random.bytes` the single worst number in the
    /// command-line benchmark. Every serious crypto library answers this the
    /// same way, so this does too: `rand::ThreadRng` is a ChaCha12 DRBG seeded
    /// from that same OS entropy, reseeded every 64 KiB, and reseeded again in
    /// the child after `fork()` (without which two processes would draw the
    /// identical stream — the one way this construction goes wrong).
    fn random_bytes(&mut self, n: usize) -> Result<Vec<u8>, String> {
        if !self.caps.iter().any(|c| c == "random") {
            return Err("no `random` capability (run with --allow-random)".into());
        }
        let mut buf = vec![0u8; n];
        rand::rngs::ThreadRng::default().fill_bytes(&mut buf);
        Ok(buf)
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
    fn request_serve(&mut self, port: u16, cb_id: u32) -> Result<(), String> {
        if !self.caps.iter().any(|c| c == "net") {
            return Err("no `net` capability (run with --allow-net, or use `mersey serve`)".into());
        }
        // Last writer wins; a program is expected to call serve once.
        self.pending_server = Some((port, cb_id));
        Ok(())
    }
    fn take_pending_server(&mut self) -> Option<(u16, u32)> {
        self.pending_server.take()
    }
    fn time_ms(&mut self, epoch: bool) -> f64 {
        use std::time::{Instant, SystemTime, UNIX_EPOCH};
        if epoch {
            return SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
        }
        // A monotonic clock needs a *fixed* origin. `Instant::now().elapsed()`
        // asks how long since the instant created on that very line, which is
        // always about zero — so `monotonic()` measured nothing at all, and two
        // readings could even come out in the wrong order.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let origin = ORIGIN.get_or_init(Instant::now);
        origin.elapsed().as_secs_f64() * 1000.0
    }
}

/// Load the whole module graph from disk (spec §4.5: closed before
/// execution). Returns modules in dependency-first order.
fn load_graph(entry: &str) -> Result<Graph, ExitCode> {
    use mersey_front::HashMap;
    // Module specifiers are '/'-separated (that is what `graph::resolve` splits
    // on to resolve a relative import against its referrer). A filesystem entry
    // path on Windows uses '\', which would make that split land on the wrong
    // separator; normalize it here (std::fs accepts '/' on Windows).
    let entry = &entry.replace('\\', "/");
    let mut sources: HashMap<String, &'static mersey_front::ast::Module> = HashMap::default();
    let mut deps: HashMap<String, Vec<String>> = HashMap::default();
    let mut dyn_deps: HashMap<String, Vec<String>> = HashMap::default();
    let mut queue = vec![entry.to_string()];
    let mut failed = false;

    while let Some(spec) = queue.pop() {
        if sources.contains_key(&spec) {
            continue;
        }
        // `std:` modules written in Mersey are embedded, not read from disk.
        // A remote dependency is read from the local cache — running code has
        // no authority to reach the network (§5.4), so if it was never fetched
        // this is an error, not a download.
        let bytes = match mersey_front::stdlib::source(&spec) {
            Some(text) => text.as_bytes().to_vec(),
            None if graph::is_remote(&spec) => match std::fs::read(cache_path(&spec)) {
                Ok(b) => b,
                Err(_) => {
                    eprintln!(
                        "mersey: `{spec}` is not in the local cache — run `mersey fetch {entry}`"
                    );
                    return Err(ExitCode::FAILURE);
                }
            },
            None => read(&spec)?,
        };
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
            if graph::is_module(&spec_import) {
                let target = graph::resolve_module(&spec, &spec_import);
                edges.push(target.clone());
                queue.push(target);
            }
        }
        // A dynamic `import("./x")` is part of the graph too — loaded, checked
        // and locked with everything else (§4.5) — it just does not *run* until
        // someone imports it.
        //
        // So it *is* an ordering edge for checking (the importer's `import(…)`
        // is typed as a promise of that module's exports, which have to be known
        // first) but *not* for execution (nothing waits for it to run).
        let mut dyn_edges = Vec::new();
        for spec_import in graph::dynamic_imports(module) {
            if graph::is_module(&spec_import) {
                let target = graph::resolve_module(&spec, &spec_import);
                dyn_edges.push(target.clone());
                queue.push(target);
            }
        }
        deps.insert(spec.clone(), edges);
        dyn_deps.insert(spec.clone(), dyn_edges);
        sources.insert(spec, module);
    }
    if failed {
        return Err(ExitCode::FAILURE);
    }
    // Checking order: dependency-first over *both* kinds of edge, so a module's
    // exports are known before anything that imports it — statically or not.
    let mut all_deps = deps.clone();
    for (spec, dyns) in &dyn_deps {
        all_deps
            .entry(spec.clone())
            .or_default()
            .extend(dyns.iter().cloned());
    }
    let check_order = match graph::topo_order(entry, &all_deps) {
        Ok(o) => o,
        Err(d) => {
            eprintln!("{d}");
            return Err(ExitCode::FAILURE);
        }
    };
    // Execution order: static edges only. Everything else is lazy.
    let exec_order = match graph::topo_order(entry, &deps) {
        Ok(o) => o,
        Err(d) => {
            eprintln!("{d}");
            return Err(ExitCode::FAILURE);
        }
    };

    Ok(Graph {
        all: check_order
            .iter()
            .map(|s| (s.clone(), sources[s]))
            .collect(),
        eager: exec_order,
    })
}

/// A loaded module graph.
struct Graph {
    /// Every module, dependency-first — the order to *check* in.
    all: Vec<(String, &'static mersey_front::ast::Module)>,
    /// The specs that run at startup, in execution order. Anything in `all` but
    /// not here is the target of a dynamic import: loaded and checked, but not
    /// run until someone imports it.
    eager: Vec<String>,
}

impl Graph {
    /// (modules to run now, modules to register as lazy).
    #[allow(clippy::type_complexity)]
    fn split(
        &self,
    ) -> (
        Vec<(String, &'static mersey_front::ast::Module)>,
        Vec<(String, &'static mersey_front::ast::Module)>,
    ) {
        let eager: Vec<_> = self
            .eager
            .iter()
            .filter_map(|s| self.all.iter().find(|(spec, _)| spec == s).cloned())
            .collect();
        let lazy: Vec<_> = self
            .all
            .iter()
            .filter(|(spec, _)| !self.eager.contains(spec))
            .cloned()
            .collect();
        (eager, lazy)
    }
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
    let graph_modules = match load_graph(path) {
        Ok(g) => g,
        Err(code) => return code,
    };
    if !check_graph_ok(&graph_modules.all) {
        return ExitCode::FAILURE;
    }
    let host = CliHost {
        dom: std::collections::HashMap::new(),
        caps,
        pending_server: None,
    };
    let mut interp = interp::new_interp(Box::new(host));
    // Tier 1: register the Cranelift backend unless disabled (benchmarks
    // compare tiers via MERSEY_JIT=0).
    if std::env::var("MERSEY_JIT").as_deref() != Ok("0") {
        interp.jit = Some(mersey_jit::hook);
    }
    // A dynamic-import target is in the graph, but does not run until someone
    // imports it.
    let (eager, lazy) = graph_modules.split();
    for (spec, module) in lazy {
        interp.register_lazy(spec, module);
    }
    match interp.run_graph(eager) {
        // Top-level called `net.serve`: hand off to the accept loop, which
        // blocks re-entering the engine per request until killed.
        Ok(()) => {
            if let Some((port, cb_id)) = interp.take_pending_server() {
                return serve_loop(&mut interp, port, cb_id);
            }
            if interp.graph_is_waiting() {
                eprintln!(
                    "mersey: a module is still waiting on a top-level `await`, and nothing \
                     in this host can settle it"
                );
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(t) => {
            eprintln!("runtime error: {}", interp.describe_thrown(&t));
            ExitCode::FAILURE
        }
    }
}

/// `serve [-jN]`: with one job, run the program inline (its top-level
/// `net.serve` is driven by `run`'s accept loop). With N>1, launch N copies of
/// ourselves as `mersey run` workers that share the listening port via
/// SO_REUSEPORT — process-level parallelism, since the engine is single-thread.
fn serve_cmd(path: &str, caps: Vec<String>, jobs: usize) -> ExitCode {
    if jobs <= 1 {
        return run(path, caps);
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("mersey: cannot find own executable to spawn workers: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Reconstruct a single-worker `run` invocation with the same caps + file.
    let mut argv: Vec<String> = vec!["run".into()];
    for c in &caps {
        argv.push(format!("--allow-{c}"));
    }
    argv.push(path.to_string());

    let mut children = Vec::new();
    for i in 0..jobs {
        match std::process::Command::new(&exe).args(&argv).spawn() {
            Ok(child) => children.push(child),
            Err(e) => {
                eprintln!("mersey: failed to spawn worker {i}: {e}");
                for mut c in children {
                    let _ = c.kill();
                }
                return ExitCode::FAILURE;
            }
        }
    }
    eprintln!("mersey: serving with {jobs} worker processes (SO_REUSEPORT)");
    // Block until a worker exits (they normally serve forever); then tear down.
    for mut c in children {
        let _ = c.wait();
    }
    ExitCode::SUCCESS
}

/// The blocking HTTP/1.1 accept loop for one process. Binds the port with
/// SO_REUSEPORT so sibling workers can share it, then serves one request per
/// connection (Connection: close), re-entering the engine via `http_dispatch`.
fn serve_loop(interp: &mut interp::Interp, port: u16, cb_id: u32) -> ExitCode {
    use socket2::{Domain, Protocol, Socket, Type};
    use std::net::{SocketAddr, TcpListener};

    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener: TcpListener = {
        let sock = match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mersey: socket: {e}");
                return ExitCode::FAILURE;
            }
        };
        let _ = sock.set_reuse_address(true);
        // SO_REUSEPORT: the kernel load-balances accepts across worker processes
        // bound to the same port (Linux 3.9+; present on macOS/BSD too). Windows
        // has no SO_REUSEPORT (and socket2 gates the method off unix), so the
        // worker-pool `serve -jN` path is Unix-only; set_reuse_address above is
        // all Windows offers.
        #[cfg(unix)]
        let _ = sock.set_reuse_port(true);
        if let Err(e) = sock.bind(&addr.into()) {
            eprintln!("mersey: cannot bind :{port}: {e}");
            return ExitCode::FAILURE;
        }
        if let Err(e) = sock.listen(1024) {
            eprintln!("mersey: listen: {e}");
            return ExitCode::FAILURE;
        }
        sock.into()
    };

    for conn in listener.incoming() {
        let mut stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        // None: malformed / closed early — drop the connection.
        if let Some((method, path, body)) = read_http_request(&mut stream) {
            match interp.http_dispatch(cb_id, &method, &path, &body) {
                Ok(resp) => {
                    use std::io::Write;
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
                Err(t) => {
                    use std::io::Write;
                    eprintln!("mersey: handler error: {}", interp.describe_thrown(&t));
                    let _ = stream.write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Minimal HTTP/1.1 request read: the request line (method, path) and the body
/// (per Content-Length). Enough for the benchmark endpoints; the ergonomic
/// header/routing layer is Mersey's `std:http`. Returns None on a malformed or
/// prematurely closed request.
fn read_http_request(stream: &mut std::net::TcpStream) -> Option<(String, String, String)> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    // Read until the header terminator \r\n\r\n appears.
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 1 << 20 {
            return None; // 1 MiB header cap
        }
    };
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    // Content-Length body, if any.
    let mut content_length = 0usize;
    for line in lines {
        if let Some(v) = line
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        {
            content_length = v.1.trim().parse().unwrap_or(0);
        }
    }
    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Some((method, path, String::from_utf8_lossy(&body).into_owned()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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
    // Leaked before checking: the disassembly must show the conversions the
    // checker decided on, and those belong to the nodes it checked.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    if diags.is_empty() {
        diags = bind::bind(module).diagnostics;
    }
    if diags.is_empty() {
        diags = tycheck::check(module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}: {d}", src.name);
        }
        return ExitCode::FAILURE;
    }
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
        let mersey_front::ast::Item::Import(im) = item else {
            continue;
        };
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
            "std:random" => "  [requires --allow-random]",
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
        Ok(g) => g.all,
        Err(code) => return code,
    };
    let mut lines = vec![format!("# mersey.lock — entry: {entry}")];
    for (spec, _) in &modules {
        // A `std:` module written in Mersey (std:http, std:url, …) has no path:
        // its source is embedded in the toolchain, so hash that. Remote modules
        // come from the cache; everything else is a file on disk.
        let bytes: Vec<u8> = if let Some(text) = mersey_front::stdlib::source(spec) {
            text.as_bytes().to_vec()
        } else {
            let from = if graph::is_remote(spec) {
                cache_path(spec)
            } else {
                spec.into()
            };
            let Ok(bytes) = std::fs::read(&from) else {
                eprintln!("mersey: cannot read {spec}");
                return ExitCode::FAILURE;
            };
            bytes
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
    base64(&sha256(data))
}

/// SHA-256 — used for SRI hashes and to name a cache file after its URL. The
/// implementation lives in `mersey_interp` (also exposed to the language as
/// `std:hash`), so the CLI and the engine share one copy.
use mersey_interp::sha256;

fn base64(digest: &[u8]) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in digest.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---- mersey test --------------------------------------------------------------

/// Run every `*.test.mersey` under `path`.
///
/// A test file is an ordinary module: `std:test`'s `test()` runs a case,
/// catches what it throws, and prints one TAP line per result. So the runner
/// does not need a hook into the engine — it runs the file and reads what it
/// said. `mersey run` on the same file does exactly the same thing, which is
/// what keeps the framework honest: there is no privileged test mode.
fn test_cmd(path: &str) -> ExitCode {
    let mut files = Vec::new();
    collect_tests(std::path::Path::new(path), &mut files);
    files.sort();
    if files.is_empty() {
        eprintln!("mersey: no *.test.mersey files under `{path}`");
        return ExitCode::from(2);
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut broken: Vec<String> = Vec::new();

    for file in &files {
        let name = file.display().to_string();
        let modules = match load_graph(&name) {
            Ok(g) => g,
            Err(_) => {
                broken.push(format!("{name}: does not load"));
                continue;
            }
        };
        if !check_graph_ok(&modules.all) {
            broken.push(format!("{name}: does not typecheck"));
            continue;
        }
        let (eager, lazy) = modules.split();

        let lines = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut interp = interp::new_interp(Box::new(TapHost {
            lines: lines.clone(),
        }));
        if std::env::var("MERSEY_JIT").as_deref() != Ok("0") {
            interp.jit = Some(mersey_jit::hook);
        }
        for (spec, module) in lazy {
            interp.register_lazy(spec, module);
        }
        let outcome = interp.run_graph(eager);

        // The host kept what the module printed.
        let lines = lines.borrow().clone();
        println!("# {name}");
        for line in &lines {
            println!("{line}");
            if line.starts_with("ok -") {
                passed += 1;
            } else if line.starts_with("not ok -") {
                failed += 1;
            }
        }
        if let Err(t) = outcome {
            // A throw that escaped every test case: the file itself is broken.
            broken.push(format!("{name}: {}", interp.describe_thrown(&t)));
        }
    }

    println!();
    println!("{passed} passed, {failed} failed");
    for b in &broken {
        println!("error: {b}");
    }
    if failed == 0 && broken.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn collect_tests(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // Skip the places that are never source.
            let skip = matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("target") | Some("node_modules") | Some(".git")
            );
            if !skip {
                collect_tests(&p, out);
            }
        } else if p.to_str().is_some_and(|s| s.ends_with(".test.mersey")) {
            out.push(p);
        }
    }
}

/// A host that keeps what the module printed, so the runner can read the TAP
/// lines back.
struct TapHost {
    lines: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl interp::Host for TapHost {
    fn print(&mut self, s: &str) {
        self.lines.borrow_mut().push(s.to_string());
    }
    fn dom_set_text(&mut self, _: &str, _: &str) {}
    fn dom_get_text(&mut self, _: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _: &str, _: &str, _: u32) {}
}

// ---- remote dependencies ------------------------------------------------------

/// Where a remote module is cached. The URL is hashed rather than mapped onto
/// the filesystem: a URL can say `..`, contain a drive letter, or be long
/// enough to break a path — none of which should be able to decide where a file
/// lands on disk.
fn cache_path(url: &str) -> std::path::PathBuf {
    let digest = sha256_hex(url.as_bytes());
    std::path::Path::new(".mersey")
        .join("cache")
        .join(format!("{digest}.mersey"))
}

/// Download every remote import reachable from `entry`, transitively.
///
/// This is the *only* place Mersey talks to the network, and it is a
/// deliberate, separate step: `mersey run` reads the cache and nothing else. A
/// hash already pinned in mersey.lock is enforced here — if a URL now serves
/// different bytes than the ones the project locked, the fetch fails rather
/// than silently updating, which is the whole point of pinning.
fn fetch_cmd(entry: &str) -> ExitCode {
    let pinned = read_lock();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue = vec![entry.to_string()];
    let mut fetched = 0usize;
    let mut new_pins: Vec<(String, String)> = Vec::new();

    while let Some(spec) = queue.pop() {
        if !seen.insert(spec.clone()) {
            continue;
        }
        let bytes = if graph::is_remote(&spec) {
            let path = cache_path(&spec);
            let cached = std::fs::read(&path).ok();
            match cached {
                Some(b) => b,
                None => {
                    println!("fetching {spec}");
                    let b = match http_get(&spec) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("mersey: cannot fetch {spec}: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    let hash = format!("sha256-{}", sha256_base64(&b));
                    // Supply chain: a URL that changed under a pinned hash is
                    // refused outright.
                    if let Some(want) = pinned.get(&spec) {
                        if want != &hash {
                            eprintln!(
                                "mersey: {spec} does not match mersey.lock\n  locked: {want}\n  served: {hash}"
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                    if let Some(dir) = path.parent() {
                        if let Err(e) = std::fs::create_dir_all(dir) {
                            eprintln!("mersey: cannot create {}: {e}", dir.display());
                            return ExitCode::FAILURE;
                        }
                    }
                    if let Err(e) = std::fs::write(&path, &b) {
                        eprintln!("mersey: cannot write {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                    new_pins.push((spec.clone(), hash));
                    fetched += 1;
                    b
                }
            }
        } else if let Some(text) = mersey_front::stdlib::source(&spec) {
            text.as_bytes().to_vec()
        } else {
            match std::fs::read(&spec) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("mersey: cannot read {spec}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        };

        // Follow this module's own imports — a package brings its own files.
        let Ok(src) = source::decode(&spec, &bytes) else {
            eprintln!("mersey: {spec} is not valid UTF-8");
            return ExitCode::FAILURE;
        };
        let parsed = parser::parse(&src);
        if !parsed.diagnostics.is_empty() {
            for d in &parsed.diagnostics {
                eprintln!("{spec}: {d}");
            }
            return ExitCode::FAILURE;
        }
        for import in graph::imports(&parsed.module) {
            if graph::is_module(&import) {
                queue.push(graph::resolve_module(&spec, &import));
            }
        }
    }

    if fetched == 0 {
        println!("up to date ({} modules, nothing to fetch)", seen.len());
    } else {
        println!("fetched {fetched} module(s) into .mersey/cache");
    }
    for (spec, hash) in &new_pins {
        if !pinned.contains_key(spec) {
            println!("  {spec}  {hash}");
        }
    }
    if !new_pins.is_empty() {
        println!("run `mersey lock {entry}` to pin these in mersey.lock");
    }
    ExitCode::SUCCESS
}

/// The hashes mersey.lock already pins, by specifier.
fn read_lock() -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string("mersey.lock") else {
        return out;
    };
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some((spec, hash)) = line.split_once("  ") {
            out.insert(spec.trim().to_string(), hash.trim().to_string());
        }
    }
    out
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    // A dependency should be a file, not a stream: cap it rather than let a
    // hostile host stream forever.
    const MAX: u64 = 8 << 20;
    let mut resp = ureq::get(url).call().map_err(|e| e.to_string())?;
    if resp.status() != 200 {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut buf = Vec::new();
    let mut reader = std::io::Read::take(resp.body_mut().as_reader(), MAX);
    std::io::Read::read_to_end(&mut reader, &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut out = String::new();
    for b in sha256(data) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
