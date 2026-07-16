//! The Mersey → WASM compiler: the compute tier of the browser polyfill.
//!
//! The JS backend runs everything, but integer kernels pay JS's price for
//! integer semantics (`|0`, `Math.imul` — ~2× a native JIT). This module
//! compiles the same subset the engine's Tier-1 Cranelift JIT accepts — typed
//! numeric functions with no allocation and no host calls — straight to a
//! WebAssembly module, where `int32` and `int64` are *machine types*. The JS
//! backend binds the exports in place of the transpiled functions; everything
//! else stays JS.
//!
//! Qualification is conservative and total: a function that uses anything
//! outside the subset is simply left to the JS backend — same program, same
//! answers, different speed. The conformance goldens gate both tiers.

use mersey_front::ast::*;
use mersey_front::check::{self, IntKind, Num};

/// A compiled export: name, parameter kinds, return kind (None = void).
pub struct WasmExport {
    pub name: String,
    pub params: Vec<Num>,
    pub ret: Option<Num>,
}

pub struct WasmTier {
    pub bytes: Vec<u8>,
    pub exports: Vec<WasmExport>,
}

/// Machine value type of a numeric kind.
fn vt(n: Num) -> u8 {
    match n {
        Num::Int(IntKind::I64 | IntKind::U64) => 0x7e, // i64
        Num::Int(_) => 0x7f,                           // i32
        Num::F32 => 0x7d,
        Num::F64 => 0x7c,
    }
}

fn is_unsigned(n: Num) -> bool {
    matches!(
        n,
        Num::Int(IntKind::U8 | IntKind::U16 | IntKind::U32 | IntKind::U64)
    )
}

fn kind_of_type(t: &TypeExpr) -> Option<Num> {
    let TypeExpr::Named { name, .. } = t else {
        return None;
    };
    Some(match name.as_str() {
        "int8" => Num::Int(IntKind::I8),
        "int16" => Num::Int(IntKind::I16),
        "int" | "int32" => Num::Int(IntKind::I32),
        "int64" => Num::Int(IntKind::I64),
        "uint8" => Num::Int(IntKind::U8),
        "uint16" => Num::Int(IntKind::U16),
        "uint32" => Num::Int(IntKind::U32),
        "uint64" => Num::Int(IntKind::U64),
        "float32" => Num::F32,
        "float64" => Num::F64,
        _ => return None,
    })
}

// ---- qualification ----------------------------------------------------------

struct Fq<'a> {
    decl: &'a FnDecl,
    params: Vec<(String, Num)>,
    ret: Option<Num>,
}

/// The set of top-level functions the WASM tier takes, with mutual calls
/// resolved to a fixpoint: a function calling a disqualified one is itself
/// disqualified.
pub fn compile(fns: &[&FnDecl]) -> Option<WasmTier> {
    compile_with(fns, &std::collections::HashSet::new())
}

/// `math_names`: identifiers bound to std:math (usually just "math") — calls
/// like `math.sqrt(x)` lower to the f64 opcode instead of disqualifying.
pub fn compile_with(
    fns: &[&FnDecl],
    math_names: &std::collections::HashSet<String>,
) -> Option<WasmTier> {
    let mut cands: Vec<Fq> = Vec::new();
    for f in fns {
        if f.is_async || !f.type_params.is_empty() {
            continue;
        }
        let mut params = Vec::new();
        let mut ok = true;
        for p in &f.params {
            if p.rest || p.optional || p.default.is_some() {
                ok = false;
                break;
            }
            let (Pattern::Name(n), Some(ty)) = (&p.target, &p.ty) else {
                ok = false;
                break;
            };
            let Some(k) = kind_of_type(ty) else {
                ok = false;
                break;
            };
            params.push((n.text.clone(), k));
        }
        if !ok {
            continue;
        }
        let ret = match &f.ret {
            None => None,
            Some(TypeExpr::Named { name, .. }) if name == "void" => None,
            Some(t) => match kind_of_type(t) {
                Some(k) => Some(k),
                None => continue,
            },
        };
        cands.push(Fq {
            decl: f,
            params,
            ret,
        });
    }
    // Fixpoint: drop candidates whose bodies fall outside the subset (which
    // includes calling a non-candidate).
    loop {
        let names: Vec<String> = cands.iter().map(|c| c.decl.name.text.clone()).collect();
        let before = cands.len();
        cands.retain(|c| body_qualifies(c, &names, math_names));
        if cands.len() == before {
            break;
        }
    }
    if cands.is_empty() {
        return None;
    }
    Some(emit(&cands, math_names))
}

/// A quick pre-pass: does the body stay inside the subset? (The emitter
/// re-infers everything; this only decides membership.)
fn body_qualifies(
    f: &Fq,
    names: &[String],
    math_names: &std::collections::HashSet<String>,
) -> bool {
    let mut locals: Vec<(String, Num)> = f.params.clone();
    let mut ok = true;
    for s in &f.decl.body {
        qual_stmt(s, names, math_names, &mut locals, &mut ok);
    }
    ok
}

/// The f64 opcodes std:math maps to directly. min/max take two operands.
fn math_op(name: &str) -> Option<(u8, u8)> {
    Some(match name {
        "abs" => (0x99, 1),
        "ceil" => (0x9b, 1),
        "floor" => (0x9c, 1),
        "trunc" => (0x9d, 1),
        "sqrt" => (0x9f, 1),
        "min" => (0xa4, 2),
        "max" => (0xa5, 2),
        _ => return None,
    })
}

