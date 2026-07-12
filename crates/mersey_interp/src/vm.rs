//! Mersey Bytecode (MBC) Tier 0: a stack bytecode compiler + dispatch loop.
//!
//! Design (ROADMAP Phase 2, at MVP-engine scale): function bodies compile
//! to `Chunk`s executed by `run_chunk`; the object model, numeric tower,
//! member dispatch, and pattern binding are the *same* helpers the AST
//! tree-walker uses, so the two engines are observably identical — which
//! the differential conformance test enforces. Any construct the compiler
//! doesn't cover makes that one function fall back to AST execution
//! (`compile_fn` → `None`); semantics never depend on which tier runs.
//!
//! Known fallbacks: `try`+`finally` containing `return`/`break`/`continue`
//! (conservatively detected), `for await`, dynamic `import()`.
//!
//! Names resolve through environment scopes at runtime (CPython-style
//! Tier 0); register allocation is the JIT tier's job (Phase 4).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mersey_front::ast::*;
use mersey_front::diag::Pos;

use crate::{
    child_env, env_define, env_get, env_set, kind_of, parse_literal, to_display, Closure, Coro,
    Env, FnBody, FnData, Interp, Thrown, VResult, Value,
};

// ---- chunk -----------------------------------------------------------------

/// A monomorphic inline cache: one per member-access site.
///
/// Sealed shapes (§4.1) mean a class's field layout is fixed forever, so once
/// a site has seen a class the offset never changes. `class == 0` is empty.
#[derive(Clone, Copy, Default)]
pub(crate) struct ICache {
    pub(crate) class: u64,
    pub(crate) slot: u32,
}

pub struct Chunk {
    pub code: Vec<Op>,
    pub consts: Vec<Value>,
    pub names: Vec<String>,
    pub patterns: Vec<&'static Pattern>,
    pub types: Vec<&'static TypeExpr>,
    pub(crate) protos: Vec<Rc<FnData>>,
    /// Source position per instruction (parallel to `code`) — errors get a
    /// file:line:col instead of a bare message.
    pub positions: Vec<Pos>,
    /// The module this chunk came from.
    pub module: String,
    /// One inline cache per member-access site, filled on first execution.
    pub(crate) caches: Vec<std::cell::Cell<ICache>>,
}

