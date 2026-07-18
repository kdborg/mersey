//! Mersey → JavaScript.
//!
//! The polyfill's second execution mode: instead of interpreting Mersey inside
//! a WASM build of the engine (correct everywhere, but an interpreter running
//! where V8/SpiderMonkey sit idle), emit JavaScript once and let the browser's
//! own JIT run it. The front end — lexer, parser, binder, checker — is the
//! same one the engine uses, so diagnostics and types are identical by
//! construction; only this backend is new, and the runtime conformance goldens
//! gate it against the engine's behaviour.
//!
//! What carries the semantics across:
//! - **Numbers.** The checker's own tables say what every arithmetic node is
//!   (`check::op_type_for`), so `int32` math wraps (`|0`, `Math.imul`),
//!   integer division truncates and traps on zero, and `int64`/`uint64`
//!   become BigInt — where `/` already truncates.
//! - **Strings.** Engine strings are WTF-16 with JS code-unit semantics — a
//!   Mersey string *is* a JS string, nothing to shim. `char` is a one-code-
//!   point JS string.
//! - **Display.** `console.log` and templates format through `$rt.D`, which
//!   reproduces the engine's `to_display` byte for byte (goldens check it).
//! - **Coercions.** The checker's C-style widenings (`check::coercion_of`)
//!   are applied at the recorded expression, exactly as the engine does.
//! - **Methods.** A call `x.m(...)` goes through `$rt.call`, which dispatches
//!   Mersey's built-in methods on strings/arrays/maps first and falls through
//!   to the object's own method — so Mersey's stdlib surface wins over the JS
//!   native where they differ, and class methods cost one map miss.

use mersey_front::ast::{self, *};
use mersey_front::check::{self, DefaultVal, IntKind, Num};
use mersey_front::parser;
use mersey_front::source::SourceFile;

pub mod wasmgen;

fn b64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn sig_tag(n: mersey_front::check::Num) -> &'static str {
    use mersey_front::check::{IntKind, Num};
    match n {
        Num::Int(IntKind::U64) => "u64",
        Num::Int(IntKind::I64) => "i64",
        Num::Int(IntKind::U32) => "u32",
        Num::Int(_) => "i32",
        Num::F32 => "f32",
        Num::F64 => "f64",
    }
}

/// The JS runtime prelude every transpiled module carries.
pub const RUNTIME: &str = include_str!("rt.js");

pub struct Output {
    pub js: String,
    /// An inline `//# sourceMappingURL=data:…` line mapping emitted
    /// statements back to the `.mersey` source (with sourcesContent).
    pub map: String,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One self-contained module: runtime prepended, body wrapped in
    /// $rt.main so an uncaught error prints the engine's line.
    Single,
    /// One module of a graph: a real ES module — `import`/`export` statements
    /// with `mersey-mod:<spec>` placeholder specifiers the loader rewrites to
    /// blob URLs; $rt is imported from the shared runtime module.
    Module,
}

pub struct GraphOutput {
    /// (spec, js) in the order given (dependency-first from the loader).
    pub modules: Vec<(String, String)>,
    pub diagnostics: Vec<String>,
}