fn qual_stmt(
    s: &Stmt,
    names: &[String],
    math_names: &std::collections::HashSet<String>,
    locals: &mut Vec<(String, Num)>,
    ok: &mut bool,
) {
    if !*ok {
        return;
    }
    match s {
        Stmt::Block(b) => {
            let mark = locals.len();
            b.iter().for_each(|s| qual_stmt(s, names, math_names, locals, ok));
            locals.truncate(mark);
        }
        Stmt::Var(v) => {
            for b in &v.bindings {
                let Pattern::Name(n) = &b.target else {
                    *ok = false;
                    return;
                };
                let k = b
                    .ty
                    .as_ref()
                    .and_then(kind_of_type)
                    .or_else(|| check::local_type_for(n))
                    .or_else(|| b.init.as_ref().and_then(|e| infer(e, locals, names)));
                let Some(k) = k else {
                    *ok = false;
                    return;
                };
                if let Some(init) = &b.init {
                    qual_expr(init, names, math_names, locals, ok);
                }
                locals.push((n.text.clone(), k));
            }
        }
        Stmt::Expr(e) => qual_expr(e, names, math_names, locals, ok),
        Stmt::Empty | Stmt::Break { label: None, .. } | Stmt::Continue { label: None, .. } => {}
        Stmt::If { cond, then, els } => {
            qual_expr(cond, names, math_names, locals, ok);
            qual_stmt(then, names, math_names, locals, ok);
            if let Some(e) = els {
                qual_stmt(e, names, math_names, locals, ok);
            }
        }
        Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
            qual_expr(cond, names, math_names, locals, ok);
            qual_stmt(body, names, math_names, locals, ok);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            match init {
                Some(ForInit::Var(v)) => qual_stmt(&Stmt::Var(clone_var(v)), names, math_names, locals, ok),
                Some(ForInit::Exprs(es)) => es.iter().for_each(|e| qual_expr(e, names, math_names, locals, ok)),
                None => {}
            }
            if let Some(c) = cond {
                qual_expr(c, names, math_names, locals, ok);
            }
            step.iter().for_each(|e| qual_expr(e, names, math_names, locals, ok));
            qual_stmt(body, names, math_names, locals, ok);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                qual_expr(v, names, math_names, locals, ok);
            }
        }
        _ => *ok = false, // ForOf, Switch, Try, Throw, labels: JS tier
    }
}

// Var borrows are awkward through ForInit; a shallow clone of the bindings'
// *shape* is enough for qualification (exprs are visited by reference above).
fn clone_var(v: &VarStmt) -> VarStmt {
    VarStmt {
        kind: v.kind,
        bindings: v
            .bindings
            .iter()
            .map(|b| Binding {
                target: clone_pattern(&b.target),
                ty: b.ty.as_ref().map(clone_type),
                init: None, // init already visited by the caller
            })
            .collect(),
    }
}
fn clone_pattern(p: &Pattern) -> Pattern {
    match p {
        Pattern::Name(n) => Pattern::Name(Name {
            text: n.text.clone(),
            pos: n.pos,
        }),
        _ => Pattern::Name(Name {
            text: String::new(),
            pos: mersey_front::diag::Pos { line: 0, col: 0 },
        }),
    }
}
fn clone_type(t: &TypeExpr) -> TypeExpr {
    match t {
        TypeExpr::Named { name, pos, .. } => TypeExpr::Named {
            name: name.clone(),
            pos: *pos,
            args: Vec::new(),
        },
        _ => TypeExpr::Named {
            name: "?".into(),
            pos: mersey_front::diag::Pos { line: 0, col: 0 },
            args: Vec::new(),
        },
    }
}

fn qual_expr(
    e: &Expr,
    names: &[String],
    math_names: &std::collections::HashSet<String>,
    locals: &Vec<(String, Num)>,
    ok: &mut bool,
) {
    if !*ok {
        return;
    }
    if infer(e, locals, names).is_none() && !matches!(e, Expr::Call { .. }) {
        // Calls to void functions infer None but still qualify.
        if let Expr::Call { callee, .. } = e {
            let _ = callee;
        } else {
            *ok = false;
            return;
        }
    }
    match e {
        Expr::Ident(_) | Expr::Lit { .. } | Expr::This(_) => {
            if matches!(e, Expr::This(_)) {
                *ok = false;
            }
        }
        Expr::Paren(x) => qual_expr(x, names, math_names, locals, ok),
        Expr::Unary { op, expr, .. } => {
            if matches!(op, UnaryOp::Await) {
                *ok = false;
                return;
            }
            qual_expr(expr, names, math_names, locals, ok);
        }
        Expr::Update { prefix: _, expr, .. } => {
            if !matches!(infer(expr, locals, names), Some(Num::Int(_))) {
                *ok = false;
                return;
            }
            qual_expr(expr, names, math_names, locals, ok);
        }
        Expr::Binary { op, l, r } => {
            if matches!(op, BinOp::Instanceof | BinOp::Coalesce | BinOp::Pow) {
                *ok = false;
                return;
            }
            qual_expr(l, names, math_names, locals, ok);
            qual_expr(r, names, math_names, locals, ok);
        }
        Expr::Assign { op: _, target, value } => {
            let Expr::Ident(_) = target.as_ref() else {
                *ok = false;
                return;
            };
            qual_expr(value, names, math_names, locals, ok);
        }
        Expr::Cond { cond, then, els } => {
            qual_expr(cond, names, math_names, locals, ok);
            qual_expr(then, names, math_names, locals, ok);
            qual_expr(els, names, math_names, locals, ok);
        }
        Expr::Cast { expr, wrapping, ty } => {
            let Some(to) = kind_of_type(ty) else {
                *ok = false;
                return;
            };
            qual_expr(expr, names, math_names, locals, ok);
            if !*ok {
                return;
            }
            let from = infer(expr, locals, names);
            let Some(from) = from else {
                *ok = false;
                return;
            };
            // Checked narrowing needs a range check + a Mersey throw — JS tier.
            // Wrapping casts and widenings are pure conversions.
            if !*wrapping && !widens(from, to) {
                *ok = false;
            }
        }
        Expr::Call { callee, args, type_args, optional } => {
            if *optional || !type_args.is_empty() {
                *ok = false;
                return;
            }
            match callee.as_ref() {
                Expr::Ident(n) if names.contains(&n.text) => {}
                // math.sqrt(x) etc: a std:math intrinsic with a wasm opcode.
                Expr::Member { obj, name, optional: false }
                    if matches!(obj.as_ref(), Expr::Ident(m)
                        if math_names.contains(&m.text)
                            && locals.iter().all(|(l, _)| l != &m.text))
                        && math_op(name).is_some() => {}
                _ => {
                    *ok = false;
                    return;
                }
            }
            for a in args {
                if a.spread {
                    *ok = false;
                    return;
                }
                qual_expr(&a.expr, names, math_names, locals, ok);
            }
        }
        _ => *ok = false,
    }
}