impl Chunk {
    pub fn pos_at(&self, pc: usize) -> Pos {
        self.positions
            .get(pc)
            .copied()
            .unwrap_or(Pos { line: 0, col: 0 })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Const(u16),
    Null,
    LoadName(u16),
    StoreName(u16),
    DeclareName(u16),
    /// Pops a value, binds it through an AST pattern (defaults included).
    BindPattern(u16),
    LoadThis,
    PushScope,
    PopScope,
    Pop,
    Dup,
    Bin(BinOp),
    Un(UnaryOp),
    Truthy,
    Jump(usize),
    JumpIfFalse(usize),
    JumpIfTrue(usize),
    /// If TOS is null: keep it and jump. Else: fall through.
    OnNullJump(usize),
    /// If TOS is not null: keep it and jump. Else: pop and fall through.
    NotNullJump(usize),
    ToDisplayStr,
    /// Stack: [callee, a1..an]
    Call(u8),
    /// Stack: [callee, argsArray]
    CallV,
    /// Stack: [recv, a1..an]
    CallMethod(u16, u8),
    CallMethodV(u16),
    NewNamed(u16, u8),
    NewNamedV(u16),
    SuperCall(u8),
    SuperMember(u16),
    CallSuperMethod(u16, u8),
    /// `super(...args)` / `super.m(...args)`: the argument list arrives as an
    /// array, so a spread does not force the whole function to the slow tier.
    SuperCallV,
    CallSuperMethodV(u16),
    /// Dynamic `import(spec)`: pushes a promise of the module's exports.
    ImportCall(u16),
    GetMember(u16, u16),
    SetMember(u16, u16),
    IndexGet,
    IndexSet,
    MakeArray,
    ArrayPush1,
    ArraySpread,
    MakeRecord,
    RecordSetField(u16),
    RecordSpread,
    MakeClosure(u16),
    InstanceOf,
    CastOp(u16, bool),
    /// `x is T`: pops a value, pushes a bool.
    IsOp(u16),
    /// Pops an async iterable, pushes its async iterator.
    AsyncIterInit,
    /// Snapshot an iterable (array clone / string chars) for `for of`.
    IterArray,
    PushHandler(usize),
    PopHandler,
    /// Peeks the thrown value, pushes whether the catch type matches.
    CatchMatches(u16),
    Throw,
    /// Suspend the coroutine on the awaited value (async functions only).
    Await,
    /// Suspend a generator, handing the value to its consumer.
    YieldOp,
    Return,
    ReturnNull,
}

/// How a chunk stopped: with a value, or suspended.
pub enum Flow {
    Done(Value),
    Await(Value),
    /// A generator produced a value and is waiting to be resumed.
    Yield(Value),
}

// ---- public entry points ------------------------------------------------------

/// Compile a function body; `None` = unsupported construct, use the AST.
pub(crate) fn compile_fn(body: &FnBody) -> Option<Rc<Chunk>> {
    compile_fn_in(body, "")
}

pub(crate) fn compile_fn_in(body: &FnBody, module: &str) -> Option<Rc<Chunk>> {
    let mut c = C::new();
    c.module = module.to_string();
    match body {
        FnBody::Block(stmts) => {
            for s in *stmts {
                c.stmt(s);
            }
            c.emit(Op::ReturnNull);
        }
        FnBody::Expr(e) => {
            c.expr(e);
            c.emit(Op::Return);
        }
    }
    c.finish()
}

/// Does this compiled body contain a `yield`? (Generators must run on the
/// VM: only it can suspend.)
/// Can anything in this loop see its per-iteration binding? Only a closure can
/// (declarations are module-level, §6.7), so if the loop makes none, the fresh
/// binding is unobservable and both tiers skip it.
pub(crate) fn loop_captures(cond: &Option<Expr>, step: &[Expr], body: &Stmt) -> bool {
    use mersey_front::check::{expr_makes_closure, stmt_makes_closure};
    stmt_makes_closure(body)
        || cond.as_ref().is_some_and(expr_makes_closure)
        || step.iter().any(expr_makes_closure)
}

pub(crate) fn chunk_yields(chunk: &Chunk) -> bool {
    chunk.code.iter().any(|op| matches!(op, Op::YieldOp))
}

/// Does this chunk suspend on an `await`? A module whose top level does is
/// itself asynchronous, and has to run as a coroutine.
pub(crate) fn chunk_awaits(chunk: &Chunk) -> bool {
    chunk.code.iter().any(|op| matches!(op, Op::Await))
}

/// Public wrapper for tests/tools: compile a function body from its AST
/// statement list.
pub fn compile_fn_public(stmts: &'static [mersey_front::ast::Stmt]) -> Option<Rc<Chunk>> {
    compile_fn(&FnBody::Block(stmts))
}

/// Compile a module's top-level statements (including exported vars).
pub(crate) fn compile_module_stmts(module: &'static Module) -> Option<Rc<Chunk>> {
    compile_module_stmts_in(module, "")
}

pub(crate) fn compile_module_stmts_in(module: &'static Module, spec: &str) -> Option<Rc<Chunk>> {
    let mut c = C::new();
    c.module = spec.to_string();
    for item in &module.items {
        match item {
            Item::Stmt(s) => c.stmt(s),
            Item::Export(ExportDecl {
                kind: ExportKind::Var(v),
                ..
            }) => c.var_stmt(v),
            _ => {}
        }
    }
    c.emit(Op::ReturnNull);
    c.finish()
}

/// Human-readable disassembly for `mersey compile`.
pub fn listing(module: &'static Module) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut dump = |title: &str, chunk: Option<Rc<Chunk>>| match chunk {
        Some(ch) => {
            let _ = writeln!(out, "== {title} ({} ops)", ch.code.len());
            if let Err(e) = verify(&ch) {
                let _ = writeln!(out, "   VERIFY FAILED: {e}");
            }
            for (i, op) in ch.code.iter().enumerate() {
                let ann = annotate(&ch, op);
                let _ = writeln!(out, "{i:5}  {op:?}{ann}");
            }
        }
        None => {
            let _ = writeln!(out, "== {title}: AST fallback");
        }
    };
    dump("<top-level>", compile_module_stmts(module));
    for item in &module.items {
        let d = match item {
            Item::Decl(d)
            | Item::Export(ExportDecl {
                kind: ExportKind::Decl(d),
                ..
            }) => d,
            _ => continue,
        };
        match d {
            Decl::Function(f) => {
                dump(&f.name.text, compile_fn(&FnBody::Block(&f.body)));
            }
            Decl::Class(cl) => {
                for m in &cl.members {
                    match m {
                        ClassMember::Method {
                            name,
                            body: Some(b),
                            ..
                        } => {
                            dump(
                                &format!("{}.{name}", cl.name.text),
                                compile_fn(&FnBody::Block(b)),
                            );
                        }
                        ClassMember::Ctor { body, .. } => {
                            dump(
                                &format!("{}.constructor", cl.name.text),
                                compile_fn(&FnBody::Block(body)),
                            );
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn annotate(ch: &Chunk, op: &Op) -> String {
    match op {
        Op::Const(i) => format!("        ; {}", to_display(&ch.consts[*i as usize])),
        Op::LoadName(i)
        | Op::StoreName(i)
        | Op::DeclareName(i)
        | Op::CallSuperMethodV(i)
        | Op::GetMember(i, _)
        | Op::SetMember(i, _)
        | Op::CallMethod(i, _)
        | Op::CallMethodV(i)
        | Op::NewNamed(i, _)
        | Op::NewNamedV(i)
        | Op::SuperMember(i)
        | Op::CallSuperMethod(i, _)
        | Op::RecordSetField(i) => {
            format!("        ; {}", ch.names[*i as usize])
        }
        _ => String::new(),
    }
}

// ---- compiler --------------------------------------------------------------------

enum CtxKind {
    Loop,
    Switch,
}

struct LoopCtx {
    kind: CtxKind,
    label: Option<String>,
    scope_depth: usize,
    /// How many `finally` blocks were pending when this loop began. A `break`
    /// out of the loop must run every `finally` entered *since* — and only
    /// those.
    finally_depth: usize,
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

/// A `finally` block that a `return`, `break` or `continue` has to run on its
/// way out of the `try` it belongs to.
#[derive(Clone)]
struct FinallyCtx {
    stmts: &'static [Stmt],
    /// Scope depth *outside* the `try`: the finally body must not see the try
    /// block's locals, so those scopes are popped before it runs.
    scope_depth: usize,
    /// Handlers to pop before running it. If the finally itself throws, that
    /// throw must not be caught by the very `try` it belongs to.
    handlers: usize,
}

struct C {
    code: Vec<Op>,
    positions: Vec<Pos>,
    cur_pos: Pos,
    consts: Vec<Value>,
    names: Vec<String>,
    patterns: Vec<&'static Pattern>,
    types: Vec<&'static TypeExpr>,
    protos: Vec<Rc<FnData>>,
    loops: Vec<LoopCtx>,
    /// `finally` blocks currently enclosing the code being compiled.
    finallys: Vec<FinallyCtx>,
    scope_depth: usize,
    temp: u32,
    labeled_next: Option<String>,
    module: String,
    /// Number of inline-cache slots reserved so far.
    n_caches: u16,
    ok: bool,
}

impl C {
    fn new() -> C {
        C {
            code: vec![],
            n_caches: 0,
            positions: vec![],
            cur_pos: Pos { line: 0, col: 0 },
            consts: vec![],
            names: vec![],
            patterns: vec![],
            types: vec![],
            protos: vec![],
            loops: vec![],
            finallys: vec![],
            scope_depth: 0,
            temp: 0,
            labeled_next: None,
            module: String::new(),
            ok: true,
        }
    }

    fn finish(self) -> Option<Rc<Chunk>> {
        if !self.ok {
            return None;
        }
        let chunk = Chunk {
            code: self.code,
            consts: self.consts,
            names: self.names,
            patterns: self.patterns,
            types: self.types,
            protos: self.protos,
            positions: self.positions,
            module: self.module,
            caches: (0..self.n_caches)
                .map(|_| std::cell::Cell::new(ICache::default()))
                .collect(),
        };
        debug_assert!(verify(&chunk).is_ok(), "verifier: {:?}", verify(&chunk));
        Some(Rc::new(chunk))
    }

    fn bail(&mut self) {
        self.ok = false;
    }

    /// Reserve a fresh cache slot: caches are per *site*, not per name, so a
    /// megamorphic site elsewhere cannot poison a monomorphic one here.
    fn new_cache(&mut self) -> u16 {
        let i = self.n_caches;
        self.n_caches += 1;
        i
    }

    fn emit_get_member(&mut self, ni: u16) -> usize {
        let c = self.new_cache();
        self.emit(Op::GetMember(ni, c))
    }

    fn emit_set_member(&mut self, ni: u16) -> usize {
        let c = self.new_cache();
        self.emit(Op::SetMember(ni, c))
    }

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.positions.push(self.cur_pos);
        self.code.len() - 1
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn patch(&mut self, at: usize, target: usize) {
        match &mut self.code[at] {
            Op::Jump(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfTrue(t)
            | Op::OnNullJump(t)
            | Op::NotNullJump(t)
            | Op::PushHandler(t) => *t = target,
            other => unreachable!("patch on {other:?}"),
        }
    }

    fn name(&mut self, s: &str) -> u16 {
        if let Some(i) = self.names.iter().position(|n| n == s) {
            return i as u16;
        }
        self.names.push(s.to_string());
        (self.names.len() - 1) as u16
    }

    fn konst(&mut self, v: Value) -> u16 {
        self.consts.push(v);
        (self.consts.len() - 1) as u16
    }

    fn fresh_temp(&mut self, tag: &str) -> String {
        self.temp += 1;
        format!("#{tag}{}", self.temp)
    }

    // ---- statements ---------------------------------------------------------

    fn stmt(&mut self, s: &'static Stmt) {
        if !self.ok {
            return;
        }
        if let Some(p) = stmt_pos(s) {
            self.cur_pos = p;
        }
        match s {
            Stmt::Block(b) => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                for s in b {
                    self.stmt(s);
                }
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Stmt::Var(v) => self.var_stmt(v),
            Stmt::Expr(e) => {
                self.expr(e);
                self.emit(Op::Pop);
            }
            Stmt::Empty => {}
            Stmt::If { cond, then, els } => {
                self.expr(cond);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.stmt(then);
                match els {
                    Some(e) => {
                        let jend = self.emit(Op::Jump(0));
                        let else_pc = self.here();
                        self.patch(jf, else_pc);
                        self.stmt(e);
                        let end = self.here();
                        self.patch(jend, end);
                    }
                    None => {
                        let end = self.here();
                        self.patch(jf, end);
                    }
                }
            }
            Stmt::While { cond, body } => self.compile_loop(None, |c| {
                let start = c.here();
                c.expr(cond);
                let jf = c.emit(Op::JumpIfFalse(0));
                c.stmt(body);
                (start, start, vec![jf])
            }),
            Stmt::DoWhile { body, cond } => self.compile_loop(None, |c| {
                let start = c.here();
                c.stmt(body);
                let cont = c.here();
                c.expr(cond);
                c.emit(Op::JumpIfTrue(start));
                (usize::MAX, cont, vec![]) // no back-jump needed
            }),
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                // `for (let i = …)` gives every iteration its own `i`, so a
                // closure made in the body captures the value it saw rather
                // than the one the loop finished with — the reason `let` exists
                // in a loop head at all.
                //
                // The update runs in the *next* iteration's scope, not this
                // one: otherwise the closure just made would see the
                // incremented value, which is the bug this feature exists to
                // prevent.
                let mut per_iteration: Vec<u16> = Vec::new();
                match init {
                    Some(ForInit::Var(v)) => {
                        self.var_stmt(v);
                        // Only when something can actually capture the
                        // binding: otherwise this is an ordinary counted loop
                        // and should stay one (no scope per iteration, and it
                        // stays inside the JIT's subset).
                        if v.kind == VarKind::Let && loop_captures(cond, step, body) {
                            let mut names: Vec<String> = Vec::new();
                            for b in &v.bindings {
                                crate::pattern_names_of(&b.target, &mut names);
                            }
                            per_iteration = names.iter().map(|n| self.name(n)).collect();
                        }
                    }
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.expr(e);
                            self.emit(Op::Pop);
                        }
                    }
                    None => {}
                }
                let fresh = !per_iteration.is_empty();
                if fresh {
                    // The first iteration's scope, seeded from the loop head.
                    self.emit(Op::PushScope);
                    self.scope_depth += 1;
                    for n in &per_iteration {
                        self.emit(Op::LoadName(*n));
                        self.emit(Op::DeclareName(*n));
                    }
                }
                self.compile_loop(None, |c| {
                    let start = c.here();
                    let jf = match cond {
                        Some(cond) => {
                            c.expr(cond);
                            vec![c.emit(Op::JumpIfFalse(0))]
                        }
                        None => vec![],
                    };
                    c.stmt(body);
                    let cont = c.here();
                    if fresh {
                        // Hand this iteration's values to the next scope: they
                        // ride the operand stack across the scope swap.
                        for n in &per_iteration {
                            c.emit(Op::LoadName(*n));
                        }
                        c.emit(Op::PopScope);
                        c.emit(Op::PushScope);
                        for n in per_iteration.iter().rev() {
                            c.emit(Op::DeclareName(*n));
                        }
                    }
                    for e in step {
                        c.expr(e);
                        c.emit(Op::Pop);
                    }
                    (start, cont, jf)
                });
                if fresh {
                    // Every exit — falling out, `break`, a false condition —
                    // leaves the current iteration scope to pop.
                    self.emit(Op::PopScope);
                    self.scope_depth -= 1;
                }
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Stmt::ForOf {
                is_await,
                target,
                iter,
                body,
                ..
            } => {
                if *is_await {
                    // `for await (const x of gen())` is the loop form of
                    // `await`: pull a promise from the async iterator, await it,
                    // stop at `null`. It compiles to exactly that, so it needs
                    // no machinery of its own and suspends like any other await.
                    self.emit(Op::PushScope);
                    self.scope_depth += 1;
                    let ait = self.fresh_temp("ait");
                    let item = self.fresh_temp("aitem");
                    let n_ait = self.name(&ait);
                    let n_item = self.name(&item);
                    let n_next = self.name("next");
                    self.expr(iter);
                    // A class implementing `AsyncIterable<T>` hands over its
                    // iterator here; an `AsyncIter` is already one.
                    self.emit(Op::AsyncIterInit);
                    self.emit(Op::DeclareName(n_ait));
                    self.emit(Op::Null);
                    self.emit(Op::DeclareName(n_item));

                    self.compile_loop(None, |c| {
                        let start = c.here();
                        c.emit(Op::LoadName(n_ait));
                        c.emit(Op::CallMethod(n_next, 0));
                        c.emit(Op::Await);
                        c.emit(Op::StoreName(n_item));
                        // `null` ends the sequence, exactly as it does for a
                        // synchronous generator's `next()`.
                        c.emit(Op::LoadName(n_item));
                        c.emit(Op::Null);
                        c.emit(Op::Bin(BinOp::Ne));
                        let jf = vec![c.emit(Op::JumpIfFalse(0))];

                        c.emit(Op::PushScope);
                        c.scope_depth += 1;
                        c.emit(Op::LoadName(n_item));
                        c.bind_target(target);
                        c.stmt(body);
                        let cont = c.here();
                        c.emit(Op::PopScope);
                        c.scope_depth -= 1;
                        (start, cont, jf)
                    });
                    self.emit(Op::PopScope);
                    self.scope_depth -= 1;
                    return;
                }
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let items = self.fresh_temp("it");
                let idx = self.fresh_temp("ix");
                self.expr(iter);
                self.emit(Op::IterArray);
                let n_items = self.name(&items);
                self.emit(Op::DeclareName(n_items));
                let zero = self.konst(Value::I32(0));
                self.emit(Op::Const(zero));
                let n_idx = self.name(&idx);
                self.emit(Op::DeclareName(n_idx));
                self.compile_loop(None, |c| {
                    let start = c.here();
                    c.emit(Op::LoadName(n_idx));
                    c.emit(Op::LoadName(n_items));
                    let len = c.name("length");
                    c.emit_get_member(len);
                    c.emit(Op::Bin(BinOp::Lt));
                    let jf = c.emit(Op::JumpIfFalse(0));
                    c.emit(Op::PushScope);
                    c.scope_depth += 1;
                    c.emit(Op::LoadName(n_items));
                    c.emit(Op::LoadName(n_idx));
                    c.emit(Op::IndexGet);
                    c.bind_target(target);
                    c.stmt(body);
                    c.scope_depth -= 1;
                    c.emit(Op::PopScope);
                    let cont = c.here();
                    c.emit(Op::LoadName(n_idx));
                    let one = c.konst(Value::I32(1));
                    c.emit(Op::Const(one));
                    c.emit(Op::Bin(BinOp::Add));
                    c.emit(Op::StoreName(n_idx));
                    (start, cont, vec![jf])
                });
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Stmt::Switch { scrutinee, clauses } => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let sw = self.fresh_temp("sw");
                self.expr(scrutinee);
                let n_sw = self.name(&sw);
                self.emit(Op::DeclareName(n_sw));
                self.loops.push(LoopCtx {
                    kind: CtxKind::Switch,
                    label: None,
                    scope_depth: self.scope_depth,
                    finally_depth: self.finallys.len(),
                    breaks: vec![],
                    continues: vec![],
                });
                let mut body_jumps: Vec<(usize, usize)> = vec![]; // (jump pc, clause idx)
                for (i, cl) in clauses.iter().enumerate() {
                    if let Some(t) = &cl.test {
                        self.emit(Op::LoadName(n_sw));
                        self.expr(t);
                        self.emit(Op::Bin(BinOp::Eq));
                        let j = self.emit(Op::JumpIfTrue(0));
                        body_jumps.push((j, i));
                    }
                }
                let default_jump = self.emit(Op::Jump(0));
                let default_idx = clauses.iter().position(|c| c.test.is_none());
                let mut body_pcs = vec![0usize; clauses.len()];
                for (i, cl) in clauses.iter().enumerate() {
                    body_pcs[i] = self.here();
                    for s in &cl.body {
                        self.stmt(s);
                    }
                }
                let end = self.here();
                for (j, i) in body_jumps {
                    self.patch(j, body_pcs[i]);
                }
                match default_idx {
                    Some(i) => self.patch(default_jump, body_pcs[i]),
                    None => self.patch(default_jump, end),
                }
                let ctx = self.loops.pop().expect("switch ctx");
                for b in ctx.breaks {
                    self.patch(b, end);
                }
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Stmt::Break { label, .. } => self.abrupt(label.as_ref(), true),
            Stmt::Continue { label, .. } => self.abrupt(label.as_ref(), false),
            Stmt::Return { value, .. } => {
                match value {
                    Some(e) => {
                        // The value is computed first and rides the operand
                        // stack while the `finally` blocks run — a `finally`
                        // cannot change what was already returned.
                        self.expr(e);
                        self.run_finallys(0);
                        self.emit(Op::Return);
                    }
                    None => {
                        self.run_finallys(0);
                        self.emit(Op::ReturnNull);
                    }
                };
            }
            Stmt::Throw(e) => {
                self.expr(e);
                self.emit(Op::Throw);
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                let outer = finally.as_ref().map(|_| self.emit(Op::PushHandler(0)));
                let inner = self.emit(Op::PushHandler(0));
                // Inside the try block both handlers are live, so an abrupt exit
                // from here pops two.
                if let Some(f) = finally {
                    self.finallys.push(FinallyCtx {
                        stmts: f,
                        scope_depth: self.scope_depth,
                        handlers: 2,
                    });
                }
                self.compile_block_stmts(block);
                self.emit(Op::PopHandler);
                let mut norm_jumps = vec![self.emit(Op::Jump(0))];
                let handler_pc = self.here();
                self.patch(inner, handler_pc);
                // Dispatching to a handler pops it, so inside a catch block only
                // the outer (finally) handler is still live.
                if let Some(ctx) = self.finallys.last_mut() {
                    if finally.is_some() {
                        ctx.handlers = 1;
                    }
                }
                for c in catches {
                    let ty = self.types.len() as u16;
                    self.types.push(&c.ty);
                    self.emit(Op::CatchMatches(ty));
                    let skip = self.emit(Op::JumpIfFalse(0));
                    self.emit(Op::PushScope);
                    self.scope_depth += 1;
                    let n = self.name(&c.name.text);
                    self.emit(Op::DeclareName(n)); // pops the thrown value
                    for s in &c.block {
                        self.stmt(s);
                    }
                    self.scope_depth -= 1;
                    self.emit(Op::PopScope);
                    norm_jumps.push(self.emit(Op::Jump(0)));
                    let next = self.here();
                    self.patch(skip, next);
                }
                self.emit(Op::Throw); // no catch matched: rethrow
                let norm = self.here();
                for j in norm_jumps {
                    self.patch(j, norm);
                }
                if finally.is_some() {
                    self.finallys.pop();
                }
                if let Some(f) = finally {
                    self.emit(Op::PopHandler); // outer
                    self.compile_block_stmts(f);
                    let done = self.emit(Op::Jump(0));
                    let fh = self.here();
                    self.patch(outer.expect("outer handler"), fh);
                    self.compile_block_stmts(f);
                    self.emit(Op::Throw);
                    let end = self.here();
                    self.patch(done, end);
                }
            }
            Stmt::Labeled { label, body } => match body.as_ref() {
                Stmt::While { .. }
                | Stmt::DoWhile { .. }
                | Stmt::For { .. }
                | Stmt::ForOf { .. } => {
                    self.labeled_next = Some(label.text.clone());
                    self.stmt(body);
                }
                _ => self.stmt(body),
            },
        }
    }

    fn compile_block_stmts(&mut self, stmts: &'static [Stmt]) {
        self.emit(Op::PushScope);
        self.scope_depth += 1;
        for s in stmts {
            self.stmt(s);
        }
        self.scope_depth -= 1;
        self.emit(Op::PopScope);
    }

    /// Shared loop scaffolding: `f` compiles condition + body and returns
    /// (back-jump target or MAX, continue target, cond-exit jumps).
    fn compile_loop(
        &mut self,
        _unused: Option<()>,
        f: impl FnOnce(&mut C) -> (usize, usize, Vec<usize>),
    ) {
        let label = self.labeled_next.take();
        self.loops.push(LoopCtx {
            kind: CtxKind::Loop,
            label,
            scope_depth: self.scope_depth,
            finally_depth: self.finallys.len(),
            breaks: vec![],
            continues: vec![],
        });
        let (back, cont, exits) = f(self);
        if back != usize::MAX {
            self.emit(Op::Jump(back));
        }
        let end = self.here();
        for j in exits {
            self.patch(j, end);
        }
        let ctx = self.loops.pop().expect("loop ctx");
        for b in ctx.breaks {
            self.patch(b, end);
        }
        for c in ctx.continues {
            self.patch(c, cont);
        }
    }

    /// Emit the `finally` blocks between here and `target_fin`, innermost
    /// first, and return the scope depth left behind.
    ///
    /// A `finally` is *duplicated* at each exit that crosses it rather than
    /// jumped to, because each exit has to carry on doing something different
    /// afterwards (return this value, break that loop). Before each copy: pop
    /// the try block's scopes, so the finally cannot see its locals; then pop
    /// that try's handlers, so a throw *inside* the finally is not caught by the
    /// very `try` it belongs to.
    fn run_finallys(&mut self, target_fin: usize) -> usize {
        let mut cur = self.scope_depth;
        for i in (target_fin..self.finallys.len()).rev() {
            let f = self.finallys[i].clone();
            while cur > f.scope_depth {
                self.emit(Op::PopScope);
                cur -= 1;
            }
            for _ in 0..f.handlers {
                self.emit(Op::PopHandler);
            }
            // While compiling the copy, this finally is no longer "pending":
            // an abrupt exit inside it must not try to run it again.
            let stashed = self.finallys.split_off(i);
            let saved_depth = self.scope_depth;
            self.scope_depth = f.scope_depth;
            self.compile_block_stmts(f.stmts);
            self.scope_depth = saved_depth;
            self.finallys.extend(stashed);
        }
        cur
    }

    fn abrupt(&mut self, label: Option<&Name>, is_break: bool) {
        let idx = self.loops.iter().rposition(|ctx| match (label, is_break) {
            (Some(l), _) => ctx.label.as_deref() == Some(l.text.as_str()),
            (None, true) => true,
            (None, false) => matches!(ctx.kind, CtxKind::Loop),
        });
        let Some(idx) = idx else { return self.bail() };
        let target_depth = self.loops[idx].scope_depth;
        let target_fin = self.loops[idx].finally_depth;
        let mut cur = self.run_finallys(target_fin);
        while cur > target_depth {
            self.emit(Op::PopScope);
            cur -= 1;
        }
        let j = self.emit(Op::Jump(0));
        if is_break {
            self.loops[idx].breaks.push(j);
        } else {
            self.loops[idx].continues.push(j);
        }
    }

    fn var_stmt(&mut self, v: &'static VarStmt) {
        for b in &v.bindings {
            match &b.init {
                Some(e) => self.expr(e),
                None => {
                    self.emit(Op::Null);
                }
            }
            self.bind_target(&b.target);
        }
    }

    /// Pops TOS, binds it to the pattern.
    fn bind_target(&mut self, p: &'static Pattern) {
        match p {
            Pattern::Name(n) => {
                let i = self.name(&n.text);
                self.emit(Op::DeclareName(i));
            }
            other => {
                let i = self.patterns.len() as u16;
                self.patterns.push(other);
                self.emit(Op::BindPattern(i));
            }
        }
    }

    // ---- expressions ---------------------------------------------------------

    fn expr(&mut self, e: &'static Expr) {
        if !self.ok {
            return;
        }
        let saved = self.cur_pos;
        if let Some(p) = expr_pos(e) {
            self.cur_pos = p;
        }
        self.expr_inner(e);
        self.cur_pos = saved;
    }

    fn expr_inner(&mut self, e: &'static Expr) {
        match e {
            Expr::Ident(n) => {
                let i = self.name(&n.text);
                self.emit(Op::LoadName(i));
            }
            Expr::This(_) => {
                self.emit(Op::LoadThis);
            }
            Expr::Lit { kind, text, .. } => match parse_literal(*kind, text) {
                Ok(v) => {
                    let i = self.konst(v);
                    self.emit(Op::Const(i));
                }
                Err(_) => self.bail(), // AST tier reports the proper error
            },
            Expr::Template(parts) => {
                let mut first = true;
                for p in parts {
                    match p {
                        TplPart::Text(t) => {
                            let v = Value::Str(Rc::new(
                                crate::unescape(t).chars().collect::<Vec<char>>(),
                            ));
                            let i = self.konst(v);
                            self.emit(Op::Const(i));
                        }
                        TplPart::Expr(e) => {
                            self.expr(e);
                            self.emit(Op::ToDisplayStr);
                        }
                    }
                    if !first {
                        self.emit(Op::Bin(BinOp::Add));
                    }
                    first = false;
                }
            }
            Expr::Array(elems) => {
                self.emit(Op::MakeArray);
                for el in elems {
                    self.expr(&el.expr);
                    self.emit(if el.spread {
                        Op::ArraySpread
                    } else {
                        Op::ArrayPush1
                    });
                }
            }
            Expr::Record(fields) => {
                self.emit(Op::MakeRecord);
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            match value {
                                Some(v) => self.expr(v),
                                None => {
                                    let i = self.name(&name.text);
                                    self.emit(Op::LoadName(i));
                                }
                            }
                            let i = self.name(&name.text);
                            self.emit(Op::RecordSetField(i));
                        }
                        RecordField::Spread(v) => {
                            self.expr(v);
                            self.emit(Op::RecordSpread);
                        }
                    }
                }
            }
            Expr::Paren(inner) => self.expr(inner),
            Expr::Arrow {
                is_async,
                params,
                body,
                ..
            } => {
                let data = Rc::new(FnData::new(
                    "<arrow>".to_string(),
                    *is_async,
                    params,
                    match body {
                        ArrowBody::Expr(e) => FnBody::Expr(e),
                        ArrowBody::Block(b) => FnBody::Block(b),
                    },
                ));
                let i = self.protos.len() as u16;
                self.protos.push(data);
                self.emit(Op::MakeClosure(i));
            }
            Expr::Unary { op, expr, .. } => {
                // `-2147483648` is one literal, not a negation of one — the
                // positive half does not fit int32, so folding is the only way
                // to write the minimum.
                if let (
                    UnaryOp::Neg,
                    Expr::Lit {
                        kind: LitKind::Int,
                        text,
                        ..
                    },
                ) = (op, &**expr)
                {
                    match crate::negated_int_literal(text) {
                        Ok(v) => {
                            let i = self.konst(v);
                            self.emit(Op::Const(i));
                            return;
                        }
                        Err(_) => {
                            // Out of range: let the ordinary path throw it at
                            // runtime, with the position it happened at.
                            self.bail();
                            return;
                        }
                    }
                }
                self.expr(expr);
                if *op == UnaryOp::Await {
                    self.emit(Op::Await);
                } else {
                    self.emit(Op::Un(*op));
                }
            }
            Expr::Update { prefix, inc, expr } => self.update(*prefix, *inc, expr),
            Expr::Binary { op, l, r } => match op {
                BinOp::And => {
                    self.expr(l);
                    self.emit(Op::Truthy);
                    self.emit(Op::Dup);
                    let j = self.emit(Op::JumpIfFalse(0));
                    self.emit(Op::Pop);
                    self.expr(r);
                    self.emit(Op::Truthy);
                    let end = self.here();
                    self.patch(j, end);
                }
                BinOp::Or => {
                    self.expr(l);
                    self.emit(Op::Truthy);
                    self.emit(Op::Dup);
                    let j = self.emit(Op::JumpIfTrue(0));
                    self.emit(Op::Pop);
                    self.expr(r);
                    self.emit(Op::Truthy);
                    let end = self.here();
                    self.patch(j, end);
                }
                BinOp::Coalesce => {
                    self.expr(l);
                    let j = self.emit(Op::NotNullJump(0));
                    self.expr(r);
                    let end = self.here();
                    self.patch(j, end);
                }
                BinOp::Instanceof => {
                    self.expr(l);
                    self.expr(r);
                    self.emit(Op::InstanceOf);
                }
                _ => {
                    self.expr(l);
                    self.expr(r);
                    self.emit(Op::Bin(*op));
                }
            },
            Expr::Assign { op, target, value } => self.assign(op, target, value),
            Expr::Cond { cond, then, els } => {
                self.expr(cond);
                let jf = self.emit(Op::JumpIfFalse(0));
                self.expr(then);
                let jend = self.emit(Op::Jump(0));
                let e_pc = self.here();
                self.patch(jf, e_pc);
                self.expr(els);
                let end = self.here();
                self.patch(jend, end);
            }
            Expr::Cast { expr, wrapping, ty } => {
                self.expr(expr);
                let i = self.types.len() as u16;
                self.types.push(ty);
                self.emit(Op::CastOp(i, *wrapping));
            }
            Expr::Is { expr, ty } => {
                self.expr(expr);
                let i = self.types.len() as u16;
                self.types.push(ty);
                self.emit(Op::IsOp(i));
            }
            Expr::Call {
                callee,
                args,
                optional,
                ..
            } => self.call(callee, args, *optional),
            Expr::New { ty, args } => {
                let TypeExpr::Named { name, .. } = ty else {
                    return self.bail();
                };
                // Keep the full path: `new geo.Point(…)` resolves through a
                // namespace import at runtime.
                let n = self.name(name);
                match self.args_fixed(args) {
                    Some(argc) => {
                        self.emit(Op::NewNamed(n, argc));
                    }
                    None => {
                        self.args_array(args);
                        self.emit(Op::NewNamedV(n));
                    }
                }
            }
            Expr::Member {
                obj,
                name,
                optional,
            } => {
                self.expr(obj);
                let i = self.name(name);
                if *optional {
                    let j = self.emit(Op::OnNullJump(0));
                    self.emit_get_member(i);
                    let end = self.here();
                    self.patch(j, end);
                } else {
                    self.emit_get_member(i);
                }
            }
            Expr::Index {
                obj,
                index,
                optional,
            } => {
                self.expr(obj);
                if *optional {
                    let j = self.emit(Op::OnNullJump(0));
                    self.expr(index);
                    self.emit(Op::IndexGet);
                    let end = self.here();
                    self.patch(j, end);
                } else {
                    self.expr(index);
                    self.emit(Op::IndexGet);
                }
            }
            Expr::SuperMember { name, .. } => {
                let i = self.name(name);
                self.emit(Op::SuperMember(i));
            }
            Expr::SuperCall { args, .. } => match self.args_fixed(args) {
                Some(argc) => {
                    self.emit(Op::SuperCall(argc));
                }
                // A spread argument list arrives as an array, so `super(...xs)`
                // no longer drops the whole function to the AST tier.
                None => {
                    self.args_array(args);
                    self.emit(Op::SuperCallV);
                }
            },
            Expr::ImportCall(inner) => {
                // The specifier is a literal (the checker enforces it, §4.5), so
                // it is a constant here.
                match &**inner {
                    Expr::Lit {
                        kind: LitKind::Str,
                        text,
                        ..
                    } => {
                        let spec = mersey_front::ast::string_value(text);
                        let n = self.name(&spec);
                        self.emit(Op::ImportCall(n));
                    }
                    _ => self.bail(),
                }
            }
            Expr::Yield { value, .. } => {
                match value {
                    Some(v) => self.expr(v),
                    None => {
                        self.emit(Op::Null);
                    }
                }
                self.emit(Op::YieldOp);
            }
        }
    }

    /// Fixed (spread-free) arguments compiled onto the stack; None = spread.
    fn args_fixed(&mut self, args: &'static [ArrayElem]) -> Option<u8> {
        if args.iter().any(|a| a.spread) || args.len() > u8::MAX as usize {
            return None;
        }
        for a in args {
            self.expr(&a.expr);
        }
        Some(args.len() as u8)
    }

    fn args_array(&mut self, args: &'static [ArrayElem]) {
        self.emit(Op::MakeArray);
        for a in args {
            self.expr(&a.expr);
            self.emit(if a.spread {
                Op::ArraySpread
            } else {
                Op::ArrayPush1
            });
        }
    }

    fn call(&mut self, callee: &'static Expr, args: &'static [ArrayElem], optional: bool) {
        if let Expr::Member {
            obj,
            name,
            optional: mopt,
        } = callee
        {
            self.expr(obj);
            let jnull = if *mopt || optional {
                Some(self.emit(Op::OnNullJump(0)))
            } else {
                None
            };
            let n = self.name(name);
            match self.args_fixed(args) {
                Some(argc) => {
                    self.emit(Op::CallMethod(n, argc));
                }
                None => {
                    self.args_array(args);
                    self.emit(Op::CallMethodV(n));
                }
            }
            if let Some(j) = jnull {
                let end = self.here();
                self.patch(j, end);
            }
            return;
        }
        if let Expr::SuperMember { name, .. } = callee {
            let n = self.name(name);
            match self.args_fixed(args) {
                Some(argc) => {
                    self.emit(Op::CallSuperMethod(n, argc));
                }
                None => self.bail(),
            }
            return;
        }
        self.expr(callee);
        let jnull = if optional {
            Some(self.emit(Op::OnNullJump(0)))
        } else {
            None
        };
        match self.args_fixed(args) {
            Some(argc) => {
                self.emit(Op::Call(argc));
            }
            None => {
                self.args_array(args);
                self.emit(Op::CallV);
            }
        }
        if let Some(j) = jnull {
            let end = self.here();
            self.patch(j, end);
        }
    }

    fn update(&mut self, prefix: bool, inc: bool, target: &'static Expr) {
        let op = if inc { BinOp::Add } else { BinOp::Sub };
        let one = self.konst(Value::I32(1));
        match target {
            Expr::Ident(n) => {
                let i = self.name(&n.text);
                self.emit(Op::LoadName(i));
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(Op::Const(one));
                self.emit(Op::Bin(op));
                if prefix {
                    self.emit(Op::Dup);
                }
                self.emit(Op::StoreName(i));
            }
            Expr::Member { obj, name, .. } => {
                // PushScope; #o = obj; result juggling via temps.
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let o = self.fresh_temp("u");
                let no = self.name(&o);
                self.expr(obj);
                self.emit(Op::DeclareName(no));
                let nm = self.name(name);
                self.emit(Op::LoadName(no));
                self.emit_get_member(nm);
                if !prefix {
                    self.emit(Op::Dup); // old kept as result
                }
                self.emit(Op::Const(one));
                self.emit(Op::Bin(op));
                if prefix {
                    self.emit(Op::Dup);
                }
                // stack: [result, new] — store new via temp
                let v = self.fresh_temp("v");
                let nv = self.name(&v);
                self.emit(Op::DeclareName(nv));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nv));
                self.emit_set_member(nm);
                self.emit(Op::Pop);
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Expr::Index { obj, index, .. } => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let (o, ix) = (self.fresh_temp("u"), self.fresh_temp("x"));
                let (no, nx) = (self.name(&o), self.name(&ix));
                self.expr(obj);
                self.emit(Op::DeclareName(no));
                self.expr(index);
                self.emit(Op::DeclareName(nx));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nx));
                self.emit(Op::IndexGet);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(Op::Const(one));
                self.emit(Op::Bin(op));
                if prefix {
                    self.emit(Op::Dup);
                }
                let v = self.fresh_temp("v");
                let nv = self.name(&v);
                self.emit(Op::DeclareName(nv));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nx));
                self.emit(Op::LoadName(nv));
                self.emit(Op::IndexSet);
                self.emit(Op::Pop);
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            _ => self.bail(),
        }
    }