/// Transpile a whole module graph (dependency-first, entry last), checked as
/// one program so cross-module types resolve — exactly as the engine checks it.
pub fn transpile_graph(mods: &[(String, String)]) -> GraphOutput {
    let mut parsed_mods: Vec<(String, &'static Module)> = Vec::new();
    let mut diagnostics = Vec::new();
    // The `std:` modules written in Mersey are part of every graph — the
    // engine embeds them, and so does the JS backend: they are checked AND
    // emitted, so `import { ok } from "std:result"` links like any module.
    let mut all: Vec<(String, String)> = Vec::new();
    for spec in mersey_front::stdlib::source_modules() {
        if let Some(text) = mersey_front::stdlib::source(spec) {
            all.push((spec.to_string(), text.to_string()));
        }
    }
    all.extend(mods.iter().cloned());
    let mods = &all;
    for (spec, source) in mods {
        let sf = SourceFile {
            name: spec.clone(),
            text: source.clone(),
        };
        let parsed = parser::parse(&sf);
        for d in &parsed.diagnostics {
            diagnostics.push(format!("{spec}: {d}"));
        }
        parsed_mods.push((spec.clone(), Box::leak(Box::new(parsed.module))));
    }
    if !diagnostics.is_empty() {
        return GraphOutput {
            modules: Vec::new(),
            diagnostics,
        };
    }
    for (spec, out) in check::check_graph(&parsed_mods) {
        for d in &out.diagnostics {
            diagnostics.push(format!("{spec}: {d}"));
        }
    }
    if !diagnostics.is_empty() {
        return GraphOutput {
            modules: Vec::new(),
            diagnostics,
        };
    }
    let modules = parsed_mods
        .iter()
        .map(|(spec, module)| {
            let mut e = Emit {
                out: String::new(),
                line: 1,
                maps: Vec::new(),
                indent: 0,
                mode: Mode::Module,
                spec: spec.clone(),
                wasm_fns: std::collections::HashSet::new(),
            };
            e.module(module);
            // NOTE: no inline map here yet — the loader's error-frame tests
            // parse raw stacks of these blob modules and the appended map
            // shifted what they see (two REAL BROWSER failures). The
            // single-file `transpile` path carries maps; wiring the graph
            // path needs that interaction understood first. Tracked.
            (spec.clone(), e.out)
        })
        .collect();
    GraphOutput {
        modules,
        diagnostics: Vec::new(),
    }
}

/// Transpile one self-contained module. Diagnostics are the checker's own —
/// a program that does not check does not transpile, same as the engine.
pub fn transpile(source: &str, name: &str, include_runtime: bool) -> Output {
    let sf = SourceFile {
        name: name.to_string(),
        text: source.to_string(),
    };
    let parsed = parser::parse(&sf);
    if !parsed.diagnostics.is_empty() {
        return Output {
            js: String::new(),
            map: String::new(),
            diagnostics: parsed.diagnostics.iter().map(|d| d.to_string()).collect(),
        };
    }
    let module: &'static Module = Box::leak(Box::new(parsed.module));
    let out = check::check(module);
    if !out.diagnostics.is_empty() {
        return Output {
            js: String::new(),
            map: String::new(),
            diagnostics: out.diagnostics.iter().map(|d| d.to_string()).collect(),
        };
    }
    let mut e = Emit {
        out: String::new(),
        line: 1,
        maps: Vec::new(),
        indent: 0,
        mode: Mode::Single,
        spec: name.to_string(),
        wasm_fns: std::collections::HashSet::new(),
    };
    e.module(module);
    let mut js = String::new();
    if include_runtime {
        js.push_str(RUNTIME);
        js.push('\n');
    }
    // The prelude shifts the module's lines down; the map accounts for it.
    let offset = js.matches('\n').count() as u32;
    js.push_str(&e.out);
    // Not appended to `js`: the loader's error-frame parsing reads these
    // modules raw (appending regressed two REAL BROWSER tests). Callers that
    // want DevTools mapping (`mersey js --map`) concatenate it themselves.
    let map = map_url(name, source, offset, &e.maps);
    Output {
        js,
        map,
        diagnostics: Vec::new(),
    }
}

/// An inline `sourceMappingURL` for the emitted module: DevTools (and any
/// stack mapper) resolves generated lines back to the `.mersey` source —
/// which the map carries in `sourcesContent`, so no server round-trip.
fn map_url(name: &str, source: &str, gen_line_offset: u32, maps: &[(u32, u32, u32, u32)]) -> String {
    use mersey_front::sourcemap::{encode, Mapping};
    let mappings: Vec<Mapping> = maps
        .iter()
        .map(|(gl, gc, sl, sc)| Mapping {
            gen_line: gl + gen_line_offset,
            gen_col: *gc,
            src_line: *sl,
            src_col: *sc,
        })
        .collect();
    let map = encode(name, source, mappings);
    format!(
        "\n//# sourceMappingURL=data:application/json;base64,{}\n",
        b64(map.as_bytes())
    )
}

struct Emit {
    out: String,
    indent: usize,
    mode: Mode,
    spec: String,
    /// Current 1-based line of `out`, and (gen_line, gen_col, src_line,
    /// src_col) recorded at each statement — the source map's raw material.
    line: u32,
    maps: Vec<(u32, u32, u32, u32)>,
    /// Top-level functions the WASM tier compiled: bound from $w, not emitted.
    wasm_fns: std::collections::HashSet<String>,
}

impl Emit {
    fn nl(&mut self) {
        self.out.push('\n');
        self.line += 1;
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
    }
    fn w(&mut self, s: &str) {
        self.line += s.matches('\n').count() as u32;
        self.out.push_str(s);
    }

    /// Compile the numeric functions to WASM and emit the instantiation +
    /// bindings. Returns true if a tier was emitted.
    fn wasm_tier(&mut self, m: &Module) {
        // A/B switch for measurement (CLI only; the browser build has no env).
        if std::env::var_os("MERSEY_JS_NO_WASM").is_some() {
            return;
        }
        let fns: Vec<&FnDecl> = m
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Decl(Decl::Function(f)) => Some(f),
                Item::Export(e) => match &e.kind {
                    ExportKind::Decl(Decl::Function(f)) => Some(f),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        // Names bound to std:math (aliases included): calls on them are
        // intrinsic candidates.
        let mut math_names = std::collections::HashSet::new();
        for it in &m.items {
            if let Item::Import(im) = it {
                if im.from == "std:math" {
                    if let Some(ImportClause::Named(list)) = &im.clause {
                        for na in list {
                            math_names
                                .insert(na.alias.as_ref().unwrap_or(&na.name).text.clone());
                        }
                    }
                }
            }
        }
        let Some(tier) = wasmgen::compile_with(&fns, &math_names) else {
            return;
        };
        self.nl();
        self.w("const $w = await $rt.wasm(\"");
        self.w(&b64(&tier.bytes));
        self.w("\", [");
        for (i, ex) in tier.exports.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.w(&format!(
                "[\"{}\",[{}],{}]",
                ex.name,
                ex.params
                    .iter()
                    .map(|k| format!("\"{}\"", sig_tag(*k)))
                    .collect::<Vec<_>>()
                    .join(","),
                match ex.ret {
                    Some(k) => format!("\"{}\"", sig_tag(k)),
                    None => "null".to_string(),
                }
            ));
            self.wasm_fns.insert(ex.name.clone());
        }
        self.w("]);");
    }

    fn module(&mut self, m: &Module) {
        if self.mode == Mode::Module {
            // A real ES module: $rt from the shared runtime module, imports
            // and exports at top level, statements run at import time.
            self.w("import { $rt } from \"mersey-mod:%rt%\";");
            self.wasm_tier(m);
            for item in &m.items {
                self.nl();
                self.item(item);
            }
            self.w("\n");
            return;
        }
        // Single module: the body runs inside one try, so a thrown error
        // prints the engine's own "runtime error:" line (the conformance
        // harness compares stdout+stderr and the exit code).
        self.w("$rt.main(async () => {");
        self.indent += 1;
        self.wasm_tier(m);
        for item in &m.items {
            self.nl();
            self.item(item);
        }
        self.indent -= 1;
        self.nl();
        self.w("});\n");
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Import(im) => self.import(im),
            Item::Decl(decl) => self.decl(decl),
            Item::Export(e) => match &e.kind {
                ExportKind::Decl(d) => {
                    if self.mode == Mode::Module && !matches!(d, Decl::Interface(_) | Decl::TypeAlias(_)) {
                        self.w("export ");
                    }
                    self.decl(d);
                }
                ExportKind::Var(v) => {
                    if self.mode == Mode::Module {
                        self.w("export ");
                    }
                    self.var(v);
                    self.w(";");
                }
                ExportKind::Named { specs, from } => {
                    if self.mode == Mode::Module && from.is_none() {
                        self.w("export { ");
                        for (i, na) in specs.iter().enumerate() {
                            if i > 0 {
                                self.w(", ");
                            }
                            self.w(&na.name.text);
                            if let Some(a) = &na.alias {
                                self.w(" as ");
                                self.w(&a.text);
                            }
                        }
                        self.w(" };");
                    }
                }
            },
            Item::Stmt(s) => self.stmt(s),
        }
    }

    fn import(&mut self, im: &ImportDecl) {
        // std:* modules live in the runtime; browser:dom names bind to the
        // real globals. Relative imports need the module-graph loader — the
        // single-module runner reports them exactly as the engine's does.
        let spec = &im.from;
        let names: Vec<(String, String)> = match &im.clause {
            Some(ImportClause::Named(list)) => list
                .iter()
                .map(|na| {
                    let bound = na.alias.as_ref().unwrap_or(&na.name).text.clone();
                    (na.name.text.clone(), bound)
                })
                .collect(),
            Some(ImportClause::Namespace(n)) => vec![("*".to_string(), n.text.clone())],
            None => vec![],
        };
        if self.mode == Mode::Module
            && spec.starts_with("std:")
            && mersey_front::stdlib::source(spec).is_some()
        {
            // A std module written in Mersey: a real import; its transpiled
            // JS is in the graph under its own spec.
            self.w("import { ");
            for (i, (name, bound)) in names.iter().enumerate() {
                if i > 0 {
                    self.w(", ");
                }
                self.w(name);
                if bound != name {
                    self.w(" as ");
                    self.w(bound);
                }
            }
            self.w(&format!(" }} from \"mersey-mod:{spec}\";"));
            self.nl();
        } else if let Some(std_name) = spec.strip_prefix("std:") {
            for (name, bound) in &names {
                if name == "*" {
                    self.w(&format!("const {bound} = $rt.std[\"{std_name}\"];"));
                } else {
                    self.w(&format!(
                        "const {bound} = $rt.std[\"{std_name}\"].{name};"
                    ));
                }
                self.nl();
            }
        } else if spec == "browser:dom" {
            for (name, bound) in &names {
                self.w(&format!(
                    "const {bound} = $rt.web(\"{name}\");"
                ));
                self.nl();
            }
        } else if self.mode == Mode::Module {
            // A relative import: a real ES import with a placeholder specifier
            // the loader rewrites to the module's blob URL. The spec resolves
            // against this module's own spec, as the engine's loader does.
            let resolved = mersey_front::graph::resolve_module(&self.spec, spec);
            match &im.clause {
                Some(ImportClause::Named(list)) => {
                    self.w("import { ");
                    for (i, na) in list.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.w(&na.name.text);
                        if let Some(a) = &na.alias {
                            self.w(" as ");
                            self.w(&a.text);
                        }
                    }
                    self.w(&format!(" }} from \"mersey-mod:{resolved}\";"));
                }
                Some(ImportClause::Namespace(n)) => {
                    self.w(&format!(
                        "import * as {} from \"mersey-mod:{resolved}\";",
                        n.text
                    ));
                }
                None => self.w(&format!("import \"mersey-mod:{resolved}\";")),
            }
            self.nl();
        } else {
            self.w(&format!(
                "throw new TypeError(\"module `{spec}` was not loaded (resolved to `{spec}`)\");"
            ));
            self.nl();
        }
    }

    fn decl(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => self.fn_decl(f),
            Decl::Class(c) => self.class_decl(c),
            Decl::Enum(e) => self.enum_decl(e),
            Decl::Interface(_) | Decl::TypeAlias(_) => {} // types erase
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        if self.wasm_fns.contains(&f.name.text) {
            // Compiled by the WASM tier; bind the export in its place.
            self.w(&format!("const {0} = $w.{0};", f.name.text));
            return;
        }
        let is_gen = body_has_yield(&f.body);
        self.w(match (f.is_async, is_gen) {
            (true, true) => "async function* ",
            (true, false) => "async function ",
            (false, true) => "function* ",
            (false, false) => "function ",
        });
        self.w(&f.name.text);
        self.params(&f.params);
        self.block(&f.body);
    }

    fn params(&mut self, params: &[Param]) {
        self.w("(");
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            if p.rest {
                self.w("...");
            }
            self.pattern(&p.target);
            if let Some(d) = &p.default {
                self.w(" = ");
                self.expr(d);
            } else if p.optional {
                self.w(" = null");
            }
        }
        self.w(")");
    }

    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Name(n) => self.w(&n.text),
            Pattern::Array { elems, rest } => {
                self.w("[");
                for (i, el) in elems.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.pattern(&el.target);
                    if let Some(d) = &el.default {
                        self.w(" = ");
                        self.expr(d);
                    }
                }
                if let Some(r) = rest {
                    if !elems.is_empty() {
                        self.w(", ");
                    }
                    self.w("...");
                    self.pattern(r);
                }
                self.w("]");
            }
            Pattern::Record(fields) => {
                self.w("{");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.w(&f.name.text);
                    if let Some(t) = &f.target {
                        self.w(": ");
                        self.pattern(t);
                    }
                    if let Some(d) = &f.default {
                        self.w(" = ");
                        self.expr(d);
                    }
                }
                self.w("}");
            }
        }
    }

    fn class_decl(&mut self, c: &ClassDecl) {
        self.w("class ");
        self.w(&c.name.text);
        if let Some(base) = &c.extends {
            self.w(" extends ");
            self.w(&type_head(base));
        }
        self.w(" {");
        self.indent += 1;

        for m in &c.members {
            match m {
                ClassMember::Field {
                    mods,
                    name,
                    ty,
                    init,
                    ..
                } => {
                    self.nl();
                    if mods.is_static {
                        self.w("static ");
                    }
                    self.w(name);
                    self.w(" = ");
                    match init {
                        Some(e) => self.expr(e),
                        None => self.default_for(ty),
                    }
                    self.w(";");
                }
                ClassMember::Ctor { params, body, .. } => {
                    self.nl();
                    self.w("constructor");
                    self.params(params);
                    self.block(body);
                }
                ClassMember::Method {
                    mods,
                    is_async,
                    name,
                    params,
                    body,
                    ..
                } => {
                    let Some(body) = body else { continue }; // abstract
                    self.nl();
                    if mods.is_static {
                        self.w("static ");
                    }
                    if *is_async {
                        self.w("async ");
                    }
                    if body_has_yield(body) {
                        self.w("*");
                    }
                    self.w(name);
                    self.params(params);
                    self.block(body);
                }
                ClassMember::Getter { mods, name, body, .. } => {
                    self.nl();
                    if mods.is_static {
                        self.w("static ");
                    }
                    self.w("get ");
                    self.w(name);
                    self.w("()");
                    self.block(body);
                }
                ClassMember::Setter {
                    mods, name, param, body, ..
                } => {
                    self.nl();
                    if mods.is_static {
                        self.w("static ");
                    }
                    self.w("set ");
                    self.w(name);
                    self.w("(");
                    self.pattern(&param.target);
                    self.w(")");
                    self.block(body);
                }
            }
        }
        self.indent -= 1;
        self.nl();
        self.w("}");
        self.nl();
        self.w(&format!("$rt.classes.set(\"{0}\", {0});", c.name.text));
    }

    fn enum_decl(&mut self, e: &EnumDecl) {
        self.w(&format!("const {} = Object.freeze({{", e.name.text));
        let mut next = 0i64;
        for (i, (name, init)) in e.members.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.w(&name.text);
            self.w(": ");
            match init {
                Some(x) => {
                    self.expr(x);
                    if let Expr::Lit {
                        kind: LitKind::Int,
                        text,
                        ..
                    } = x
                    {
                        if let Ok(n) = text.replace('_', "").parse::<i64>() {
                            next = n + 1;
                        }
                    }
                }
                None => {
                    self.w(&next.to_string());
                    next += 1;
                }
            }
        }
        self.w("});");
    }

    fn default_for(&mut self, ty: &TypeExpr) {
        match check::default_for_ty(ty) {
            Some(DefaultVal::Num(Num::Int(IntKind::I64 | IntKind::U64))) => self.w("0n"),
            Some(DefaultVal::Num(_)) => self.w("0"),
            Some(other) => self.default_val(other),
            None => self.w("null"),
        }
    }

    fn default_val(&mut self, d: DefaultVal) {
        // Non-numeric defaults; the numeric cases are handled by the caller.
        let s = match d {
            DefaultVal::Num(_) => "0",
            DefaultVal::BigInt => "0n",
            DefaultVal::BigDec => "$rt.bigdec(\"0\")",
            DefaultVal::Str => "\"\"",
            DefaultVal::Char => "\"\\0\"",
            DefaultVal::Bool => "false",
            DefaultVal::Array => "[]",
            DefaultVal::Map => "new Map()",
            DefaultVal::Set => "new Set()",
            DefaultVal::Bytes => "new Uint8Array(0)",
        };
        self.w(s);
    }

    /// The default-descriptor for a pattern: array positions / record fields
    /// whose binding has a default, as thunks (evaluated only when the value
    /// is null). Positions without defaults are null.
    fn default_thunks(&mut self, p: &Pattern) {
        match p {
            Pattern::Name(_) => self.w("null"),
            Pattern::Array { elems, .. } => {
                self.w("[");
                for (i, el) in elems.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    match &el.default {
                        Some(d) => {
                            self.w("() => ");
                            self.expr(d);
                        }
                        None => self.w("null"),
                    }
                }
                self.w("]");
            }
            Pattern::Record(fields) => {
                self.w("{");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.w(&f.name.text);
                    self.w(": ");
                    match &f.default {
                        Some(d) => {
                            self.w("() => ");
                            self.expr(d);
                        }
                        None => self.w("null"),
                    }
                }
                self.w("}");
            }
        }
    }

    fn block(&mut self, body: &[Stmt]) {
        self.w(" {");
        self.indent += 1;
        for s in body {
            self.nl();
            self.stmt(s);
        }
        self.indent -= 1;
        self.nl();
        self.w("}");
    }

    fn stmt(&mut self, s: &Stmt) {
        if let Some(p) = mersey_front::ast::stmt_first_pos(s) {
            self.maps
                .push((self.line, self.indent as u32 * 2 + 1, p.line, p.col));
        }
        match s {
            Stmt::Block(body) => {
                self.w("{");
                self.indent += 1;
                for s in body {
                    self.nl();
                    self.stmt(s);
                }
                self.indent -= 1;
                self.nl();
                self.w("}");
            }
            Stmt::Var(v) => {
                self.var(v);
                self.w(";");
            }
            Stmt::Expr(e) => {
                self.expr(e);
                self.w(";");
            }
            Stmt::Empty => self.w(";"),
            Stmt::If { cond, then, els } => {
                self.w("if (");
                self.expr(cond);
                self.w(") ");
                self.stmt(then);
                if let Some(e) = els {
                    self.w(" else ");
                    self.stmt(e);
                }
            }
            Stmt::While { cond, body } => {
                self.w("while (");
                self.expr(cond);
                self.w(") ");
                self.stmt(body);
            }
            Stmt::DoWhile { body, cond } => {
                self.w("do ");
                self.stmt(body);
                self.w(" while (");
                self.expr(cond);
                self.w(");");
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.w("for (");
                match init {
                    Some(ForInit::Var(v)) => self.var(v),
                    Some(ForInit::Exprs(es)) => {
                        for (i, e) in es.iter().enumerate() {
                            if i > 0 {
                                self.w(", ");
                            }
                            self.expr(e);
                        }
                    }
                    None => {}
                }
                self.w("; ");
                if let Some(c) = cond {
                    self.expr(c);
                }
                self.w("; ");
                for (i, e) in step.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.expr(e);
                }
                self.w(") ");
                self.stmt(body);
            }
            Stmt::ForOf {
                is_await,
                kind,
                target,
                iter,
                body,
                ..
            } => {
                self.w("for ");
                if *is_await {
                    self.w("await ");
                }
                self.w("(");
                self.w(kind.as_str());
                self.w(" ");
                self.pattern(target);
                self.w(" of $rt.iter(");
                self.expr(iter);
                self.w(")) ");
                self.stmt(body);
            }
            Stmt::Switch { scrutinee, clauses } => {
                self.w("switch (");
                self.expr(scrutinee);
                self.w(") {");
                self.indent += 1;
                for c in clauses {
                    self.nl();
                    match &c.test {
                        Some(t) => {
                            self.w("case ");
                            self.expr(t);
                            self.w(": {");
                        }
                        None => self.w("default: {"),
                    }
                    self.indent += 1;
                    for s in &c.body {
                        self.nl();
                        self.stmt(s);
                    }
                    // Mersey switch falls through, C-style: the program writes
                    // its own breaks, and the emitter adds none.
                    self.indent -= 1;
                    self.nl();
                    self.w("}");
                }
                self.indent -= 1;
                self.nl();
                self.w("}");
            }
            Stmt::Break { label, .. } => {
                self.w("break");
                if let Some(l) = label {
                    self.w(" ");
                    self.w(&l.text);
                }
                self.w(";");
            }
            Stmt::Continue { label, .. } => {
                self.w("continue");
                if let Some(l) = label {
                    self.w(" ");
                    self.w(&l.text);
                }
                self.w(";");
            }
            Stmt::Return { value, .. } => {
                self.w("return");
                if let Some(v) = value {
                    self.w(" ");
                    self.expr(v);
                }
                self.w(";");
            }
            Stmt::Throw(e) => {
                self.w("throw ");
                self.expr(e);
                self.w(";");
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                self.w("try");
                self.block(block);
                if !catches.is_empty() {
                    self.w(" catch ($e) {");
                    self.indent += 1;
                    // Typed catch clauses: first matching type wins; no match
                    // rethrows — the engine's dispatch.
                    for (i, c) in catches.iter().enumerate() {
                        self.nl();
                        if i > 0 {
                            self.w("else ");
                        }
                        let ty = type_head(&c.ty);
                        if ty == "Error" || ty == "unknown" {
                            self.w("if (true) {");
                        } else {
                            self.w(&format!("if ($rt.is($e, \"{ty}\")) {{"));
                        }
                        self.indent += 1;
                        self.nl();
                        self.w(&format!("let {} = $e;", c.name.text));
                        for s in &c.block {
                            self.nl();
                            self.stmt(s);
                        }
                        self.indent -= 1;
                        self.nl();
                        self.w("}");
                    }
                    self.nl();
                    self.w("else { throw $e; }");
                    self.indent -= 1;
                    self.nl();
                    self.w("}");
                }
                if let Some(f) = finally {
                    self.w(" finally");
                    self.block(f);
                }
            }
            Stmt::Labeled { label, body } => {
                self.w(&label.text);
                self.w(": ");
                self.stmt(body);
            }
        }
    }

    fn var(&mut self, v: &VarStmt) {
        // `const` in Mersey allows later narrowing assigns in some checker
        // flows? No — const is const. But JS `const` in a `for(const…)` init
        // differs; keep kinds as written.
        self.w(v.kind.as_str());
        self.w(" ");
        for (i, b) in v.bindings.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.pattern(&b.target);
            match (&b.init, &b.ty) {
                (Some(init), _) => {
                    self.w(" = ");
                    // Mersey defaults fire on null (its missing value), JS's
                    // only on undefined — normalize through the runtime when
                    // the pattern carries defaults.
                    if pattern_has_default(&b.target) {
                        self.w("$rt.dflt(");
                        self.expr(init);
                        self.w(", ");
                        self.default_thunks(&b.target);
                        self.w(")");
                    } else {
                        self.expr(init);
                    }
                }
                (None, Some(ty)) => {
                    self.w(" = ");
                    self.default_for(ty);
                }
                (None, None) => self.w(" = null"),
            }
        }
    }

    /// Emit an expression, applying the checker's recorded coercion for this
    /// node (the C-style widening only the checker can see, §3.3).
    fn expr(&mut self, e: &Expr) {
        match check::coercion_for(e) {
            Some(n) => {
                self.conv_open(n);
                self.expr_naked(e);
                self.w(")");
            }
            None => self.expr_naked(e),
        }
    }

    fn conv_open(&mut self, n: Num) {
        match n {
            Num::Int(IntKind::I64) => self.w("$rt.wI64("),
            Num::Int(IntKind::U64) => self.w("$rt.wU64("),
            Num::Int(IntKind::I32) => self.w("$rt.wI32("),
            Num::Int(IntKind::U32) => self.w("$rt.wU32("),
            Num::Int(IntKind::I16) => self.w("$rt.wI16("),
            Num::Int(IntKind::U16) => self.w("$rt.wU16("),
            Num::Int(IntKind::I8) => self.w("$rt.wI8("),
            Num::Int(IntKind::U8) => self.w("$rt.wU8("),
            Num::F32 => self.w("Math.fround("),
            Num::F64 => self.w("$rt.wF64("),
        }
    }

    fn expr_naked(&mut self, e: &Expr) {
        match e {
            Expr::Ident(n) => self.w(&n.text),
            Expr::This(_) => self.w("this"),
            Expr::Lit { kind, text, .. } => self.lit(*kind, text),
            Expr::Template(parts) => {
                self.w("`");
                for p in parts {
                    match p {
                        TplPart::Text(t) => {
                            let t = t
                                .replace('\\', "\\\\")
                                .replace('`', "\\`")
                                .replace("${", "\\${");
                            self.w(&t);
                        }
                        TplPart::Expr(ex) => {
                            self.w("${$rt.D(");
                            self.expr(ex);
                            self.w(")}");
                        }
                    }
                }
                self.w("`");
            }
            Expr::Array(elems) => {
                self.w("[");
                for (i, el) in elems.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    if el.spread {
                        self.w("...$rt.iter(");
                        self.expr(&el.expr);
                        self.w(")");
                    } else {
                        self.expr(&el.expr);
                    }
                }
                self.w("]");
            }
            Expr::Record(fields) => {
                self.w("({");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    match f {
                        RecordField::Named { name, value } => {
                            self.w(&name.text);
                            if let Some(v) = value {
                                self.w(": ");
                                self.expr(v);
                            }
                        }
                        RecordField::Spread(x) => {
                            self.w("...");
                            self.expr(x);
                        }
                    }
                }
                self.w("})");
            }
            Expr::Paren(inner) => {
                self.w("(");
                self.expr(inner);
                self.w(")");
            }
            Expr::Arrow {
                is_async,
                params,
                body,
                ..
            } => {
                if *is_async {
                    self.w("async ");
                }
                self.params(params);
                self.w(" => ");
                match body {
                    ArrowBody::Expr(x) => {
                        self.w("(");
                        self.expr(x);
                        self.w(")");
                    }
                    ArrowBody::Block(b) => self.block(b),
                }
            }
            Expr::Unary { op, expr, .. } => match op {
                UnaryOp::Await => {
                    self.w("(await ");
                    self.expr(expr);
                    self.w(")");
                }
                _ => {
                    self.w("(");
                    self.w(op.as_str());
                    self.expr(expr);
                    self.w(")");
                }
            },
            Expr::Update { prefix, inc, expr } => {
                if *prefix {
                    self.w(if *inc { "++" } else { "--" });
                    self.expr_naked(expr);
                } else {
                    self.expr_naked(expr);
                    self.w(if *inc { "++" } else { "--" });
                }
            }
            Expr::Binary { op, l, r } => self.binary(e, *op, l, r),
            Expr::Assign { op, target, value } => {
                self.assign_target(target);
                self.w(" ");
                self.w(op);
                self.w(" ");
                self.expr(value);
                if let Some(n) = check::result_coercion_for(e) {
                    self.w(", ");
                    self.assign_target(target);
                    self.w(" = ");
                    self.conv_open(n);
                    self.assign_target(target);
                    self.w(")");
                }
            }
            Expr::Cond { cond, then, els } => {
                self.w("(");
                self.expr(cond);
                self.w(" ? ");
                self.expr(then);
                self.w(" : ");
                self.expr(els);
                self.w(")");
            }
            Expr::Cast { expr, wrapping, ty } => {
                let head = type_head(ty);
                match head.as_str() {
                    "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32"
                    | "uint64" | "int" | "float32" | "float64" => {
                        self.w(&format!(
                            "$rt.cast(\"{head}\", {}, ",
                            if *wrapping { "true" } else { "false" }
                        ));
                        self.expr(expr);
                        self.w(")");
                    }
                    _ => {
                        self.w(&format!("$rt.castRef(\"{head}\", "));
                        self.expr(expr);
                        self.w(")");
                    }
                }
            }
            Expr::Is { expr, ty } => {
                self.w("$rt.is(");
                self.expr(expr);
                self.w(&format!(", \"{}\")", type_head(ty)));
            }
            Expr::Call {
                callee,
                args,
                optional,
                ..
            } => match callee.as_ref() {
                // x.m(...) dispatches through the runtime: Mersey's built-in
                // method surface wins over the JS native where they differ.
                Expr::Member {
                    obj,
                    name,
                    optional: m_opt,
                } => {
                    self.w("$rt.call(");
                    self.expr(obj);
                    self.w(&format!(
                        ", \"{name}\", {}",
                        if *m_opt || *optional { "true" } else { "false" }
                    ));
                    for a in args {
                        self.w(", ");
                        if a.spread {
                            self.w("...$rt.iter(");
                            self.expr(&a.expr);
                            self.w(")");
                        } else {
                            self.expr(&a.expr);
                        }
                    }
                    self.w(")");
                }
                _ => {
                    self.expr_naked(callee);
                    if *optional {
                        self.w("?.");
                    }
                    self.args(args);
                }
            },
            Expr::New { ty, args } => {
                self.w("new ");
                self.w(&type_head(ty));
                self.args(args);
            }
            Expr::Member {
                obj,
                name,
                optional,
            } => {
                self.w("$rt.get(");
                self.expr(obj);
                self.w(&format!(
                    ", \"{name}\", {})",
                    if *optional { "true" } else { "false" }
                ));
            }
            Expr::Index {
                obj,
                index,
                optional: _,
            } => {
                self.w("$rt.index(");
                self.expr(obj);
                self.w(", ");
                self.expr(index);
                self.w(")");
            }
            Expr::SuperMember { name, .. } => {
                self.w("super.");
                self.w(name);
            }
            Expr::SuperCall { args, .. } => {
                self.w("super");
                self.args(args);
            }
            Expr::ImportCall(inner) => {
                // A literal specifier resolves now (the graph is closed, §4.5);
                // the runtime maps it to the module's URL at run time.
                if let Expr::Lit {
                    kind: LitKind::Str,
                    text,
                    ..
                } = inner.as_ref()
                {
                    let raw = mersey_front::ast::string_value(text);
                    let resolved = mersey_front::graph::resolve_module(&self.spec, &raw);
                    self.w(&format!("$rt.dynImport(\"mersey-mod:{resolved}\")"));
                } else {
                    self.w("$rt.dynImport(");
                    self.expr(inner);
                    self.w(")");
                }
            }
            Expr::Yield { value, .. } => {
                self.w("(yield");
                if let Some(v) = value {
                    self.w(" ");
                    self.expr(v);
                }
                self.w(")");
            }
        }
    }

    /// The left side of an assignment: plain member/index syntax, not the
    /// runtime read helpers.
    fn assign_target(&mut self, t: &Expr) {
        match t {
            Expr::Member { obj, name, .. } => {
                self.expr_naked(obj);
                self.w(".");
                self.w(name);
            }
            Expr::Index { obj, index, .. } => {
                self.expr_naked(obj);
                self.w("[");
                self.expr(index);
                self.w("]");
            }
            other => self.expr_naked(other),
        }
    }

    fn args(&mut self, args: &[ArrayElem]) {
        self.w("(");
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            if a.spread {
                self.w("...$rt.iter(");
                self.expr(&a.expr);
                self.w(")");
            } else {
                self.expr(&a.expr);
            }
        }
        self.w(")");
    }

    fn binary(&mut self, node: &Expr, op: BinOp, l: &Expr, r: &Expr) {
        use BinOp::*;
        let nt = check::op_type_for(node);
        match op {
            Add | Sub | Mul | Div | Rem | Pow => self.arith(op, l, r, nt),
            Eq => self.eq(l, r, false),
            Ne => self.eq(l, r, true),
            Lt | Gt | Le | Ge => match nt {
                // A known numeric lane compares natively.
                Some(_) => self.plain(l, r, op.as_str()),
                // Unknown lane: strings compare natively too, but bigdec needs
                // value comparison — the runtime dispatches.
                None => {
                    self.w(&format!("$rt.ord(\"{}\", ", op.as_str()));
                    self.expr(l);
                    self.w(", ");
                    self.expr(r);
                    self.w(")");
                }
            },
            And | Or | Coalesce => self.plain(l, r, op.as_str()),
            Instanceof => self.plain(l, r, "instanceof"),
            BitAnd | BitOr | BitXor => match nt {
                Some(Num::Int(IntKind::U32)) => self.wrapped(l, r, op.as_str(), ") >>> 0)"),
                _ => self.plain(l, r, op.as_str()),
            },
            Shl | Shr => match nt {
                Some(Num::Int(IntKind::I64 | IntKind::U64)) => self.plain(l, r, op.as_str()),
                Some(Num::Int(IntKind::U32)) => self.wrapped(l, r, op.as_str(), ") >>> 0)"),
                _ => self.plain(l, r, op.as_str()),
            },
        }
    }

    fn plain(&mut self, l: &Expr, r: &Expr, js: &str) {
        self.w("(");
        self.expr(l);
        self.w(&format!(" {js} "));
        self.expr(r);
        self.w(")");
    }

    fn wrapped(&mut self, l: &Expr, r: &Expr, js: &str, close: &str) {
        self.w("((");
        self.expr(l);
        self.w(&format!(" {js} "));
        self.expr(r);
        self.w(close);
    }

    fn eq(&mut self, l: &Expr, r: &Expr, neg: bool) {
        // Structural kinds (bigint objects, bigdec) can't use ===; the runtime
        // decides by value and falls through to === for primitives.
        self.w(if neg { "!$rt.eq(" } else { "$rt.eq(" });
        self.expr(l);
        self.w(", ");
        self.expr(r);
        self.w(")");
    }

    fn arith(&mut self, op: BinOp, l: &Expr, r: &Expr, nt: Option<Num>) {
        use BinOp::*;
        let js = op.as_str();
        match nt {
            Some(Num::Int(
                k @ (IntKind::I8
                | IntKind::I16
                | IntKind::I32
                | IntKind::U8
                | IntKind::U16
                | IntKind::U32),
            )) => {
                let close = match k {
                    IntKind::I32 => ") | 0)",
                    IntKind::U32 => ") >>> 0)",
                    IntKind::I16 => ") << 16 >> 16)",
                    IntKind::U16 => ") & 0xFFFF)",
                    IntKind::I8 => ") << 24 >> 24)",
                    IntKind::U8 => ") & 0xFF)",
                    _ => unreachable!(),
                };
                match op {
                    Mul => {
                        if k == IntKind::I32 {
                            self.w("Math.imul(");
                            self.expr(l);
                            self.w(", ");
                            self.expr(r);
                            self.w(")");
                        } else {
                            self.w("((Math.imul(");
                            self.expr(l);
                            self.w(", ");
                            self.expr(r);
                            self.w(&format!("){}", &close[1..]));
                        }
                    }
                    Div => {
                        self.w("$rt.idiv(");
                        self.expr(l);
                        self.w(", ");
                        self.expr(r);
                        self.w(")");
                    }
                    Rem => {
                        self.w("$rt.imod(");
                        self.expr(l);
                        self.w(", ");
                        self.expr(r);
                        self.w(")");
                    }
                    _ => self.wrapped(l, r, js, close),
                }
            }
            Some(Num::Int(IntKind::I64 | IntKind::U64)) => match op {
                Div => {
                    self.w("$rt.idiv64(");
                    self.expr(l);
                    self.w(", ");
                    self.expr(r);
                    self.w(")");
                }
                Rem => {
                    self.w("$rt.imod64(");
                    self.expr(l);
                    self.w(", ");
                    self.expr(r);
                    self.w(")");
                }
                _ => self.plain(l, r, js),
            },
            Some(Num::F32) => {
                self.w("Math.fround(");
                self.expr(l);
                self.w(&format!(" {js} "));
                self.expr(r);
                self.w(")");
            }
            // f64: plain JS arithmetic — no dispatch, the JIT's own fast path.
            Some(Num::F64) => self.plain(l, r, js),
            _ => {
                // No numeric op type: string concat, bigint/bigdec arithmetic,
                // or plain f64. `add` dispatches by value; the rest are f64 or
                // bigdec (runtime handles the object kinds).
                if op == Add {
                    self.w("$rt.add(");
                    self.expr(l);
                    self.w(", ");
                    self.expr(r);
                    self.w(")");
                } else {
                    self.w(&format!("$rt.arith(\"{js}\", "));
                    self.expr(l);
                    self.w(", ");
                    self.expr(r);
                    self.w(")");
                }
            }
        }
    }

    fn lit(&mut self, kind: LitKind, text: &str) {
        match kind {
            LitKind::Null => self.w("null"),
            LitKind::Bool => self.w(text),
            LitKind::Int => {
                let t = text.replace('_', "");
                let is64 = t.ends_with("i64")
                    || t.ends_with("u64")
                    || (t.ends_with('l') && !t.ends_with("ul"))
                    || t.ends_with("ul");
                let digits = strip_int_suffix(&t);
                self.w(digits);
                if is64 {
                    self.w("n");
                }
            }
            LitKind::Float => {
                let t = text.replace('_', "");
                let core = t.trim_end_matches('f');
                if t.ends_with('f') {
                    self.w(&format!("Math.fround({core})"));
                } else {
                    self.w(core);
                }
            }
            LitKind::BigInt => {
                // Mersey bigint and int64 share JS BigInt; `123n` is already a
                // valid JS literal.
                let t = text.replace('_', "");
                self.w(&t);
            }
            LitKind::BigDec => {
                let t = text.replace('_', "");
                self.w(&format!("$rt.bigdec(\"{}\")", t.trim_end_matches('m')));
            }
            LitKind::Str => self.w(text),
            LitKind::Char => {
                let inner = &text[2..text.len() - 1];
                self.w(&format!("\"{inner}\""));
            }
        }
    }
}