fn widens(from: Num, to: Num) -> bool {
    use IntKind::*;
    match (from, to) {
        (a, b) if a == b => true,
        (Num::Int(a), Num::Int(b)) => rank(a) <= rank(b) && signed(a) == signed(b),
        (Num::Int(_), Num::F64) | (Num::Int(_), Num::F32) => true,
        (Num::F32, Num::F64) => true,
        _ => false,
    }
}
fn rank(k: IntKind) -> u8 {
    use IntKind::*;
    match k {
        I8 | U8 => 1,
        I16 | U16 => 2,
        I32 | U32 => 3,
        I64 | U64 => 4,
    }
}
fn signed(k: IntKind) -> bool {
    matches!(k, IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64)
}

/// The numeric kind of an expression, from the checker's tables plus local
/// context. The *coerced* kind when the checker recorded a conversion.
fn infer(e: &Expr, locals: &Vec<(String, Num)>, names: &[String]) -> Option<Num> {
    if let Some(k) = check::coercion_for(e) {
        return Some(k);
    }
    let raw = match e {
        Expr::Ident(n) => locals.iter().rev().find(|(s, _)| s == &n.text).map(|(_, k)| *k),
        Expr::Lit { kind, text, .. } => match kind {
            LitKind::Int => {
                let t = text.replace('_', "");
                Some(if t.ends_with("i64") || t.ends_with('l') && !t.ends_with("ul") {
                    Num::Int(IntKind::I64)
                } else if t.ends_with("u64") || t.ends_with("ul") {
                    Num::Int(IntKind::U64)
                } else if t.ends_with("u32") || t.ends_with('u') {
                    Num::Int(IntKind::U32)
                } else {
                    Num::Int(IntKind::I32)
                })
            }
            LitKind::Float => Some(if text.ends_with('f') { Num::F32 } else { Num::F64 }),
            _ => None,
        },
        Expr::Paren(x) => infer(x, locals, names),
        Expr::Unary { op, expr, .. } => match op {
            UnaryOp::Not => Some(Num::Int(IntKind::I32)), // bool as i32
            _ => infer(expr, locals, names),
        },
        Expr::Update { expr, .. } => infer(expr, locals, names),
        Expr::Binary { op, l, r } => {
            use BinOp::*;
            match op {
                Eq | Ne | Lt | Gt | Le | Ge | And | Or => Some(Num::Int(IntKind::I32)),
                _ => check::op_type_for(e)
                    .or_else(|| infer(l, locals, names))
                    .or_else(|| infer(r, locals, names)),
            }
        }
        Expr::Assign { target, .. } => infer(target, locals, names),
        Expr::Cond { then, els, .. } => {
            infer(then, locals, names).or_else(|| infer(els, locals, names))
        }
        Expr::Cast { ty, .. } => kind_of_type(ty),
        Expr::Call { callee, .. } => {
            let Expr::Ident(_n) = callee.as_ref() else {
                return None;
            };
            // Resolved against the candidate table by the emitter; here we
            // only need "numeric or void" — report i32 as a stand-in when the
            // name is a candidate. The emitter uses the real signature.
            None
        }
        _ => None,
    };
    raw
}

// ---- emission -----------------------------------------------------------------

struct Sig {
    params: Vec<u8>,
    ret: Vec<u8>,
}

fn emit(fns: &[Fq], math_names: &std::collections::HashSet<String>) -> WasmTier {
    // Signatures, deduplicated.
    let mut sigs: Vec<Sig> = Vec::new();
    let mut fn_sig: Vec<usize> = Vec::new();
    for f in fns {
        let s = Sig {
            params: f.params.iter().map(|(_, k)| vt(*k)).collect(),
            ret: f.ret.map(|k| vec![vt(k)]).unwrap_or_default(),
        };
        let idx = sigs
            .iter()
            .position(|x| x.params == s.params && x.ret == s.ret)
            .unwrap_or_else(|| {
                sigs.push(s);
                sigs.len() - 1
            });
        fn_sig.push(idx);
    }
    let names: Vec<String> = fns.iter().map(|f| f.decl.name.text.clone()).collect();
    let table: Vec<FnInfo> = fns
        .iter()
        .map(|f| FnInfo {
            name: f.decl.name.text.clone(),
            params: f.params.clone(),
            ret: f.ret,
            decl: f.decl,
        })
        .collect();

    let mut bodies: Vec<Vec<u8>> = Vec::new();
    for f in fns {
        let mut g = Gen {
            code: Vec::new(),
            locals: f
                .params
                .iter()
                .enumerate()
                .map(|(i, (n, k))| (n.clone(), i as u32, *k))
                .collect(),
            n_params: f.params.len(),
            extra: Vec::new(),
            names: &names,
            table: &table,
            math_names,
            ret: f.ret,
            cur_depth: 0,
            inline_depth: 0,
            loops: Vec::new(),
        };
        for s in &f.decl.body {
            g.stmt(s);
        }
        // An implicit fall-off-the-end return: void is fine; a value-returning
        // function that falls off pushes a zero (the checker guarantees all
        // paths return, so this is dead — wasm just needs the types to line up).
        if let Some(k) = f.ret {
            g.zero(k);
        }
        g.code.push(0x0f); // return
        g.code.push(0x0b); // end

        // locals declaration: group the extras by valtype.
        let mut decl = Vec::new();
        let extra: Vec<u8> = g.extra.clone();
        let mut groups: Vec<(u8, u32)> = Vec::new();
        for t in extra {
            match groups.last_mut() {
                Some((lt, n)) if *lt == t => *n += 1,
                _ => groups.push((t, 1)),
            }
        }
        leb(&mut decl, groups.len() as u64);
        for (t, n) in groups {
            leb(&mut decl, n as u64);
            decl.push(t);
        }
        decl.extend_from_slice(&g.code);
        bodies.push(decl);
    }

    // ---- assemble the module ----
    let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // type section
    let mut sec = Vec::new();
    leb(&mut sec, sigs.len() as u64);
    for s in &sigs {
        sec.push(0x60);
        leb(&mut sec, s.params.len() as u64);
        sec.extend_from_slice(&s.params);
        leb(&mut sec, s.ret.len() as u64);
        sec.extend_from_slice(&s.ret);
    }
    section(&mut m, 1, &sec);
    // function section
    let mut sec = Vec::new();
    leb(&mut sec, fns.len() as u64);
    for i in &fn_sig {
        leb(&mut sec, *i as u64);
    }
    section(&mut m, 3, &sec);
    // export section
    let mut sec = Vec::new();
    leb(&mut sec, fns.len() as u64);
    for (i, f) in fns.iter().enumerate() {
        let name = f.decl.name.text.as_bytes();
        leb(&mut sec, name.len() as u64);
        sec.extend_from_slice(name);
        sec.push(0x00);
        leb(&mut sec, i as u64);
    }
    section(&mut m, 7, &sec);
    // code section
    let mut sec = Vec::new();
    leb(&mut sec, bodies.len() as u64);
    for b in &bodies {
        leb(&mut sec, b.len() as u64);
        sec.extend_from_slice(b);
    }
    section(&mut m, 10, &sec);

    WasmTier {
        bytes: m,
        exports: fns
            .iter()
            .map(|f| WasmExport {
                name: f.decl.name.text.clone(),
                params: f.params.iter().map(|(_, k)| *k).collect(),
                ret: f.ret,
            })
            .collect(),
    }
}