    fn assign(&mut self, op: &'static str, target: &'static Expr, value: &'static Expr) {
        if op == "=" {
            match target {
                Expr::Ident(n) => {
                    self.expr(value);
                    self.emit(Op::Dup);
                    let i = self.name(&n.text);
                    self.emit(Op::StoreName(i));
                }
                Expr::Member { obj, name, .. } => {
                    self.expr(obj);
                    self.expr(value);
                    let i = self.name(name);
                    self.emit_set_member(i);
                }
                Expr::Index { obj, index, .. } => {
                    self.expr(obj);
                    self.expr(index);
                    self.expr(value);
                    self.emit(Op::IndexSet);
                }
                _ => self.bail(),
            }
            return;
        }
        // Compound / logical assignment.
        let bin = match op {
            "+=" => Some(BinOp::Add),
            "-=" => Some(BinOp::Sub),
            "*=" => Some(BinOp::Mul),
            "/=" => Some(BinOp::Div),
            "%=" => Some(BinOp::Rem),
            "**=" => Some(BinOp::Pow),
            "<<=" => Some(BinOp::Shl),
            ">>=" => Some(BinOp::Shr),
            "&=" => Some(BinOp::BitAnd),
            "|=" => Some(BinOp::BitOr),
            "^=" => Some(BinOp::BitXor),
            _ => None, // &&= ||= ??=
        };
        match target {
            Expr::Ident(n) => {
                let i = self.name(&n.text);
                self.emit(Op::LoadName(i));
                match (bin, op) {
                    (Some(b), _) => {
                        self.expr(value);
                        self.emit(Op::Bin(b));
                        self.emit(Op::Dup);
                        self.emit(Op::StoreName(i));
                    }
                    (None, "??=") => {
                        let j = self.emit(Op::NotNullJump(0));
                        self.expr(value);
                        self.emit(Op::Dup);
                        self.emit(Op::StoreName(i));
                        let end = self.here();
                        self.patch(j, end);
                    }
                    (None, "&&=") | (None, "||=") => {
                        self.emit(Op::Dup);
                        self.emit(Op::Truthy);
                        let j = if op == "&&=" {
                            self.emit(Op::JumpIfFalse(0))
                        } else {
                            self.emit(Op::JumpIfTrue(0))
                        };
                        self.emit(Op::Pop);
                        self.expr(value);
                        self.emit(Op::Dup);
                        self.emit(Op::StoreName(i));
                        let end = self.here();
                        self.patch(j, end);
                    }
                    _ => self.bail(),
                }
            }
            Expr::Member { obj, name, .. } if bin.is_some() => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let o = self.fresh_temp("a");
                let no = self.name(&o);
                self.expr(obj);
                self.emit(Op::DeclareName(no));
                let nm = self.name(name);
                self.emit(Op::LoadName(no));
                self.emit_get_member(nm);
                self.expr(value);
                self.emit(Op::Bin(bin.expect("compound")));
                let v = self.fresh_temp("v");
                let nv = self.name(&v);
                self.emit(Op::DeclareName(nv));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nv));
                self.emit_set_member(nm);
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            Expr::Index { obj, index, .. } if bin.is_some() => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                let (o, ix) = (self.fresh_temp("a"), self.fresh_temp("x"));
                let (no, nx) = (self.name(&o), self.name(&ix));
                self.expr(obj);
                self.emit(Op::DeclareName(no));
                self.expr(index);
                self.emit(Op::DeclareName(nx));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nx));
                self.emit(Op::IndexGet);
                self.expr(value);
                self.emit(Op::Bin(bin.expect("compound")));
                let v = self.fresh_temp("v");
                let nv = self.name(&v);
                self.emit(Op::DeclareName(nv));
                self.emit(Op::LoadName(no));
                self.emit(Op::LoadName(nx));
                self.emit(Op::LoadName(nv));
                self.emit(Op::IndexSet);
                self.scope_depth -= 1;
                self.emit(Op::PopScope);
            }
            _ => self.bail(),
        }
    }
}