fn strip_int_suffix(t: &str) -> &str {
    for s in ["u64", "u32", "u16", "ul", "u8", "i64", "i32", "i16", "i8", "l", "u"] {
        if let Some(d) = t.strip_suffix(s) {
            return d;
        }
    }
    t
}

/// The head identifier of a type expression (`Foo<T>` → "Foo", `T?` → head of
/// T). Used for casts, catches and `is` — where the runtime tests by name.
fn type_head(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named { name, .. } => name.clone(),
        TypeExpr::Nullable(inner) => type_head(inner),
        TypeExpr::ArrayOf(inner) => format!("{}[]", type_head(inner)),
        TypeExpr::Union(_) => "union".to_string(),
        TypeExpr::Tuple(_) => "array".to_string(),
        TypeExpr::Record(_) => "record".to_string(),
        TypeExpr::Function { .. } => "function".to_string(),
    }
}

fn body_has_yield(body: &[Stmt]) -> bool {
    let mut found = false;
    for s in body {
        ast::walk_stmt(s, &mut |e: &Expr| {
            if matches!(e, Expr::Yield { .. }) {
                found = true;
            }
        });
    }
    found
}

fn pattern_has_default(p: &Pattern) -> bool {
    match p {
        Pattern::Name(_) => false,
        Pattern::Array { elems, rest } => {
            elems.iter().any(|e| e.default.is_some() || pattern_has_default(&e.target))
                || rest.as_deref().is_some_and(pattern_has_default)
        }
        Pattern::Record(fields) => fields.iter().any(|f| {
            f.default.is_some() || f.target.as_ref().is_some_and(pattern_has_default)
        }),
    }
}

fn ends_in_jump(body: &[Stmt]) -> bool {
    matches!(
        body.last(),
        Some(Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Throw(_))
    )
}