fn section(m: &mut Vec<u8>, id: u8, body: &[u8]) {
    m.push(id);
    leb(m, body.len() as u64);
    m.extend_from_slice(body);
}

fn leb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let sign = b & 0x40 != 0;
        if (v == 0 && !sign) || (v == -1 && sign) {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

struct Gen<'a> {
    code: Vec<u8>,
    /// In-scope names -> (wasm local index, kind). Truncated at block exit —
    /// shadowing resolves lexically — while the slots themselves live for the
    /// whole function (wasm locals are function-scoped).
    locals: Vec<(String, u32, Num)>,
    n_params: usize,
    /// value types of the non-param locals, same order.
    extra: Vec<u8>,
    names: &'a [String],
    table: &'a [FnInfo<'a>],
    math_names: &'a std::collections::HashSet<String>,
    ret: Option<Num>,
    /// Current structured-control nesting depth (labels open right now).
    cur_depth: usize,
    /// How many inline expansions deep we are (bounded).
    inline_depth: usize,
    loops: Vec<LoopCtx>,
}

struct FnInfo<'a> {
    name: String,
    params: Vec<(String, Num)>,
    ret: Option<Num>,
    decl: &'a FnDecl,
}

/// A function whose body is exactly `return <expr>;` inlines at call sites —
/// the wins V8's JS inliner gets for free, done here for the wasm tier.
fn leaf_expr(decl: &FnDecl) -> Option<&Expr> {
    match decl.body.as_slice() {
        [Stmt::Return { value: Some(e), .. }] => Some(e),
        _ => None,
    }
}

struct LoopCtx {
    /// Absolute depth of the label `continue` branches to (the loop header,
    /// or the body block in a `for` so the step still runs).
    cont_depth: usize,
    /// Absolute depth of the label `break` branches to (the wrapping block).
    break_depth: usize,
}

const I32_MIN: i64 = i32::MIN as i64;

fn strip_suffix(t: &str) -> &str {
    for s in ["u64", "u32", "u16", "ul", "u8", "i64", "i32", "i16", "i8", "l", "u"] {
        if let Some(d) = t.strip_suffix(s) {
            return d;
        }
    }
    t
}