pub(crate) fn stmt_pos(s: &Stmt) -> Option<Pos> {
    match s {
        Stmt::Return { pos, .. } | Stmt::Break { pos, .. } | Stmt::Continue { pos, .. } => {
            Some(*pos)
        }
        Stmt::Expr(e) | Stmt::Throw(e) => expr_pos(e),
        Stmt::If { cond, .. } | Stmt::While { cond, .. } | Stmt::DoWhile { cond, .. } => {
            expr_pos(cond)
        }
        Stmt::ForOf { iter, .. } => expr_pos(iter),
        Stmt::Switch { scrutinee, .. } => expr_pos(scrutinee),
        Stmt::Var(v) => v.bindings.first().and_then(|b| match &b.target {
            Pattern::Name(n) => Some(n.pos),
            _ => b.init.as_ref().and_then(expr_pos),
        }),
        Stmt::Labeled { label, .. } => Some(label.pos),
        _ => None,
    }
}

pub(crate) fn expr_pos(e: &Expr) -> Option<Pos> {
    match e {
        Expr::Ident(n) => Some(n.pos),
        Expr::This(p) => Some(*p),
        Expr::Lit { pos, .. } => Some(*pos),
        Expr::Unary { pos, .. } => Some(*pos),
        Expr::SuperMember { pos, .. } | Expr::SuperCall { pos, .. } => Some(*pos),
        Expr::Paren(inner) | Expr::Update { expr: inner, .. } | Expr::Cast { expr: inner, .. } => {
            expr_pos(inner)
        }
        Expr::Binary { l, .. } | Expr::Assign { target: l, .. } | Expr::Cond { cond: l, .. } => {
            expr_pos(l)
        }
        Expr::Call { callee, .. } => expr_pos(callee),
        Expr::Member { obj, .. } | Expr::Index { obj, .. } => expr_pos(obj),
        Expr::New { ty, .. } => match ty {
            TypeExpr::Named { pos, .. } => Some(*pos),
            _ => None,
        },
        _ => None,
    }
}

/// Conservative: does this statement list contain `return`/`break`/
/// `continue` (not crossing into nested arrows, which are separate
/// functions)? Used to route try+finally with abrupt exits to the AST tier.

// ---- runtime ----------------------------------------------------------------------

/// Synchronous execution: a non-async chunk can never suspend.
pub(crate) fn run_chunk(i: &mut Interp, chunk: &Chunk, env: Env) -> VResult {
    let mut state = Exec {
        pc: 0,
        stack: Vec::with_capacity(16),
        scopes: vec![env],
        handlers: Vec::new(),
    };
    match exec(i, chunk, &mut state, None)? {
        Flow::Done(v) => Ok(v),
        Flow::Await(_) => Err(i.throw_public("TypeError", "`await` outside an async function")),
        Flow::Yield(_) => Err(i.throw_public("TypeError", "`yield` outside a generator")),
    }
}

/// Resumable execution for async functions: the coroutine carries the whole
/// VM state, so `await` suspends by simply returning.
pub(crate) fn run_coro(
    i: &mut Interp,
    coro: &mut Coro,
    resumed: Option<(Value, bool)>,
) -> Result<Flow, Thrown> {
    let chunk = coro.chunk.clone();
    let mut state = Exec {
        pc: coro.pc,
        stack: std::mem::take(&mut coro.stack),
        scopes: std::mem::take(&mut coro.scopes),
        handlers: std::mem::take(&mut coro.handlers),
    };
    let out = exec(i, &chunk, &mut state, resumed);
    coro.pc = state.pc;
    coro.stack = state.stack;
    coro.scopes = state.scopes;
    coro.handlers = state.handlers;
    out
}