impl<'a> Gen<'a> {
    fn local_kind(&self, name: &str) -> Option<Num> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, _, k)| *k)
    }
    fn local_idx(&self, name: &str) -> u32 {
        self.locals
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, i, _)| *i)
            .expect("qualified local")
    }
    fn add_local(&mut self, name: &str, k: Num) -> u32 {
        let idx = (self.n_params + self.extra.len()) as u32;
        self.extra.push(vt(k));
        self.locals.push((name.to_string(), idx, k));
        idx
    }
    fn scratch(&mut self, k: Num) -> u32 {
        // Anonymous slot for divide guards; no name, no scope entry.
        let idx = (self.n_params + self.extra.len()) as u32;
        self.extra.push(vt(k));
        idx
    }

    fn op(&mut self, byte: u8) {
        self.code.push(byte);
    }
    fn get(&mut self, idx: u32) {
        self.op(0x20);
        leb(&mut self.code, idx as u64);
    }
    fn set(&mut self, idx: u32) {
        self.op(0x21);
        leb(&mut self.code, idx as u64);
    }
    fn const_i(&mut self, k: Num, v: i64) {
        if vt(k) == 0x7e {
            self.op(0x42);
            sleb(&mut self.code, v);
        } else {
            self.op(0x41);
            sleb(&mut self.code, v as i32 as i64);
        }
    }
    fn zero(&mut self, k: Num) {
        match vt(k) {
            0x7f => self.const_i(k, 0),
            0x7e => self.const_i(k, 0),
            0x7d => {
                self.op(0x43);
                self.code.extend_from_slice(&0f32.to_le_bytes());
            }
            _ => {
                self.op(0x44);
                self.code.extend_from_slice(&0f64.to_le_bytes());
            }
        }
    }

    /// Re-normalize a narrow int held in an i32 after an operation that can
    /// overflow its width — the same rule the engine and the JS tier apply.
    fn wrap(&mut self, k: Num) {
        match k {
            Num::Int(IntKind::I8) => {
                self.const_i(k, 24);
                self.op(0x74); // shl
                self.const_i(k, 24);
                self.op(0x75); // shr_s
            }
            Num::Int(IntKind::U8) => {
                self.const_i(k, 0xff);
                self.op(0x71); // and
            }
            Num::Int(IntKind::I16) => {
                self.const_i(k, 16);
                self.op(0x74);
                self.const_i(k, 16);
                self.op(0x75);
            }
            Num::Int(IntKind::U16) => {
                self.const_i(k, 0xffff);
                self.op(0x71);
            }
            _ => {} // i32/u32/i64/u64: machine width; floats: exact
        }
    }

    /// Convert a value of kind `from` (on the stack) to kind `to`.
    fn convert(&mut self, from: Num, to: Num) {
        if from == to {
            return;
        }
        let (fv, tv) = (vt(from), vt(to));
        match (fv, tv) {
            (0x7f, 0x7f) => self.wrap(to),
            (0x7f, 0x7e) => self.op(if is_unsigned(from) { 0xad } else { 0xac }),
            (0x7e, 0x7f) => {
                self.op(0xa7); // i32.wrap_i64
                self.wrap(to);
            }
            (0x7e, 0x7e) => {}
            (0x7f, 0x7c) => self.op(if is_unsigned(from) { 0xb8 } else { 0xb7 }),
            (0x7f, 0x7d) => self.op(if is_unsigned(from) { 0xb3 } else { 0xb2 }),
            (0x7e, 0x7c) => self.op(if is_unsigned(from) { 0xba } else { 0xb9 }),
            (0x7e, 0x7d) => self.op(if is_unsigned(from) { 0xb5 } else { 0xb4 }),
            (0x7d, 0x7c) => self.op(0xbb), // promote
            (0x7c, 0x7d) => self.op(0xb6), // demote
            // float -> int: non-trapping saturating truncation, then wrap.
            (0x7c | 0x7d, 0x7f) => {
                self.op(0xfc);
                leb(
                    &mut self.code,
                    match (fv, is_unsigned(to)) {
                        (0x7d, false) => 0,
                        (0x7d, true) => 1,
                        (_, false) => 2,
                        (_, true) => 3,
                    },
                );
                self.wrap(to);
            }
            (0x7c | 0x7d, 0x7e) => {
                self.op(0xfc);
                leb(
                    &mut self.code,
                    match (fv, is_unsigned(to)) {
                        (0x7d, false) => 4,
                        (0x7d, true) => 5,
                        (_, false) => 6,
                        (_, true) => 7,
                    },
                );
            }
            _ => {}
        }
    }

    // ---- statements ------------------------------------------------------

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        let mark = self.locals.len();
        f(self);
        self.locals.truncate(mark);
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block(b) => self.scoped(|g| b.iter().for_each(|s| g.stmt(s))),
            Stmt::Var(v) => {
                for b in &v.bindings {
                    let Pattern::Name(n) = &b.target else { unreachable!() };
                    let k = b
                        .ty
                        .as_ref()
                        .and_then(kind_of_type)
                        .or_else(|| check::local_type_for(n))
                        .or_else(|| b.init.as_ref().and_then(|e| self.infer_here(e)))
                        .expect("qualified local kind");
                    match &b.init {
                        Some(init) => {
                            let ik = self.expr(init);
                            self.convert(ik, k);
                        }
                        None => self.zero(k),
                    }
                    let idx = self.add_local(&n.text, k);
                    self.set(idx);
                }
            }
            Stmt::Expr(e) => self.stmt_expr(e),
            Stmt::Empty => {}
            Stmt::If { cond, then, els } => {
                self.expr(cond);
                self.op(0x04); // if
                self.op(0x40); // void
                self.cur_depth += 1;
                self.scoped(|g| g.stmt(then));
                if let Some(e) = els {
                    self.op(0x05); // else
                    self.scoped(|g| g.stmt(e));
                }
                self.cur_depth -= 1;
                self.op(0x0b);
            }
            Stmt::While { cond, body } => {
                self.op(0x02); // block (break target)
                self.op(0x40);
                self.cur_depth += 1;
                let brk = self.cur_depth;
                self.op(0x03); // loop
                self.op(0x40);
                self.cur_depth += 1;
                let top = self.cur_depth;
                self.expr(cond);
                self.op(0x45); // i32.eqz
                self.br_to(brk, true);
                self.loops.push(LoopCtx {
                    cont_depth: top,
                    break_depth: brk,
                });
                self.scoped(|g| g.stmt(body));
                self.loops.pop();
                self.br_to(top, false);
                self.cur_depth -= 1;
                self.op(0x0b);
                self.cur_depth -= 1;
                self.op(0x0b);
            }
            Stmt::DoWhile { body, cond } => {
                self.op(0x02);
                self.op(0x40);
                self.cur_depth += 1;
                let brk = self.cur_depth;
                self.op(0x03);
                self.op(0x40);
                self.cur_depth += 1;
                let top = self.cur_depth;
                // continue in a do-while re-tests the condition: give the body
                // its own block whose end lands on the test.
                self.op(0x02);
                self.op(0x40);
                self.cur_depth += 1;
                let cont = self.cur_depth;
                self.loops.push(LoopCtx {
                    cont_depth: cont,
                    break_depth: brk,
                });
                self.scoped(|g| g.stmt(body));
                self.loops.pop();
                self.cur_depth -= 1;
                self.op(0x0b);
                self.expr(cond);
                self.br_to(top, true);
                self.cur_depth -= 1;
                self.op(0x0b);
                self.cur_depth -= 1;
                self.op(0x0b);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => self.scoped(|g| g.for_loop(init, cond, step, body)),
            Stmt::Break { .. } => {
                let d = self.loops.last().expect("qualified break").break_depth;
                self.br_to(d, false);
            }
            Stmt::Continue { .. } => {
                let d = self.loops.last().expect("qualified continue").cont_depth;
                self.br_to(d, false);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    let k = self.expr(v);
                    if let Some(r) = self.ret {
                        self.convert(k, r);
                    }
                }
                self.op(0x0f);
            }
            _ => unreachable!("disqualified statement"),
        }
    }

    fn for_loop(
        &mut self,
        init: &Option<ForInit>,
        cond: &Option<Expr>,
        step: &[Expr],
        body: &Stmt,
    ) {
        {
            {
                if let Some(fi) = init {
                    match fi {
                        ForInit::Var(v) => {
                            for b in &v.bindings {
                                let Pattern::Name(n) = &b.target else { unreachable!() };
                                let k = b
                                    .ty
                                    .as_ref()
                                    .and_then(kind_of_type)
                                    .or_else(|| check::local_type_for(n))
                                    .or_else(|| b.init.as_ref().and_then(|e| self.infer_here(e)))
                                    .expect("qualified");
                                match &b.init {
                                    Some(initx) => {
                                        let ik = self.expr(initx);
                                        self.convert(ik, k);
                                    }
                                    None => self.zero(k),
                                }
                                let idx = self.add_local(&n.text, k);
                                self.set(idx);
                            }
                        }
                        ForInit::Exprs(es) => es.iter().for_each(|e| self.stmt_expr(e)),
                    }
                }
                self.op(0x02);
                self.op(0x40);
                self.cur_depth += 1;
                let brk = self.cur_depth;
                self.op(0x03);
                self.op(0x40);
                self.cur_depth += 1;
                let top = self.cur_depth;
                if let Some(c) = cond {
                    self.expr(c);
                    self.op(0x45);
                    self.br_to(brk, true);
                }
                self.op(0x02); // continue lands after the body, before step
                self.op(0x40);
                self.cur_depth += 1;
                let cont = self.cur_depth;
                self.loops.push(LoopCtx {
                    cont_depth: cont,
                    break_depth: brk,
                });
                self.scoped(|g| g.stmt(body));
                self.loops.pop();
                self.cur_depth -= 1;
                self.op(0x0b);
                step.iter().for_each(|e| self.stmt_expr(e));
                self.br_to(top, false);
                self.cur_depth -= 1;
                self.op(0x0b);
                self.cur_depth -= 1;
                self.op(0x0b);
            }
        }
    }

    fn br_to(&mut self, label_depth: usize, conditional: bool) {
        let rel = self.cur_depth - label_depth;
        self.op(if conditional { 0x0d } else { 0x0c });
        leb(&mut self.code, rel as u64);
    }

    /// An expression in statement position: leaves nothing on the stack.
    fn stmt_expr(&mut self, e: &Expr) {
        match e {
            Expr::Assign { op, target, value } => {
                let Expr::Ident(n) = target.as_ref() else { unreachable!() };
                let k = self.local_kind(&n.text).expect("qualified");
                let idx = self.local_idx(&n.text);
                if *op == "=" {
                    let vk = self.expr(value);
                    self.convert(vk, k);
                } else {
                    self.get(idx);
                    let vk = self.expr(value);
                    self.convert(vk, k);
                    self.arith_op(&op[..op.len() - 1], k);
                }
                self.set(idx);
            }
            Expr::Update { inc, expr, .. } => {
                let Expr::Ident(n) = expr.as_ref() else { unreachable!() };
                let k = self.local_kind(&n.text).expect("qualified");
                let idx = self.local_idx(&n.text);
                self.get(idx);
                self.const_i(k, 1);
                self.op(match (vt(k), *inc) {
                    (0x7f, true) => 0x6a,
                    (0x7f, false) => 0x6b,
                    (0x7e, true) => 0x7c,
                    (0x7e, false) => 0x7d,
                    _ => unreachable!("float update disqualified"),
                });
                self.wrap(k);
                self.set(idx);
            }
            Expr::Paren(x) => self.stmt_expr(x),
            _ => {
                let k = self.expr_maybe_void(e);
                if k.is_some() {
                    self.op(0x1a); // drop
                }
            }
        }
    }

    // ---- expressions -------------------------------------------------------

    /// Emit `e`; returns the kind left on the stack (None = void call).
    fn expr_maybe_void(&mut self, e: &Expr) -> Option<Num> {
        if let Expr::Call { callee, args, .. } = e {
            // std:math intrinsic: the f64 opcode, no call at all.
            if let Expr::Member { obj: _, name, .. } = callee.as_ref() {
                let (opcode, _arity) = math_op(name).expect("qualified math call");
                for a in args {
                    let k = self.expr(&a.expr);
                    self.convert(k, Num::F64);
                }
                self.op(opcode);
                return Some(Num::F64);
            }
            let Expr::Ident(n) = callee.as_ref() else { unreachable!() };
            let fi = self.names.iter().position(|x| x == &n.text).expect("qualified call");
            let info = &self.table[fi];
            // Inline a leaf callee: bind the arguments to fresh slots under
            // the parameter names (scoped, so shadowing is exact), then emit
            // the return expression in place. Bounded depth handles chains
            // and self-recursive leaves.
            if self.inline_depth < 3 {
                if let Some(body) = leaf_expr(info.decl) {
                    let mark = self.locals.len();
                    // Evaluate all args first (left to right), then bind — a
                    // param name may shadow an outer local used by a later arg.
                    let mut slots = Vec::new();
                    for (i, a) in args.iter().enumerate() {
                        let ak = self.expr(&a.expr);
                        self.convert(ak, info.params[i].1);
                        let slot = self.scratch(info.params[i].1);
                        self.set(slot);
                        slots.push(slot);
                    }
                    for (i, (pname, pk)) in info.params.iter().enumerate() {
                        self.locals.push((pname.clone(), slots[i], *pk));
                    }
                    self.inline_depth += 1;
                    let k = self.expr(body);
                    if let Some(r) = info.ret {
                        self.convert(k, r);
                    }
                    self.inline_depth -= 1;
                    self.locals.truncate(mark);
                    return info.ret;
                }
            }
            for (i, a) in args.iter().enumerate() {
                let ak = self.expr(&a.expr);
                self.convert(ak, info.params[i].1);
            }
            self.op(0x10);
            leb(&mut self.code, fi as u64);
            return info.ret;
        }
        Some(self.expr(e))
    }

    fn expr(&mut self, e: &Expr) -> Num {
        let k = self.expr_raw(e);
        if let Some(c) = check::coercion_for(e) {
            self.convert(k, c);
            return c;
        }
        k
    }

    fn infer_here(&self, e: &Expr) -> Option<Num> {
        if let Expr::Call { callee, .. } = e {
            if let Expr::Member { .. } = callee.as_ref() {
                return Some(Num::F64); // qualified math intrinsic
            }
        }
        let flat: Vec<(String, Num)> =
            self.locals.iter().map(|(n, _, k)| (n.clone(), *k)).collect();
        infer(e, &flat, self.names).or_else(|| {
            if let Expr::Call { callee, .. } = e {
                if let Expr::Ident(n) = callee.as_ref() {
                    if let Some(i) = self.names.iter().position(|x| x == &n.text) {
                        return self.table[i].ret;
                    }
                }
            }
            None
        })
    }

    fn expr_raw(&mut self, e: &Expr) -> Num {
        match e {
            Expr::Ident(n) => {
                let k = self.local_kind(&n.text).expect("qualified ident");
                let idx = self.local_idx(&n.text);
                self.get(idx);
                k
            }
            Expr::Lit { kind, text, .. } => self.literal(*kind, text, false),
            Expr::Paren(x) => self.expr(x),
            Expr::Unary { op, expr, .. } => match op {
                UnaryOp::Neg => {
                    if let Expr::Lit {
                        kind: LitKind::Int,
                        text,
                        ..
                    } = expr.as_ref()
                    {
                        // Fold the sign into the constant: -2147483648 is a
                        // valid int32 only as one literal (§3.3).
                        return self.literal(LitKind::Int, text, true);
                    }
                    let k = self.expr(expr);
                    match vt(k) {
                        0x7c => self.op(0x9a),
                        0x7d => self.op(0x8c),
                        _ => {
                            // 0 - x, computed via a scratch local.
                            let t = self.scratch(k);
                            self.set(t);
                            self.zero(k);
                            self.get(t);
                            self.op(if vt(k) == 0x7e { 0x7d } else { 0x6b });
                            self.wrap(k);
                        }
                    }
                    k
                }
                UnaryOp::Not => {
                    self.expr(expr);
                    self.op(0x45); // i32.eqz
                    Num::Int(IntKind::I32)
                }
                UnaryOp::BitNot => {
                    let k = self.expr(expr);
                    self.const_i(k, -1);
                    self.op(if vt(k) == 0x7e { 0x85 } else { 0x73 }); // xor
                    self.wrap(k);
                    k
                }
                UnaryOp::Plus => self.expr(expr),
                UnaryOp::Await => unreachable!(),
            },
            Expr::Update { prefix, inc, expr } => {
                let Expr::Ident(n) = expr.as_ref() else { unreachable!() };
                let k = self.local_kind(&n.text).expect("qualified");
                let idx = self.local_idx(&n.text);
                if !*prefix {
                    self.get(idx); // old value stays as the result
                }
                self.get(idx);
                self.const_i(k, 1);
                self.op(match (vt(k), *inc) {
                    (0x7f, true) => 0x6a,
                    (0x7f, false) => 0x6b,
                    (0x7e, true) => 0x7c,
                    (0x7e, false) => 0x7d,
                    _ => unreachable!(),
                });
                self.wrap(k);
                self.set(idx);
                if *prefix {
                    self.get(idx);
                }
                k
            }
            Expr::Binary { op, l, r } => self.binary(e, *op, l, r),
            Expr::Assign { .. } => {
                // Value position: perform, then read the target back.
                self.stmt_expr(e);
                let Expr::Assign { target, .. } = e else { unreachable!() };
                let Expr::Ident(n) = target.as_ref() else { unreachable!() };
                let k = self.local_kind(&n.text).expect("q");
                let idx = self.local_idx(&n.text);
                self.get(idx);
                k
            }
            Expr::Cond { cond, then, els } => {
                let k = self
                    .infer_here(then)
                    .or_else(|| self.infer_here(els))
                    .expect("qualified cond kind");
                self.expr(cond);
                self.op(0x04);
                self.op(vt(k));
                self.cur_depth += 1;
                let tk = self.expr(then);
                self.convert(tk, k);
                self.op(0x05);
                let ek = self.expr(els);
                self.convert(ek, k);
                self.cur_depth -= 1;
                self.op(0x0b);
                k
            }
            Expr::Cast { expr, ty, .. } => {
                let from = self.expr(expr);
                let to = kind_of_type(ty).expect("qualified cast");
                self.convert(from, to);
                to
            }
            Expr::Call { .. } => self
                .expr_maybe_void(e)
                .expect("void call in value position disqualified"),
            _ => unreachable!("disqualified expression"),
        }
    }

    fn literal(&mut self, kind: LitKind, text: &str, neg: bool) -> Num {
        match kind {
            LitKind::Int => {
                let t = text.replace('_', "");
                let k = if t.ends_with("i64") || (t.ends_with('l') && !t.ends_with("ul")) {
                    Num::Int(IntKind::I64)
                } else if t.ends_with("u64") || t.ends_with("ul") {
                    Num::Int(IntKind::U64)
                } else if t.ends_with("u32") || t.ends_with('u') {
                    Num::Int(IntKind::U32)
                } else {
                    Num::Int(IntKind::I32)
                };
                let digits = strip_suffix(&t);
                let (radix, body) = if let Some(b) = digits.strip_prefix("0x") {
                    (16, b)
                } else if let Some(b) = digits.strip_prefix("0o") {
                    (8, b)
                } else if let Some(b) = digits.strip_prefix("0b") {
                    (2, b)
                } else {
                    (10, digits)
                };
                let raw = u64::from_str_radix(body, radix).unwrap_or(0);
                let v = if neg { -(raw as i128) } else { raw as i128 } as i64;
                self.const_i(k, v);
                k
            }
            LitKind::Float => {
                let t = text.replace('_', "");
                let core = t.trim_end_matches('f');
                let v: f64 = core.parse().unwrap_or(0.0);
                let v = if neg { -v } else { v };
                if t.ends_with('f') {
                    self.op(0x43);
                    self.code.extend_from_slice(&(v as f32).to_le_bytes());
                    Num::F32
                } else {
                    self.op(0x44);
                    self.code.extend_from_slice(&v.to_le_bytes());
                    Num::F64
                }
            }
            _ => unreachable!("disqualified literal"),
        }
    }

    fn binary(&mut self, node: &Expr, op: BinOp, l: &Expr, r: &Expr) -> Num {
        use BinOp::*;
        match op {
            And | Or => {
                // Short-circuit, result i32 bool.
                self.expr(l);
                self.op(0x04);
                self.op(0x7f);
                self.cur_depth += 1;
                if op == And {
                    self.expr(r);
                    self.op(0x05);
                    self.const_i(Num::Int(IntKind::I32), 0);
                } else {
                    self.const_i(Num::Int(IntKind::I32), 1);
                    self.op(0x05);
                    self.expr(r);
                }
                self.cur_depth -= 1;
                self.op(0x0b);
                Num::Int(IntKind::I32)
            }
            Eq | Ne | Lt | Gt | Le | Ge => {
                let k = check::op_type_for(node)
                    .or_else(|| self.infer_here(l))
                    .or_else(|| self.infer_here(r))
                    .expect("qualified compare kind");
                let lk = self.expr(l);
                self.convert(lk, k);
                let rk = self.expr(r);
                self.convert(rk, k);
                let u = is_unsigned(k);
                let byte = match (vt(k), op) {
                    (0x7f, Eq) => 0x46,
                    (0x7f, Ne) => 0x47,
                    (0x7f, Lt) => if u { 0x49 } else { 0x48 },
                    (0x7f, Gt) => if u { 0x4b } else { 0x4a },
                    (0x7f, Le) => if u { 0x4d } else { 0x4c },
                    (0x7f, Ge) => if u { 0x4f } else { 0x4e },
                    (0x7e, Eq) => 0x51,
                    (0x7e, Ne) => 0x52,
                    (0x7e, Lt) => if u { 0x54 } else { 0x53 },
                    (0x7e, Gt) => if u { 0x56 } else { 0x55 },
                    (0x7e, Le) => if u { 0x58 } else { 0x57 },
                    (0x7e, Ge) => if u { 0x5a } else { 0x59 },
                    (0x7d, Eq) => 0x5b,
                    (0x7d, Ne) => 0x5c,
                    (0x7d, Lt) => 0x5d,
                    (0x7d, Gt) => 0x5e,
                    (0x7d, Le) => 0x5f,
                    (0x7d, Ge) => 0x60,
                    (_, Eq) => 0x61,
                    (_, Ne) => 0x62,
                    (_, Lt) => 0x63,
                    (_, Gt) => 0x64,
                    (_, Le) => 0x65,
                    (_, Ge) => 0x66,
                    _ => unreachable!(),
                };
                self.op(byte);
                Num::Int(IntKind::I32)
            }
            Add | Sub | Mul | Div | Rem | Shl | Shr | BitAnd | BitOr | BitXor => {
                let k = check::op_type_for(node)
                    .or_else(|| self.infer_here(l))
                    .or_else(|| self.infer_here(r))
                    .expect("qualified arith kind");
                let lk = self.expr(l);
                self.convert(lk, k);
                let rk = self.expr(r);
                self.convert(rk, k);
                match op {
                    Div if vt(k) == 0x7f && !is_unsigned(k) => self.div_guard_i32(k),
                    Div if vt(k) == 0x7e && !is_unsigned(k) => self.div_guard_i64(k),
                    Rem if vt(k) == 0x7f && !is_unsigned(k) => self.rem_guard_i32(k),
                    Rem if vt(k) == 0x7e && !is_unsigned(k) => self.rem_guard_i64(k),
                    _ => {
                        self.arith_op(op.as_str(), k);
                    }
                }
                k
            }
            _ => unreachable!("disqualified operator"),
        }
    }

    fn arith_op(&mut self, op: &str, k: Num) {
        let u = is_unsigned(k);
        let byte = match (vt(k), op) {
            (0x7f, "+") => 0x6a,
            (0x7f, "-") => 0x6b,
            (0x7f, "*") => 0x6c,
            (0x7f, "/") => if u { 0x6e } else { 0x6d },
            (0x7f, "%") => if u { 0x70 } else { 0x6f },
            (0x7f, "&") => 0x71,
            (0x7f, "|") => 0x72,
            (0x7f, "^") => 0x73,
            (0x7f, "<<") => 0x74,
            (0x7f, ">>") => if u { 0x76 } else { 0x75 },
            (0x7e, "+") => 0x7c,
            (0x7e, "-") => 0x7d,
            (0x7e, "*") => 0x7e,
            (0x7e, "/") => if u { 0x80 } else { 0x7f },
            (0x7e, "%") => if u { 0x82 } else { 0x81 },
            (0x7e, "&") => 0x83,
            (0x7e, "|") => 0x84,
            (0x7e, "^") => 0x85,
            (0x7e, "<<") => 0x86,
            (0x7e, ">>") => if u { 0x88 } else { 0x87 },
            (0x7d, "+") => 0x92,
            (0x7d, "-") => 0x93,
            (0x7d, "*") => 0x94,
            (0x7d, "/") => 0x95,
            (0x7c, "+") => 0xa0,
            (0x7c, "-") => 0xa1,
            (0x7c, "*") => 0xa2,
            (0x7c, "/") => 0xa3,
            _ => unreachable!("op {op} on {:?}", k),
        };
        self.op(byte);
        if matches!(op, "+" | "-" | "*" | "<<") {
            self.wrap(k);
        }
    }

    /// i32.div_s with the INT_MIN / -1 case answered (INT_MIN), not trapped.
    /// Division by zero still traps; the JS glue maps that to Mersey's
    /// RangeError.
    fn div_guard_i32(&mut self, k: Num) {
        let (lt, rt) = (self.scratch(k), self.scratch(k));
        self.set(rt);
        self.set(lt);
        self.get(lt);
        self.const_i(k, I32_MIN);
        self.op(0x46); // eq
        self.get(rt);
        self.const_i(k, -1);
        self.op(0x46);
        self.op(0x71); // and
        self.op(0x04);
        self.op(0x7f);
        self.cur_depth += 1;
        self.const_i(k, I32_MIN);
        self.op(0x05);
        self.get(lt);
        self.get(rt);
        self.op(0x6d); // div_s
        self.cur_depth -= 1;
        self.op(0x0b);
    }
    fn div_guard_i64(&mut self, k: Num) {
        let (lt, rt) = (self.scratch(k), self.scratch(k));
        self.set(rt);
        self.set(lt);
        self.get(lt);
        self.const_i(k, i64::MIN);
        self.op(0x51);
        self.get(rt);
        self.const_i(k, -1);
        self.op(0x51);
        self.op(0x71);
        self.op(0x04);
        self.op(0x7e);
        self.cur_depth += 1;
        self.const_i(k, i64::MIN);
        self.op(0x05);
        self.get(lt);
        self.get(rt);
        self.op(0x7f); // i64.div_s
        self.cur_depth -= 1;
        self.op(0x0b);
    }
    fn rem_guard_i32(&mut self, k: Num) {
        let (lt, rt) = (self.scratch(k), self.scratch(k));
        self.set(rt);
        self.set(lt);
        self.get(rt);
        self.const_i(k, -1);
        self.op(0x46);
        self.op(0x04);
        self.op(0x7f);
        self.cur_depth += 1;
        self.const_i(k, 0);
        self.op(0x05);
        self.get(lt);
        self.get(rt);
        self.op(0x6f); // rem_s
        self.cur_depth -= 1;
        self.op(0x0b);
    }
    fn rem_guard_i64(&mut self, k: Num) {
        let (lt, rt) = (self.scratch(k), self.scratch(k));
        self.set(rt);
        self.set(lt);
        self.get(rt);
        self.const_i(k, -1);
        self.op(0x51);
        self.op(0x04);
        self.op(0x7e);
        self.cur_depth += 1;
        self.const_i(k, 0);
        self.op(0x05);
        self.get(lt);
        self.get(rt);
        self.op(0x81); // rem_s
        self.cur_depth -= 1;
        self.op(0x0b);
    }
}