pub(crate) struct Exec {
    pc: usize,
    stack: Vec<Value>,
    scopes: Vec<Env>,
    handlers: Vec<(usize, usize, usize)>,
}

fn exec(
    i: &mut Interp,
    chunk: &Chunk,
    state: &mut Exec,
    resumed: Option<(Value, bool)>,
) -> Result<Flow, Thrown> {
    let pc_ref: &mut usize = &mut state.pc;
    let stack: &mut Vec<Value> = &mut state.stack;
    let scopes: &mut Vec<Env> = &mut state.scopes;
    let handlers: &mut Vec<(usize, usize, usize)> = &mut state.handlers;
    let mut pc = *pc_ref;
    // Resuming from an await: deliver the settled value (or throw it).
    if let Some((value, rejected)) = resumed {
        if rejected {
            match unwind(i, stack, scopes, handlers, Thrown(value), &mut pc) {
                Ok(()) => {}
                Err(t) => {
                    *pc_ref = pc;
                    return Err(t);
                }
            }
        } else {
            stack.push(value);
        }
    }

    macro_rules! cur {
        () => {
            scopes.last().expect("scope")
        };
    }
    macro_rules! throwing {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(t) => {
                    if let Some((hpc, sl, stl)) = handlers.pop() {
                        scopes.truncate(sl);
                        stack.truncate(stl);
                        stack.push(t.0);
                        pc = hpc;
                        continue;
                    }
                    *pc_ref = pc;
                    return Err(t);
                }
            }
        };
    }

    loop {
        let op = chunk.code[pc];
        i.set_site(chunk.pos_at(pc));
        pc += 1;
        match op {
            Op::Const(ci) => stack.push(chunk.consts[ci as usize].clone()),
            Op::Null => stack.push(Value::Null),
            Op::LoadName(ni) => {
                let name = &chunk.names[ni as usize];
                let v = throwing!(env_get(cur!(), name)
                    .ok_or_else(|| i.throw("TypeError", format!("`{name}` is not defined"))));
                stack.push(v);
            }
            Op::StoreName(ni) => {
                let name = &chunk.names[ni as usize];
                let v = stack.pop().expect("store");
                throwing!(if env_set(cur!(), name, v) {
                    Ok(())
                } else {
                    Err(i.throw("TypeError", format!("`{name}` is not defined")))
                });
            }
            Op::DeclareName(ni) => {
                let v = stack.pop().expect("declare");
                env_define(cur!(), &chunk.names[ni as usize], v);
            }
            Op::BindPattern(pi) => {
                let v = stack.pop().expect("bindpat");
                let env = cur!().clone();
                throwing!(i.bind_pattern(chunk.patterns[pi as usize], v, &env));
            }
            Op::LoadThis => {
                let v = throwing!(env_get(cur!(), "this")
                    .ok_or_else(|| i.throw("TypeError", "`this` is not available here")));
                stack.push(v);
            }
            Op::PushScope => {
                let child = child_env(cur!());
                scopes.push(child);
            }
            Op::PopScope => {
                scopes.pop();
            }
            Op::Pop => {
                stack.pop();
            }
            Op::Dup => {
                let v = stack.last().expect("dup").clone();
                stack.push(v);
            }
            Op::Bin(op) => {
                let r = stack.pop().expect("bin r");
                let l = stack.pop().expect("bin l");
                let v = match op {
                    BinOp::Eq | BinOp::Ne => {
                        let eq = throwing!(i.values_equal(&l, &r));
                        Value::Bool(if op == BinOp::Eq { eq } else { !eq })
                    }
                    _ => throwing!(i.numeric_binop(op, l, r)),
                };
                stack.push(v);
            }
            Op::Un(op) => {
                let v = stack.pop().expect("un");
                if op == UnaryOp::Await {
                    throwing!(i.type_error::<()>("`await` is not in the MVP"));
                    continue;
                }
                let out = throwing!(i.eval_unary(op, v));
                stack.push(out);
            }
            Op::Truthy => {
                let v = stack.pop().expect("truthy");
                let b = throwing!(i.value_truthy(&v));
                stack.push(Value::Bool(b));
            }
            Op::Jump(t) => pc = t,
            Op::JumpIfFalse(t) => {
                let v = stack.pop().expect("jf");
                if !throwing!(i.value_truthy(&v)) {
                    pc = t;
                }
            }
            Op::JumpIfTrue(t) => {
                let v = stack.pop().expect("jt");
                if throwing!(i.value_truthy(&v)) {
                    pc = t;
                }
            }
            Op::OnNullJump(t) => {
                if matches!(stack.last(), Some(Value::Null)) {
                    pc = t;
                }
            }
            Op::NotNullJump(t) => {
                if matches!(stack.last(), Some(Value::Null)) {
                    stack.pop();
                } else {
                    pc = t;
                }
            }
            Op::ToDisplayStr => {
                let v = stack.pop().expect("tds");
                // A class implementing `Display` gets its `toString()` called —
                // which means this can run Mersey code, and can throw.
                let shown = throwing!(i.display(&v));
                stack.push(Value::Str(Rc::new(shown.chars().collect())));
            }
            Op::Call(argc) => {
                let args = split_args(stack, argc as usize);
                let callee = stack.pop().expect("callee");
                let v = throwing!(i.call_value(&callee, args));
                stack.push(v);
            }
            Op::CallV => {
                let args = pop_array(stack);
                let callee = stack.pop().expect("callee");
                let v = throwing!(i.call_value(&callee, args));
                stack.push(v);
            }
            Op::CallMethod(ni, argc) => {
                let args = split_args(stack, argc as usize);
                let recv = stack.pop().expect("recv");
                let v = throwing!(i.call_member(&recv, &chunk.names[ni as usize], args));
                stack.push(v);
            }
            Op::CallMethodV(ni) => {
                let args = pop_array(stack);
                let recv = stack.pop().expect("recv");
                let v = throwing!(i.call_member(&recv, &chunk.names[ni as usize], args));
                stack.push(v);
            }
            Op::NewNamed(ni, argc) => {
                let args = split_args(stack, argc as usize);
                let env = cur!().clone();
                let v = throwing!(i.new_named(&chunk.names[ni as usize], args, &env));
                stack.push(v);
            }
            Op::NewNamedV(ni) => {
                let args = pop_array(stack);
                let env = cur!().clone();
                let v = throwing!(i.new_named(&chunk.names[ni as usize], args, &env));
                stack.push(v);
            }
            Op::SuperCall(argc) => {
                let args = split_args(stack, argc as usize);
                let env = cur!().clone();
                let v = throwing!(i.super_call(args, &env));
                stack.push(v);
            }
            Op::SuperMember(ni) => {
                let env = cur!().clone();
                let v = throwing!(i.super_lookup(&chunk.names[ni as usize], &env));
                stack.push(v);
            }
            Op::CallSuperMethod(ni, argc) => {
                let args = split_args(stack, argc as usize);
                let env = cur!().clone();
                let v = throwing!(i.call_super_method(&chunk.names[ni as usize], args, &env));
                stack.push(v);
            }
            Op::SuperCallV => {
                let args = pop_array(stack);
                let env = cur!().clone();
                let v = throwing!(i.super_call(args, &env));
                stack.push(v);
            }
            Op::CallSuperMethodV(ni) => {
                let args = pop_array(stack);
                let env = cur!().clone();
                let v = throwing!(i.call_super_method(&chunk.names[ni as usize], args, &env));
                stack.push(v);
            }
            Op::ImportCall(ni) => {
                let spec = chunk.names[ni as usize].clone();
                let v = throwing!(i.dynamic_import(&spec));
                stack.push(v);
            }
            Op::GetMember(ni, ci) => {
                let o = stack.pop().expect("obj");
                // Fast path: a hit is a constant-offset load out of the
                // instance's slot vector — no hashing, no chain walk.
                let hit = match &o {
                    Value::Instance(inst) => {
                        let b = inst.borrow();
                        let ic = chunk.caches[ci as usize].get();
                        if ic.class == b.class.id {
                            Some(b.slots[ic.slot as usize].clone())
                        } else if let Some(slot) = b.class.slot_of(&chunk.names[ni as usize]) {
                            chunk.caches[ci as usize].set(ICache {
                                class: b.class.id,
                                slot,
                            });
                            Some(b.slots[slot as usize].clone())
                        } else {
                            None // a method, a getter, or an error: slow path
                        }
                    }
                    _ => None,
                };
                let v = match hit {
                    Some(v) => v,
                    None => {
                        let name = &chunk.names[ni as usize];
                        throwing!(match i.get_member(&o, name) {
                            Ok(Some(v)) => Ok(v),
                            Ok(None) => Err(i.throw(
                                "TypeError",
                                format!("no member `{name}` on {}", kind_of(&o))
                            )),
                            Err(t) => Err(t),
                        })
                    }
                };
                stack.push(v);
            }
            Op::SetMember(ni, ci) => {
                let v = stack.pop().expect("val");
                let o = stack.pop().expect("obj");
                let stored = match &o {
                    Value::Instance(inst) => {
                        let mut b = inst.borrow_mut();
                        let ic = chunk.caches[ci as usize].get();
                        if ic.class == b.class.id {
                            b.slots[ic.slot as usize] = v.clone();
                            true
                        } else if let Some(slot) = b.class.slot_of(&chunk.names[ni as usize]) {
                            chunk.caches[ci as usize].set(ICache {
                                class: b.class.id,
                                slot,
                            });
                            b.slots[slot as usize] = v.clone();
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if !stored {
                    throwing!(i.set_member(&o, &chunk.names[ni as usize], v.clone()));
                }
                stack.push(v);
            }
            Op::IndexGet => {
                let idx = stack.pop().expect("idx");
                let o = stack.pop().expect("obj");
                let v = throwing!(i.index_get(&o, &idx));
                stack.push(v);
            }
            Op::IndexSet => {
                let v = stack.pop().expect("val");
                let idx = stack.pop().expect("idx");
                let o = stack.pop().expect("obj");
                throwing!(i.index_set(&o, &idx, v.clone()));
                stack.push(v);
            }
            Op::MakeArray => stack.push(crate::new_array(Vec::new())),
            Op::ArrayPush1 => {
                let v = stack.pop().expect("elem");
                if let Some(Value::Array(a)) = stack.last() {
                    a.borrow_mut().push(v);
                }
            }
            Op::ArraySpread => {
                let v = stack.pop().expect("spread");
                let items = match &v {
                    Value::Array(a) => a.borrow().clone(),
                    _ => {
                        throwing!(i.type_error::<()>("can only spread arrays"));
                        continue;
                    }
                };
                if let Some(Value::Array(a)) = stack.last() {
                    a.borrow_mut().extend(items);
                }
            }
            Op::MakeRecord => stack.push(crate::new_record(Vec::new())),
            Op::RecordSetField(ni) => {
                let v = stack.pop().expect("field");
                if let Some(Value::Record(r)) = stack.last() {
                    crate::rec_set(&mut r.borrow_mut(), &chunk.names[ni as usize], v);
                }
            }
            Op::RecordSpread => {
                let v = stack.pop().expect("spread");
                let entries = match &v {
                    Value::Record(src) => src.borrow().clone(),
                    _ => {
                        throwing!(i.type_error::<()>("can only spread records"));
                        continue;
                    }
                };
                if let Some(Value::Record(r)) = stack.last() {
                    let mut fields = r.borrow_mut();
                    for (k, val) in entries {
                        crate::rec_set(&mut fields, &k, val);
                    }
                }
            }
            Op::MakeClosure(pi) => {
                let data = chunk.protos[pi as usize].clone();
                let this = env_get(cur!(), "this");
                stack.push(Value::Closure(Rc::new(Closure {
                    data,
                    env: cur!().clone(),
                    this,
                    cls: None,
                })));
            }
            Op::InstanceOf => {
                let r = stack.pop().expect("rhs");
                let l = stack.pop().expect("lhs");
                let v = throwing!(i.instance_of(&l, &r));
                stack.push(v);
            }
            Op::CastOp(ti, wrapping) => {
                let v = stack.pop().expect("cast");
                let out = throwing!(i.eval_cast(v, wrapping, chunk.types[ti as usize]));
                stack.push(out);
            }
            Op::IsOp(ti) => {
                let v = stack.pop().expect("is");
                let out = i.value_is(&v, chunk.types[ti as usize]);
                stack.push(Value::Bool(out));
            }
            Op::AsyncIterInit => {
                let v = stack.pop().expect("aiter");
                let it = throwing!(i.async_iter_of(&v));
                stack.push(it);
            }
            Op::IterArray => {
                let v = stack.pop().expect("iter");
                // Arrays, strings, host iterables, generators — and a class that
                // implements `Iterable<T>`, whose `iter()` is called here.
                let items: Vec<Value> = throwing!(i.iter_values(&v));
                stack.push(crate::new_array(items));
            }
            Op::PushHandler(t) => handlers.push((t, scopes.len(), stack.len())),
            Op::PopHandler => {
                handlers.pop();
            }
            Op::CatchMatches(ti) => {
                let matches = match stack.last() {
                    Some(v) => i.catch_matches(chunk.types[ti as usize], v),
                    None => false,
                };
                stack.push(Value::Bool(matches));
            }
            Op::Throw => {
                let v = stack.pop().expect("throw");
                throwing!(Err::<(), Thrown>(Thrown(v)));
            }
            Op::Await => {
                let v = stack.pop().expect("await");
                *pc_ref = pc; // resume after the Await
                return Ok(Flow::Await(v));
            }
            Op::YieldOp => {
                let v = stack.pop().expect("yield");
                *pc_ref = pc;
                return Ok(Flow::Yield(v));
            }
            Op::Return => {
                *pc_ref = pc;
                return Ok(Flow::Done(stack.pop().expect("ret")));
            }
            Op::ReturnNull => {
                *pc_ref = pc;
                return Ok(Flow::Done(Value::Null));
            }
        }
    }
}

/// Route a thrown value into the innermost handler, or propagate.
fn unwind(
    _i: &mut Interp,
    stack: &mut Vec<Value>,
    scopes: &mut Vec<Env>,
    handlers: &mut Vec<(usize, usize, usize)>,
    t: Thrown,
    pc: &mut usize,
) -> Result<(), Thrown> {
    match handlers.pop() {
        Some((hpc, sl, stl)) => {
            scopes.truncate(sl);
            stack.truncate(stl);
            stack.push(t.0);
            *pc = hpc;
            Ok(())
        }
        None => Err(t),
    }
}

fn split_args(stack: &mut Vec<Value>, argc: usize) -> Vec<Value> {
    stack.split_off(stack.len() - argc)
}

fn pop_array(stack: &mut Vec<Value>) -> Vec<Value> {
    match stack.pop() {
        Some(Value::Array(a)) => a.borrow().clone(),
        _ => Vec::new(),
    }
}

// ---- verifier -----------------------------------------------------------------------

/// Static checks: every index in range, every jump target in range, and a
/// consistent operand-stack depth at every pc (join points must agree).
pub fn verify(chunk: &Chunk) -> Result<(), String> {
    analyze(chunk).map(|_| ())
}

/// Verifier core; returns the stack depth at each pc (None = unreachable).
/// The JIT tier consumes this to shape block parameters.
pub fn analyze(chunk: &Chunk) -> Result<Vec<Option<i32>>, String> {
    let n = chunk.code.len();
    let mut depth_at: Vec<Option<i32>> = vec![None; n + 1];
    let mut work = vec![(0usize, 0i32)];
    // Handler entry points get thrown-value depth relative to their push.
    while let Some((pc, depth)) = work.pop() {
        if pc > n {
            return Err(format!("pc {pc} out of range"));
        }
        if pc == n {
            continue; // fell off the end (ReturnNull is always emitted, but be lenient)
        }
        if let Some(d) = depth_at[pc] {
            if d != depth {
                return Err(format!("stack depth mismatch at {pc}: {d} vs {depth}"));
            }
            continue;
        }
        depth_at[pc] = Some(depth);
        let op = chunk.code[pc];
        let (pops, pushes): (i32, i32) = match op {
            Op::Const(i) => {
                bounds(i, chunk.consts.len(), "const")?;
                (0, 1)
            }
            Op::Null | Op::LoadThis | Op::MakeArray | Op::MakeRecord => (0, 1),
            Op::LoadName(i) | Op::SuperMember(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (0, 1)
            }
            Op::StoreName(i) | Op::DeclareName(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (1, 0)
            }
            Op::BindPattern(i) => {
                bounds(i, chunk.patterns.len(), "pattern")?;
                (1, 0)
            }
            Op::PushScope | Op::PopScope | Op::PopHandler => (0, 0),
            Op::Pop => (1, 0),
            Op::Dup => (1, 2),
            Op::Bin(_) => (2, 1),
            Op::Un(_) | Op::Truthy | Op::ToDisplayStr | Op::IterArray => (1, 1),
            Op::Jump(t) => {
                work.push((t, depth));
                (0, 0)
            }
            Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                work.push((t, depth - 1));
                (1, 0)
            }
            Op::OnNullJump(t) => {
                work.push((t, depth));
                (0, 0)
            }
            Op::NotNullJump(t) => {
                work.push((t, depth));
                (1, 0)
            }
            Op::Call(a) => (a as i32 + 1, 1),
            Op::CallV => (2, 1),
            Op::CallMethod(i, a) => {
                bounds(i, chunk.names.len(), "name")?;
                (a as i32 + 1, 1)
            }
            Op::CallMethodV(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (2, 1)
            }
            Op::NewNamed(i, a) => {
                bounds(i, chunk.names.len(), "name")?;
                (a as i32, 1)
            }
            Op::NewNamedV(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (1, 1)
            }
            Op::SuperCall(a) => (a as i32, 1),
            Op::SuperCallV => (1, 1),
            Op::CallSuperMethodV(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (1, 1)
            }
            Op::CallSuperMethod(i, a) => {
                bounds(i, chunk.names.len(), "name")?;
                (a as i32, 1)
            }
            Op::ImportCall(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (0, 1)
            }
            Op::GetMember(i, c) => {
                bounds(i, chunk.names.len(), "name")?;
                bounds(c, chunk.caches.len(), "cache")?;
                (1, 1)
            }
            Op::SetMember(i, c) => {
                bounds(i, chunk.names.len(), "name")?;
                bounds(c, chunk.caches.len(), "cache")?;
                (2, 1)
            }
            Op::IndexGet => (2, 1),
            Op::IndexSet => (3, 1),
            Op::ArrayPush1 | Op::ArraySpread | Op::RecordSpread => (1, 0),
            Op::RecordSetField(i) => {
                bounds(i, chunk.names.len(), "name")?;
                (1, 0)
            }
            Op::MakeClosure(i) => {
                bounds(i, chunk.protos.len(), "proto")?;
                (0, 1)
            }
            Op::InstanceOf => (2, 1),
            Op::CastOp(i, _) => {
                bounds(i, chunk.types.len(), "type")?;
                (1, 1)
            }
            Op::IsOp(i) => {
                bounds(i, chunk.types.len(), "type")?;
                (1, 1)
            }
            Op::AsyncIterInit => (1, 1),
            Op::PushHandler(t) => {
                work.push((t, depth + 1));
                (0, 0)
            }
            Op::CatchMatches(i) => {
                bounds(i, chunk.types.len(), "type")?;
                (0, 1)
            }
            Op::Throw => (1, 0),
            Op::Await => (1, 1),
            Op::YieldOp => (1, 1),
            Op::Return => (1, 0),
            Op::ReturnNull => (0, 0),
        };
        let next = depth - pops + pushes;
        if depth - pops < 0 {
            return Err(format!("stack underflow at {pc} ({op:?})"));
        }
        // Fallthrough (except unconditional control transfers).
        match op {
            Op::Jump(_) | Op::Return | Op::ReturnNull | Op::Throw => {}
            _ => work.push((pc + 1, next)),
        }
    }
    depth_at.pop();
    Ok(depth_at)
}

fn bounds(i: u16, len: usize, what: &str) -> Result<(), String> {
    if (i as usize) < len {
        Ok(())
    } else {
        Err(format!("{what} index {i} out of range ({len})"))
    }
}
