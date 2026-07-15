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
use mersey_front::check::{IntKind, Num};
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
    /// Frame slots this chunk needs (see `Op::LoadSlot`).
    pub n_slots: u16,
    /// What each slot holds, where the checker knew. A slot with a type is a
    /// *register*: Tier 1 can keep it in a machine register of the right width,
    /// and a function that mixes `int32` and `float64` stops being a function it
    /// has to refuse.
    pub slot_types: Vec<Option<Num>>,
    /// Does anything in this body actually live in the environment?
    ///
    /// If nothing does, the call needs no `Scope` — no `Rc`, no `GcCell`, no
    /// `HashMap`, and nothing for the collector to track. The frame is the whole
    /// story, and names that are not locals (globals, imports) still resolve,
    /// because the chain this runs against is the closure's own environment,
    /// whose root is the globals.
    pub needs_env: bool,
    /// Every parameter is a plain name — no default, no rest, no destructuring —
    /// so the arguments map one-to-one onto the first slots and can be moved
    /// straight in. `bind_params` exists for everything else.
    pub simple_params: bool,
    /// Where `this` lives, for a body that mentions it. It is a frame slot like
    /// any other: putting it in the environment is what forced every method to
    /// allocate one.
    pub this_slot: Option<u16>,
    /// The method each name resolves to, per class — a monomorphic inline cache,
    /// one per name. Sealed shapes (§4.1) mean a class's method set never
    /// changes, so the only thing that can invalidate an entry is a *different*
    /// class arriving at the same site.
    #[allow(clippy::type_complexity)]
    pub method_cache:
        Vec<std::cell::RefCell<Option<(u64, Rc<crate::FnData>, Rc<crate::ClassDef>)>>>,
    /// The class each name resolves to, filled in on first use.
    ///
    /// `new Point(…)` used to scan the name for a `.`, then hash it, then walk
    /// the scope chain — on every allocation. It can never resolve to a
    /// different class: a class declaration is not a variable and cannot be
    /// reassigned (E0304). So it is looked up once.
    pub class_cache: Vec<std::cell::RefCell<Option<Rc<crate::ClassDef>>>>,
    /// Does this body contain a `yield`? A property of the bytecode, computed
    /// once — it used to be a *linear scan of every instruction*, run on every
    /// call, to answer a question whose answer never changes.
    pub yields: bool,
    /// …and an `await`.
    pub awaits: bool,
    /// Calls seen. On the chunk, not in a map keyed by its address — a hash per
    /// call to decide whether to compile something is a strange thing to pay for
    /// on every call forever.
    pub hot: std::cell::Cell<u32>,
    /// Parameters that live in slots: (name index, slot). They arrive bound in
    /// the environment — `bind_params` handles defaults, rest and destructuring,
    /// and the tree-walker reads them from there — so the frame is filled from it
    /// once, at entry, instead of on every access.
    pub param_slots: Vec<(u16, u16)>,
    pub patterns: Vec<&'static Pattern>,
    pub types: Vec<&'static TypeExpr>,
    pub(crate) protos: Vec<Rc<FnData>>,
    /// Source position per instruction (parallel to `code`) — errors get a
    /// file:line:col instead of a bare message.
    pub positions: Vec<Pos>,
    /// The module this chunk came from.
    pub module: Rc<str>,
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

/// Append a decimal integer straight into a UTF-16 buffer — no String, no
/// intermediate allocation. The hot case of `${i}` in a template.
fn append_int_u16(out: &mut Vec<u16>, mut v: i64) {
    if v < 0 {
        out.push(u16::from(b'-'));
        // i64::MIN negates onto itself; split off the last digit first.
        let last = (v % 10).unsigned_abs() as u16;
        v = (v / 10).abs();
        if v == 0 {
            out.push(u16::from(b'0') + last);
            return;
        }
        append_digits(out, v as u64);
        out.push(u16::from(b'0') + last);
        return;
    }
    append_digits(out, v as u64);
}

fn append_digits(out: &mut Vec<u16>, v: u64) {
    let mut buf = [0u16; 20];
    let mut i = buf.len();
    let mut v = v;
    loop {
        i -= 1;
        buf[i] = u16::from(b'0') + (v % 10) as u16;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Const(u16),
    Null,
    LoadName(u16),
    StoreName(u16),
    DeclareName(u16),
    /// A local, by frame slot: no name, no hash, no scope chain.
    ///
    /// `LoadName` looks a `String` up in a `HashMap` and then walks to the parent
    /// scope and does it again. A loop touching three locals paid five hash
    /// lookups an iteration, and that — not the arithmetic — was most of what
    /// Tier 0 cost. The compiler already knows where every local lives; these say
    /// so.
    ///
    /// A local a nested closure can see does *not* get a slot: the closure
    /// outlives the frame, so that binding still lives in the environment and
    /// still goes through `LoadName`. Everything else is a slot.
    LoadSlot(u16),
    StoreSlot(u16),
    /// Pops a value, binds it through an AST pattern (defaults included).
    BindPattern(u16),
    LoadThis,
    PushScope,
    PopScope,
    Pop,
    Dup,
    Bin(BinOp),
    /// A binary operator whose operands the checker proved are *both* this
    /// numeric type (§3.3 inserts the conversions that make it so).
    ///
    /// `Bin` has to work out what it is holding: an `int32 + int32` walked a
    /// string check, a bigint check, a promotion into a common type, and then a
    /// second dispatch — four matches to add two numbers whose types were known
    /// at compile time. This is the same operator with the answer supplied.
    BinNum(BinOp, Num),
    Un(UnaryOp),
    Truthy,
    /// Convert the value on top of the stack to a numeric type (§3.3).
    ///
    /// This is what makes the bytecode typed. Without it the engine erased the
    /// declared type and dispatched on whatever the value happened to be, so
    /// `let x: float64 = 7; x / 2` stored an `i32` and did integer division.
    Convert(Num),
    Jump(usize),
    JumpIfFalse(usize),
    JumpIfTrue(usize),
    /// If TOS is null: keep it and jump. Else: fall through.
    OnNullJump(usize),
    /// If TOS is not null: keep it and jump. Else: pop and fall through.
    NotNullJump(usize),
    ToDisplayStr,
    /// Pop n parts and join them into ONE string: the fused template op.
    /// `\`value-${i}\`` is two parts and one allocation, not a display-convert
    /// and a concat per part (each of which allocated).
    TemplateJoin(u8),
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
    /// A fresh empty map/set/byte-buffer: the zero of a container type.
    MakeMap,
    MakeSet,
    MakeBytes,
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
    compile_fn_in(body, "", &[])
}

pub(crate) fn compile_fn_in(
    body: &FnBody,
    module: &str,
    params: &[mersey_front::ast::Param],
) -> Option<Rc<Chunk>> {
    let mut c = C::new();
    c.module = module.to_string();
    // Which locals a closure can see decides where they live, so it has to be
    // known before a single one is placed.
    c.captured = captured_names(body);
    c.simple_params = params
        .iter()
        .all(|p| matches!(p.target, Pattern::Name(_)) && !p.rest && p.default.is_none());
    for p in params {
        if let Pattern::Name(n) = &p.target {
            match c.declare_local_typed(&n.text, local_type_for(n)) {
                Some(slot) => {
                    let ni = c.name(&n.text);
                    c.param_slots.push((ni, slot));
                }
                // A captured parameter lives in the environment, so the
                // arguments cannot simply be moved into the frame.
                None => c.simple_params = false,
            }
        }
    }
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

/// The conversion this expression's value needs (§3.3).
///
/// The table lives in the checker, which is the only thing that knows: checking
/// a program is what makes its conversions available, so there is nothing for an
/// engine or an embedder to remember to install — and therefore nothing to
/// forget, which would mean silently wrong arithmetic.
pub(crate) use mersey_front::check::{
    coercion_for, coercion_for_name, local_type_for, op_type_for, result_coercion_for,
};

/// Convert a number to a declared numeric type. The C conversions (§3.3):
/// integers truncate and wrap to the target width, floats round.
pub fn convert_num(v: &Value, to: Num) -> Value {
    // Read the value as the two widest carriers, then narrow to the target. An
    // integer source keeps exactness; a float source rounds toward zero, which
    // is what a C cast does.
    let (i, f, is_float) = match v {
        Value::I32(n) => (*n as i128, *n as f64, false),
        Value::I64(n) => (*n as i128, *n as f64, false),
        Value::U32(n) => (*n as i128, *n as f64, false),
        Value::U64(n) => (*n as i128, *n as f64, false),
        Value::F32(x) => (*x as i128, *x as f64, true),
        Value::F64(x) => (*x as i128, *x as f64, true),
        // Not a number: `Convert` is only emitted where the checker proved one.
        _ => return v.clone(),
    };
    match to {
        Num::F32 => Value::F32(f as f32),
        Num::F64 => Value::F64(f),
        // The int kinds below 32 bits promote to `int32` in arithmetic (§3.3),
        // so they are carried as one — but the *stored* value still has to fit
        // the declared width, which is what the wrap here does.
        Num::Int(IntKind::I8) => Value::I32(i as i8 as i32),
        Num::Int(IntKind::I16) => Value::I32(i as i16 as i32),
        Num::Int(IntKind::I32) => Value::I32(i as i32),
        Num::Int(IntKind::I64) => Value::I64(i as i64),
        Num::Int(IntKind::U8) => Value::I32(i as u8 as i32),
        Num::Int(IntKind::U16) => Value::I32(i as u16 as i32),
        Num::Int(IntKind::U32) => Value::U32(if is_float { f as u32 } else { i as u32 }),
        Num::Int(IntKind::U64) => Value::U64(if is_float { f as u64 } else { i as u64 }),
    }
}

/// A numeric literal, parsed directly at its declared type.
///
/// `let b: uint32 = 4294967295` must not be parsed as an `int32` first: it does
/// not fit one, and the engine used to raise a range error for a literal that
/// fits the type it was actually given.
/// `int32` arithmetic, with the language's semantics (§3.6): wrapping `+ - *`,
/// masked shifts, and a division that *throws* rather than faulting.
///
/// This has to agree with `Interp::promoted_binop` exactly — the tree-walker
/// still goes that way, and the two are compared on every conformance program.
fn i32_binop(i: &mut Interp, op: BinOp, x: i32, y: i32) -> VResult {
    use BinOp::*;
    Ok(match op {
        Add => Value::I32(x.wrapping_add(y)),
        Sub => Value::I32(x.wrapping_sub(y)),
        Mul => Value::I32(x.wrapping_mul(y)),
        Div => {
            if y == 0 {
                return Err(i.throw("RangeError", "division by zero"));
            }
            match x.checked_div(y) {
                Some(q) => Value::I32(q),
                None => return Err(i.throw("RangeError", "integer overflow in division")),
            }
        }
        Rem => {
            if y == 0 {
                return Err(i.throw("RangeError", "division by zero"));
            }
            Value::I32(x.checked_rem(y).unwrap_or(0))
        }
        Pow => {
            if y < 0 {
                return Err(i.throw("RangeError", "negative integer exponent"));
            }
            let mut acc: i32 = 1;
            for _ in 0..y {
                acc = acc.wrapping_mul(x);
            }
            Value::I32(acc)
        }
        Shl => Value::I32(x.wrapping_shl(y as u32)),
        Shr => Value::I32(x.wrapping_shr(y as u32)),
        BitAnd => Value::I32(x & y),
        BitOr => Value::I32(x | y),
        BitXor => Value::I32(x ^ y),
        Lt => Value::Bool(x < y),
        Gt => Value::Bool(x > y),
        Le => Value::Bool(x <= y),
        Ge => Value::Bool(x >= y),
        Eq => Value::Bool(x == y),
        Ne => Value::Bool(x != y),
        _ => return i.type_error("operator not defined for int32"),
    })
}

/// `float64` arithmetic. IEEE: division by zero is an infinity, not an error.
fn f64_binop(op: BinOp, x: f64, y: f64) -> Value {
    use BinOp::*;
    match op {
        Add => Value::F64(x + y),
        Sub => Value::F64(x - y),
        Mul => Value::F64(x * y),
        Div => Value::F64(x / y),
        Rem => Value::F64(x % y),
        Pow => Value::F64(x.powf(y)),
        Lt => Value::Bool(x < y),
        Gt => Value::Bool(x > y),
        Le => Value::Bool(x <= y),
        Ge => Value::Bool(x >= y),
        Eq => Value::Bool(x == y),
        Ne => Value::Bool(x != y),
        // The float operators the language does not define (`&`, `<<`, …) never
        // reach here: the checker rejects them.
        _ => Value::Null,
    }
}

/// The `this` a frame is holding, if it is holding one.
fn this_of(chunk: &Chunk, frame: &[Value], base: usize) -> Option<Value> {
    chunk
        .this_slot
        .map(|s| frame[base + s as usize].clone())
        .filter(|v| !matches!(v, Value::Null))
}

/// A fresh frame for `chunk`, with its slot-resolved parameters filled in.
///
/// Parameters arrive bound in the environment — `bind_params` does defaults,
/// rest and destructuring, and the tree-walker reads them from there. They are
/// copied into the frame once, here, instead of being looked up by name on every
/// access, which is what they were.
pub(crate) fn new_frame(chunk: &Chunk, env: &Env, this: Option<&Value>) -> Vec<Value> {
    let mut frame = vec![Value::Null; chunk.n_slots as usize];
    for (ni, slot) in &chunk.param_slots {
        if let Some(v) = env_get(env, &chunk.names[*ni as usize]) {
            frame[*slot as usize] = v;
        }
    }
    if let (Some(s), Some(t)) = (chunk.this_slot, this) {
        frame[s as usize] = t.clone();
    }
    frame
}

/// The frame for a call that needs no environment: the arguments go straight
/// into the slots the compiler gave them.
///
/// This is what `needs_env` and `simple_params` are for. The general path
/// allocates a `Scope` — an `Rc`, a `GcCell`, a `HashMap`, and an entry in the
/// collector's young list — inserts each argument into it by name, and then hashes
/// each one back *out* again to fill the frame. For most functions none of that is
/// ever read.
pub(crate) fn arg_frame(chunk: &Chunk, args: Vec<Value>, this: Option<&Value>) -> Vec<Value> {
    let mut frame = vec![Value::Null; chunk.n_slots as usize];
    // Parameters are slots 0..n, in order — `simple_params` is the promise that
    // they are, and the compiler hands them out that way.
    for (i, v) in args.into_iter().enumerate() {
        frame[i] = v;
    }
    if let (Some(s), Some(t)) = (chunk.this_slot, this) {
        frame[s as usize] = t.clone();
    }
    frame
}

/// Names the *module* chunk must keep in the environment.
///
/// A module-level `let` is not a local: it is a global. Every function in the
/// module reaches it by name through the global scope, an export hands it to
/// another module by name, and neither can see a frame slot. So anything a
/// function body, a class member, or an export can name stays where they look
/// for it — and only what nothing outside the top-level statements can name gets
/// a slot.
fn module_captured(module: &Module) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut ident = |e: &Expr, out: &mut std::collections::HashSet<String>| {
        if let Expr::Ident(n) = e {
            out.insert(n.text.clone());
        }
    };
    for item in &module.items {
        match item {
            // Every name a function or a class member can utter.
            Item::Decl(d) => {
                mersey_front::ast::walk_decl(d, &mut |e| ident(e, &mut out));
            }
            Item::Export(ExportDecl { kind, .. }) => match kind {
                ExportKind::Decl(d) => {
                    mersey_front::ast::walk_decl(d, &mut |e| ident(e, &mut out));
                }
                // An export is reached by name from another module.
                ExportKind::Var(v) => {
                    for b in &v.bindings {
                        let mut names = Vec::new();
                        crate::pattern_names_of(&b.target, &mut names);
                        out.extend(names);
                    }
                }
                _ => {}
            },
            Item::Stmt(_) | Item::Import(_) => {}
        }
    }
    // …and the closures the top-level statements themselves make.
    for item in &module.items {
        if let Item::Stmt(s) = item {
            mersey_front::ast::walk_stmt(s, &mut |e| {
                if let Expr::Arrow { .. } = e {
                    mersey_front::ast::walk_expr(e, &mut |inner| ident(inner, &mut out));
                }
            });
        }
    }
    out
}

/// Names that a nested closure inside this body can see.
///
/// A closure outlives the frame that made it, so a local it captures cannot live
/// in that frame — it stays an environment binding, reached by name. Everything
/// else gets a slot.
///
/// The analysis is by *name*, not by binding: a closure with its own `x` also
/// pins an outer `x`. That is deliberate. Getting it wrong in this direction
/// costs a hash lookup for a variable that did not need one; getting it wrong in
/// the other direction would put a captured variable in a frame that is about to
/// be destroyed, and the closure would read a dead slot.
fn captured_names(body: &FnBody) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut visit = |e: &Expr| {
        if let Expr::Arrow { .. } = e {
            // Every name mentioned anywhere inside the closure, however deep.
            mersey_front::ast::walk_expr(e, &mut |inner| {
                if let Expr::Ident(n) = inner {
                    out.insert(n.text.clone());
                }
            });
        }
    };
    match body {
        FnBody::Block(stmts) => {
            for s in *stmts {
                mersey_front::ast::walk_stmt(s, &mut visit);
            }
        }
        FnBody::Expr(e) => mersey_front::ast::walk_expr(e, &mut visit),
    }
    out
}

/// A numeric literal, built directly at its declared type — including one
/// behind a `-` or parentheses, which is still a literal to everyone but the
/// parser.
pub(crate) fn fold_const(e: &Expr, to: Num) -> Option<Value> {
    fn peel(e: &Expr, neg: bool) -> Option<(LitKind, &str, bool)> {
        match e {
            Expr::Lit { kind, text, .. } if matches!(kind, LitKind::Int | LitKind::Float) => {
                Some((*kind, text, neg))
            }
            Expr::Paren(inner) => peel(inner, neg),
            Expr::Unary {
                op: UnaryOp::Neg,
                expr,
                ..
            } => peel(expr, !neg),
            _ => None,
        }
    }
    let (kind, text, neg) = peel(e, false)?;
    let mut src = String::new();
    if neg {
        src.push('-');
    }
    src.push_str(text);
    // A *suffixed* literal (`5i32`) will not parse at another width; it falls
    // back to being evaluated and converted, which is the same answer.
    fold_literal(kind, &src, to)
}

pub(crate) fn fold_literal(kind: LitKind, text: &str, to: Num) -> Option<Value> {
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    Some(match to {
        Num::F32 => Value::F32(clean.parse::<f32>().ok()?),
        Num::F64 => Value::F64(clean.parse::<f64>().ok()?),
        _ if kind == LitKind::Float => return None, // a float into an int: not a widening
        Num::Int(IntKind::U64) => Value::U64(clean.parse::<u64>().ok()?),
        Num::Int(IntKind::U32) => Value::U32(clean.parse::<u32>().ok()?),
        Num::Int(IntKind::I64) => Value::I64(clean.parse::<i64>().ok()?),
        Num::Int(_) => Value::I32(clean.parse::<i32>().ok()?),
    })
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
    chunk.yields
}

/// Does this chunk suspend on an `await`? A module whose top level does is
/// itself asynchronous, and has to run as a coroutine.
pub(crate) fn chunk_awaits(chunk: &Chunk) -> bool {
    chunk.awaits
}

/// Public wrapper for tests/tools: compile a function body from its AST
/// statement list.
pub fn compile_fn_public(
    stmts: &'static [mersey_front::ast::Stmt],
    params: &[mersey_front::ast::Param],
) -> Option<Rc<Chunk>> {
    compile_fn_in(&FnBody::Block(stmts), "", params)
}

/// Compile a module's top-level statements (including exported vars).
pub(crate) fn compile_module_stmts(module: &'static Module) -> Option<Rc<Chunk>> {
    compile_module_stmts_in(module, "")
}

pub(crate) fn compile_module_stmts_in(module: &'static Module, spec: &str) -> Option<Rc<Chunk>> {
    let mut c = C::new();
    c.module = spec.to_string();
    c.captured = module_captured(module);
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
                dump(
                    &f.name.text,
                    compile_fn_in(&FnBody::Block(&f.body), "", &f.params),
                );
            }
            Decl::Class(cl) => {
                for m in &cl.members {
                    match m {
                        ClassMember::Method {
                            name,
                            params,
                            body: Some(b),
                            ..
                        } => {
                            dump(
                                &format!("{}.{name}", cl.name.text),
                                compile_fn_in(&FnBody::Block(b), "", params),
                            );
                        }
                        ClassMember::Ctor { params, body, .. } => {
                            dump(
                                &format!("{}.constructor", cl.name.text),
                                compile_fn_in(&FnBody::Block(body), "", params),
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
    /// Names some nested closure in this body refers to. Such a local must stay
    /// in the environment: the closure outlives the frame that would hold it.
    /// Collected by name, so a closure with its own `x` also pins an outer `x` —
    /// conservative, and the cost of being wrong that way is a hash lookup, not a
    /// wrong answer.
    captured: std::collections::HashSet<String>,
    /// Lexical scopes of slot-resolved locals, innermost last.
    slot_scopes: Vec<HashMap<String, u16>>,
    /// Frame slots handed out so far. Slots are not reused between sibling
    /// blocks — a frame is a `Vec` of `Value`, and a few extra nulls cost less
    /// than the liveness analysis that would pack them.
    n_slots: u16,
    /// Parameters that got slots: (name index, slot).
    param_slots: Vec<(u16, u16)>,
    /// What each slot holds, indexed by slot.
    slot_types: Vec<Option<Num>>,
    /// The slot `this` lives in, allocated the first time the body needs it.
    this_slot: Option<u16>,
    /// Every parameter is a plain name with no default and no rest.
    simple_params: bool,
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
            captured: std::collections::HashSet::new(),
            slot_scopes: vec![HashMap::new()],
            n_slots: 0,
            param_slots: vec![],
            slot_types: vec![],
            this_slot: None,
            simple_params: true,
            temp: 0,
            labeled_next: None,
            module: String::new(),
            ok: true,
        }
    }

    fn finish(mut self) -> Option<Rc<Chunk>> {
        if !self.ok {
            return None;
        }
        // Does anything actually live in the environment? Only these three ops
        // put it there.
        let needs_env = self
            .code
            .iter()
            .any(|op| matches!(op, Op::DeclareName(_) | Op::BindPattern(_) | Op::PushScope));
        let yields = self.code.iter().any(|op| matches!(op, Op::YieldOp));
        let awaits = self.code.iter().any(|op| matches!(op, Op::Await));
        let chunk = Chunk {
            yields,
            awaits,
            class_cache: (0..self.names.len())
                .map(|_| std::cell::RefCell::new(None))
                .collect(),
            method_cache: (0..self.names.len())
                .map(|_| std::cell::RefCell::new(None))
                .collect(),
            n_slots: self.n_slots,
            slot_types: std::mem::take(&mut self.slot_types),
            needs_env,
            simple_params: self.simple_params,
            this_slot: self.this_slot,
            hot: std::cell::Cell::new(0),
            param_slots: std::mem::take(&mut self.param_slots),
            code: self.code,
            consts: self.consts,
            names: self.names,
            patterns: self.patterns,
            types: self.types,
            protos: self.protos,
            positions: self.positions,
            module: self.module.into(),
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

    /// Give `name` a frame slot in the current block, unless a closure can see
    /// it — in which case it stays an environment binding.
    fn declare_local(&mut self, name: &str) -> Option<u16> {
        self.declare_local_typed(name, None)
    }

    fn declare_local_typed(&mut self, name: &str, ty: Option<Num>) -> Option<u16> {
        if self.captured.contains(name) {
            return None;
        }
        let slot = self.n_slots;
        self.n_slots += 1;
        self.slot_types.push(ty);
        self.slot_scopes
            .last_mut()
            .expect("a scope")
            .insert(name.to_string(), slot);
        Some(slot)
    }

    /// The slot `name` lives in, if it lives in one.
    fn slot_of(&self, name: &str) -> Option<u16> {
        self.slot_scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
    }

    fn push_slot_scope(&mut self) {
        self.slot_scopes.push(HashMap::new());
    }

    fn pop_slot_scope(&mut self) {
        self.slot_scopes.pop();
    }

    /// Read a name: a slot if it is a local we placed, otherwise a name lookup —
    /// which is now only globals, imports, and locals a closure captured.
    fn load_name(&mut self, name: &str) {
        match self.slot_of(name) {
            Some(slot) => {
                self.emit(Op::LoadSlot(slot));
            }
            None => {
                let i = self.name(name);
                self.emit(Op::LoadName(i));
            }
        }
    }

    /// Assign to an existing binding.
    fn store_name(&mut self, name: &str) {
        match self.slot_of(name) {
            Some(slot) => {
                self.emit(Op::StoreSlot(slot));
            }
            None => {
                let i = self.name(name);
                self.emit(Op::StoreName(i));
            }
        }
    }

    /// Introduce a binding, giving it a slot where it can have one.
    fn declare_name(&mut self, name: &str) {
        match self.declare_local(name) {
            Some(slot) => {
                self.emit(Op::StoreSlot(slot));
            }
            None => {
                let i = self.name(name);
                self.emit(Op::DeclareName(i));
            }
        }
    }

    /// Declare a local, carrying the numeric type the checker gave it.
    fn declare_typed(&mut self, n: &'static Name) {
        match self.declare_local_typed(&n.text, local_type_for(n)) {
            Some(slot) => {
                self.emit(Op::StoreSlot(slot));
            }
            None => {
                let i = self.name(&n.text);
                self.emit(Op::DeclareName(i));
            }
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

    /// Does anything declared *directly* in these statements need to live in the
    /// environment?
    ///
    /// If nothing does, the block needs no runtime scope — and a `Scope` is an
    /// allocation, a `GcCell` and a `HashMap`. A loop body used to make one per
    /// iteration whether or not it held anything.
    fn declares_env(&self, stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| match s {
            Stmt::Var(v) => self.var_declares_env(v),
            _ => false,
        })
    }

    fn var_declares_env(&self, v: &VarStmt) -> bool {
        v.bindings.iter().any(|b| match &b.target {
            Pattern::Name(n) => self.captured.contains(&n.text),
            // Destructuring binds through the environment.
            _ => true,
        })
    }

    /// Does a closure anywhere in this body refer to a name this pattern binds?
    /// If so its binding lives in the environment, and a loop must give each
    /// iteration its own.
    fn target_is_captured(&self, p: &Pattern) -> bool {
        let mut names = Vec::new();
        crate::pattern_names_of(p, &mut names);
        names.iter().any(|n| self.captured.contains(n))
    }

    /// Emit an operator, typed if the checker knew what its operands are.
    fn emit_bin(&mut self, e: &'static Expr, op: BinOp) {
        match op_type_for(e) {
            Some(n) => self.emit(Op::BinNum(op, n)),
            None => self.emit(Op::Bin(op)),
        };
    }

    /// The slot `this` lives in, allocating it on first use.
    fn this_slot(&mut self) -> u16 {
        match self.this_slot {
            Some(s) => s,
            None => {
                let s = self.n_slots;
                self.n_slots += 1;
                self.slot_types.push(None); // an instance, not a number
                self.this_slot = Some(s);
                s
            }
        }
    }

    /// A compiler temp. Always a slot: it has no name in the source, so no
    /// closure can refer to it.
    fn temp_slot(&mut self, tag: &str) -> u16 {
        self.temp += 1;
        let name = format!("#{tag}{}", self.temp);
        let slot = self.n_slots;
        self.n_slots += 1;
        self.slot_types.push(None); // a compiler temp: an array, an index, a value
        self.slot_scopes
            .last_mut()
            .expect("a scope")
            .insert(name, slot);
        slot
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
                let scope = self.declares_env(b);
                if scope {
                    self.emit(Op::PushScope);
                    self.scope_depth += 1;
                }
                self.push_slot_scope();
                for s in b {
                    self.stmt(s);
                }
                self.pop_slot_scope();
                if scope {
                    self.scope_depth -= 1;
                    self.emit(Op::PopScope);
                }
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
                // The loop head's own scope, needed only if the head declares
                // something that lives in the environment. An ordinary counted
                // loop declares an `i` that nothing can see, and now allocates
                // nothing at all.
                let head_scope = match init {
                    Some(ForInit::Var(v)) => self.var_declares_env(v),
                    _ => false,
                };
                if head_scope {
                    self.emit(Op::PushScope);
                    self.scope_depth += 1;
                }
                self.push_slot_scope();
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
                        // Only when something can actually capture the
                        // binding: otherwise this is an ordinary counted loop
                        // and should stay one (no scope per iteration, and it
                        // stays inside the JIT's subset).
                        let fresh_each = v.kind == VarKind::Let && loop_captures(cond, step, body);
                        if fresh_each {
                            // Each iteration needs its *own* binding, which is a
                            // thing only the environment can give: a frame slot
                            // is one storage location for the whole call, and the
                            // closures the body makes are precisely what would
                            // notice.
                            for b in &v.bindings {
                                let mut names: Vec<String> = Vec::new();
                                crate::pattern_names_of(&b.target, &mut names);
                                for n in names {
                                    self.captured.insert(n);
                                }
                            }
                        }
                        self.var_stmt(v);
                        if fresh_each {
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
                self.pop_slot_scope();
                if head_scope {
                    self.scope_depth -= 1;
                    self.emit(Op::PopScope);
                }
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
                    self.push_slot_scope();
                    let n_ait = self.temp_slot("ait");
                    let n_item = self.temp_slot("aitem");
                    let n_next = self.name("next");
                    self.expr(iter);
                    // A class implementing `AsyncIterable<T>` hands over its
                    // iterator here; an `AsyncIter` is already one.
                    self.emit(Op::AsyncIterInit);
                    self.emit(Op::StoreSlot(n_ait));
                    self.emit(Op::Null);
                    self.emit(Op::StoreSlot(n_item));

                    self.compile_loop(None, |c| {
                        let start = c.here();
                        c.emit(Op::LoadSlot(n_ait));
                        c.emit(Op::CallMethod(n_next, 0));
                        c.emit(Op::Await);
                        c.emit(Op::StoreSlot(n_item));
                        // `null` ends the sequence, exactly as it does for a
                        // synchronous generator's `next()`.
                        c.emit(Op::LoadSlot(n_item));
                        c.emit(Op::Null);
                        c.emit(Op::Bin(BinOp::Ne));
                        let jf = vec![c.emit(Op::JumpIfFalse(0))];

                        c.emit(Op::PushScope);
                        c.scope_depth += 1;
                        c.push_slot_scope();
                        c.emit(Op::LoadSlot(n_item));
                        c.bind_target(target);
                        c.stmt(body);
                        let cont = c.here();
                        c.pop_slot_scope();
                        c.emit(Op::PopScope);
                        c.scope_depth -= 1;
                        (start, cont, jf)
                    });
                    self.pop_slot_scope();
                    return;
                }
                self.push_slot_scope();
                let n_items = self.temp_slot("it");
                let n_idx = self.temp_slot("ix");
                self.expr(iter);
                self.emit(Op::IterArray);
                self.emit(Op::StoreSlot(n_items));
                let zero = self.konst(Value::I32(0));
                self.emit(Op::Const(zero));
                self.emit(Op::StoreSlot(n_idx));
                self.compile_loop(None, |c| {
                    let start = c.here();
                    c.emit(Op::LoadSlot(n_idx));
                    c.emit(Op::LoadSlot(n_items));
                    let len = c.name("length");
                    c.emit_get_member(len);
                    c.emit(Op::Bin(BinOp::Lt));
                    let jf = c.emit(Op::JumpIfFalse(0));
                    // The item's own scope, needed only when a closure can see
                    // it — then each iteration must get its own binding.
                    let fresh = c.target_is_captured(target);
                    c.push_slot_scope();
                    if fresh {
                        c.emit(Op::PushScope);
                        c.scope_depth += 1;
                    }
                    c.emit(Op::LoadSlot(n_items));
                    c.emit(Op::LoadSlot(n_idx));
                    c.emit(Op::IndexGet);
                    c.bind_target(target);
                    c.stmt(body);
                    if fresh {
                        c.scope_depth -= 1;
                        c.emit(Op::PopScope);
                    }
                    c.pop_slot_scope();
                    let cont = c.here();
                    c.emit(Op::LoadSlot(n_idx));
                    let one = c.konst(Value::I32(1));
                    c.emit(Op::Const(one));
                    c.emit(Op::Bin(BinOp::Add));
                    c.emit(Op::StoreSlot(n_idx));
                    (start, cont, vec![jf])
                });
                self.pop_slot_scope();
            }
            Stmt::Switch { scrutinee, clauses } => {
                self.emit(Op::PushScope);
                self.scope_depth += 1;
                self.push_slot_scope();
                let n_sw = self.temp_slot("sw");
                self.expr(scrutinee);
                self.emit(Op::StoreSlot(n_sw));
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
                        self.emit(Op::LoadSlot(n_sw));
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
                self.pop_slot_scope();
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
                    self.push_slot_scope();
                    self.declare_name(&c.name.text); // pops the thrown value
                    for s in &c.block {
                        self.stmt(s);
                    }
                    self.pop_slot_scope();
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
        use mersey_front::check::{default_for_ty, DefaultVal};
        for b in &v.bindings {
            match &b.init {
                Some(e) => self.expr(e),
                // No initializer: the binding starts at its type's zero. A scalar
                // zero is a constant; a container has to be *made*, fresh, every
                // time the declaration runs — a shared constant would be one
                // container aliased by every execution of this statement.
                None => match b.ty.as_ref().and_then(default_for_ty) {
                    Some(d) if crate::default_is_shareable(d) => {
                        let ci = self.konst(crate::default_value(d));
                        self.emit(Op::Const(ci));
                    }
                    Some(DefaultVal::Map) => {
                        self.emit(Op::MakeMap);
                    }
                    Some(DefaultVal::Set) => {
                        self.emit(Op::MakeSet);
                    }
                    Some(DefaultVal::Bytes) => {
                        self.emit(Op::MakeBytes);
                    }
                    Some(_) => {
                        self.emit(Op::MakeArray);
                    }
                    None => {
                        self.emit(Op::Null);
                    }
                },
            }
            self.bind_target(&b.target);
        }
    }

    /// Pops TOS, binds it to the pattern.
    fn bind_target(&mut self, p: &'static Pattern) {
        match p {
            Pattern::Name(n) => self.declare_typed(n),
            other => {
                let i = self.patterns.len() as u16;
                self.patterns.push(other);
                self.emit(Op::BindPattern(i));
            }
        }
    }

    // ---- expressions ---------------------------------------------------------

    /// Compile an expression, then apply whatever conversion the checker
    /// recorded for it (§3.3). A literal is converted *at compile time* — that
    /// is not just an optimisation: `let b: uint32 = 4294967295` has no int32 to
    /// convert *from*, and parsing it as one is a range error for a value that
    /// fits the type it was declared with perfectly well.
    fn expr(&mut self, e: &'static Expr) {
        let Some(to) = coercion_for(e) else {
            self.expr_at(e);
            return;
        };
        if let Some(v) = fold_const(e, to) {
            let c = self.konst(v);
            self.emit(Op::Const(c));
            return;
        }
        self.expr_at(e);
        self.emit(Op::Convert(to));
    }

    /// Compile the expression itself, tracking its source position.
    fn expr_at(&mut self, e: &'static Expr) {
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
            Expr::Ident(n) => self.load_name(&n.text),
            Expr::This(_) => {
                let s = self.this_slot();
                self.emit(Op::LoadSlot(s));
            }
            Expr::Lit { kind, text, .. } => match parse_literal(*kind, text) {
                Ok(v) => {
                    let i = self.konst(v);
                    self.emit(Op::Const(i));
                }
                Err(_) => self.bail(), // AST tier reports the proper error
            },
            Expr::Template(parts) => {
                // Push every part raw, then join once (one allocation total).
                // Beyond 255 parts, fall back to pairwise concatenation.
                if parts.len() <= u8::MAX as usize && !parts.is_empty() {
                    for p in parts {
                        match p {
                            TplPart::Text(t) => {
                                let v = Value::Str(Rc::new(
                                    crate::utf16(&(crate::unescape(t))),
                                ));
                                let i = self.konst(v);
                                self.emit(Op::Const(i));
                            }
                            TplPart::Expr(e) => self.expr(e),
                        }
                    }
                    self.emit(Op::TemplateJoin(parts.len() as u8));
                } else {
                    let mut first = true;
                    for p in parts {
                        match p {
                            TplPart::Text(t) => {
                                let v = Value::Str(Rc::new(
                                    crate::utf16(&(crate::unescape(t))),
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
                                    self.load_name(&name.text);
                                    // `{ x }` may still widen (§3.3): the
                                    // checker keys that conversion on the field
                                    // name, since there is no expression to key
                                    // it on.
                                    if let Some(to) = coercion_for_name(name) {
                                        self.emit(Op::Convert(to));
                                    }
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
                ret,
                body,
            } => {
                let data = Rc::new(FnData::new(
                    "<arrow>".into(),
                    *is_async,
                    params,
                    match body {
                        ArrowBody::Expr(e) => FnBody::Expr(e),
                        ArrowBody::Block(b) => FnBody::Block(b),
                    },
                    ret.as_ref(),
                ));
                let i = self.protos.len() as u16;
                self.protos.push(data);
                // The arrow takes the enclosing `this` with it, so the slot has
                // to exist even if this body never writes `this` itself.
                self.this_slot();
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
            Expr::Update { prefix, inc, expr } => self.update(e, *prefix, *inc, expr),
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
                    self.emit_bin(e, *op);
                }
            },
            Expr::Assign { op, target, value } => self.assign(e, op, target, value),
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
                self.this_slot(); // `super` needs it
                self.emit(Op::SuperMember(i));
            }
            Expr::SuperCall { args, .. } => match self.args_fixed(args) {
                Some(argc) => {
                    self.this_slot(); // `super` needs it
                    self.emit(Op::SuperCall(argc));
                }
                // A spread argument list arrives as an array, so `super(...xs)`
                // no longer drops the whole function to the AST tier.
                None => {
                    self.args_array(args);
                    self.this_slot(); // `super` needs it
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
                    self.this_slot(); // `super` needs it
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

    fn update(&mut self, e: &'static Expr, prefix: bool, inc: bool, target: &'static Expr) {
        let op = if inc { BinOp::Add } else { BinOp::Sub };
        // The `1` is built at the operand's own type: an `int32` counter must not
        // add a float, and a `float64` one must not add an int and then discover
        // the mismatch at run time.
        let one = self.konst(match op_type_for(e) {
            Some(Num::F64) => Value::F64(1.0),
            Some(Num::F32) => Value::F32(1.0),
            Some(Num::Int(IntKind::I64)) => Value::I64(1),
            Some(Num::Int(IntKind::U32)) => Value::U32(1),
            Some(Num::Int(IntKind::U64)) => Value::U64(1),
            _ => Value::I32(1),
        });
        match target {
            Expr::Ident(n) => {
                self.load_name(&n.text);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(Op::Const(one));
                self.emit_bin(e, op);
                if prefix {
                    self.emit(Op::Dup);
                }
                self.store_name(&n.text);
            }
            Expr::Member { obj, name, .. } => {
                let no = self.temp_slot("u");
                self.expr(obj);
                self.emit(Op::StoreSlot(no));
                let nm = self.name(name);
                self.emit(Op::LoadSlot(no));
                self.emit_get_member(nm);
                if !prefix {
                    self.emit(Op::Dup); // old kept as result
                }
                self.emit(Op::Const(one));
                self.emit_bin(e, op);
                if prefix {
                    self.emit(Op::Dup);
                }
                // stack: [result, new] — store new via temp
                let nv = self.temp_slot("v");
                self.emit(Op::StoreSlot(nv));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nv));
                self.emit_set_member(nm);
                self.emit(Op::Pop);
            }
            Expr::Index { obj, index, .. } => {
                let (no, nx) = (self.temp_slot("u"), self.temp_slot("x"));
                self.expr(obj);
                self.emit(Op::StoreSlot(no));
                self.expr(index);
                self.emit(Op::StoreSlot(nx));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nx));
                self.emit(Op::IndexGet);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(Op::Const(one));
                self.emit_bin(e, op);
                if prefix {
                    self.emit(Op::Dup);
                }
                let nv = self.temp_slot("v");
                self.emit(Op::StoreSlot(nv));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nx));
                self.emit(Op::LoadSlot(nv));
                self.emit(Op::IndexSet);
                self.emit(Op::Pop);
            }
            _ => self.bail(),
        }
    }

    fn assign(
        &mut self,
        e: &'static Expr,
        op: &'static str,
        target: &'static Expr,
        value: &'static Expr,
    ) {
        if op == "=" {
            match target {
                Expr::Ident(n) => {
                    self.expr(value);
                    self.emit(Op::Dup);
                    self.store_name(&n.text);
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
                self.load_name(&n.text);
                match (bin, op) {
                    (Some(b), _) => {
                        self.expr(value);
                        self.emit_bin(e, b);
                        // §3.3 rule 6: `a op= b` computes in the common type and
                        // converts back to `a`'s. Integer promotion means the
                        // common type is at least an int32, so an `int16` would
                        // otherwise hold a value it cannot represent.
                        if let Some(to) = result_coercion_for(e) {
                            self.emit(Op::Convert(to));
                        }
                        self.emit(Op::Dup);
                        self.store_name(&n.text);
                    }
                    (None, "??=") => {
                        let j = self.emit(Op::NotNullJump(0));
                        self.expr(value);
                        self.emit(Op::Dup);
                        self.store_name(&n.text);
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
                        self.store_name(&n.text);
                        let end = self.here();
                        self.patch(j, end);
                    }
                    _ => self.bail(),
                }
            }
            Expr::Member { obj, name, .. } if bin.is_some() => {
                // The temps are slots, so this needs no runtime scope at all.
                let no = self.temp_slot("a");
                self.expr(obj);
                self.emit(Op::StoreSlot(no));
                let nm = self.name(name);
                self.emit(Op::LoadSlot(no));
                self.emit_get_member(nm);
                self.expr(value);
                self.emit_bin(e, bin.expect("compound"));
                if let Some(to) = result_coercion_for(e) {
                    self.emit(Op::Convert(to)); // §3.3 rule 6
                }
                let nv = self.temp_slot("v");
                self.emit(Op::StoreSlot(nv));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nv));
                self.emit_set_member(nm);
            }
            Expr::Index { obj, index, .. } if bin.is_some() => {
                let (no, nx) = (self.temp_slot("a"), self.temp_slot("x"));
                self.expr(obj);
                self.emit(Op::StoreSlot(no));
                self.expr(index);
                self.emit(Op::StoreSlot(nx));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nx));
                self.emit(Op::IndexGet);
                self.expr(value);
                self.emit_bin(e, bin.expect("compound"));
                if let Some(to) = result_coercion_for(e) {
                    self.emit(Op::Convert(to)); // §3.3 rule 6
                }
                let nv = self.temp_slot("v");
                self.emit(Op::StoreSlot(nv));
                self.emit(Op::LoadSlot(no));
                self.emit(Op::LoadSlot(nx));
                self.emit(Op::LoadSlot(nv));
                self.emit(Op::IndexSet);
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

/// A Mersey call that is running *inside* the dispatch loop rather than inside a
/// nested Rust one.
///
/// A call used to cost 90ns — for a function with no arguments and no locals. It
/// re-entered `exec`, which is a two-thousand-line Rust function: a prologue, a
/// fresh `Exec`, a fresh operand stack. V8 does a call in about two nanoseconds,
/// and CPython in forty, and CPython got there by doing exactly this: a
/// Mersey-to-Mersey call pushes a frame and *keeps going round the same loop*.
///
/// What is saved here is the caller's half of that: where it was, and where its
/// things end. The callee's locals sit above the caller's in one shared vector,
/// and so does its operand stack.
struct InlineFrame {
    chunk: Rc<Chunk>,
    pc: usize,
    /// The caller's frame base — the callee's is where the caller's locals ended.
    frame_base: usize,
    scopes_len: usize,
    handlers_len: usize,
    stack_base: usize,
    /// The callee was a method, so its class went on the stack for `super`.
    pushed_cls: bool,
    /// What the *caller* needs to compile itself, if one of its loops gets hot.
    osr: Option<OsrCtx>,
}

/// What a frame needs in order to be compiled and resumed at a loop header.
///
/// The declared signature, which the bytecode does not carry — and the receiver's
/// class, for a method. Compiled code reads a field at a constant offset, and the
/// offset is a fact about the *class*, so a method that does not know which class
/// it is running against cannot be compiled at all.
#[derive(Clone)]
pub(crate) struct OsrCtx {
    pub params: &'static [mersey_front::ast::Param],
    pub ret: Option<Num>,
    pub ret_bool: bool,
    pub ret_obj: Option<Rc<crate::ClassDef>>,
    pub this: Option<Rc<crate::ClassDef>>,
}

/// Synchronous execution: a non-async chunk can never suspend.
pub(crate) fn run_chunk(
    i: &mut Interp,
    chunk: &Rc<Chunk>,
    env: Env,
    frame: Vec<Value>,
    osr: Option<OsrCtx>,
) -> VResult {
    let mut state = Exec {
        pc: 0,
        stack: Vec::with_capacity(16),
        scopes: vec![env],
        frame,
        handlers: Vec::new(),
    };
    let out = exec(i, chunk, &mut state, None, osr);
    match out? {
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
        frame: std::mem::take(&mut coro.frame),
        handlers: std::mem::take(&mut coro.handlers),
    };
    // A coroutine is not a candidate for OSR: its frame is heap state that can
    // suspend, and compiled code has no way to give it back mid-loop.
    let out = exec(i, &chunk, &mut state, resumed, None);
    coro.pc = state.pc;
    coro.stack = state.stack;
    coro.scopes = state.scopes;
    coro.frame = state.frame;
    coro.handlers = state.handlers;
    out
}

pub(crate) struct Exec {
    pc: usize,
    stack: Vec<Value>,
    scopes: Vec<Env>,
    frame: Vec<Value>,
    handlers: Vec<(usize, usize, usize)>,
}

fn exec(
    i: &mut Interp,
    chunk0: &Rc<Chunk>,
    state: &mut Exec,
    resumed: Option<(Value, bool)>,
    osr: Option<OsrCtx>,
) -> Result<Flow, Thrown> {
    let pc_ref: &mut usize = &mut state.pc;
    let stack: &mut Vec<Value> = &mut state.stack;
    let scopes: &mut Vec<Env> = &mut state.scopes;
    let frame: &mut Vec<Value> = &mut state.frame;
    // The chunk is no longer fixed for the life of the call: a Mersey-to-Mersey
    // call switches to the callee's and keeps going round this same loop, instead
    // of re-entering this function. See `InlineFrame`.
    let mut chunk: Rc<Chunk> = chunk0.clone();
    let mut calls: Vec<InlineFrame> = Vec::new();
    let mut frame_base: usize = 0;
    // The frame that is *running* is the one to compile when one of its loops
    // gets hot — and once calls are inlined, that is usually not the frame this
    // loop was entered with. A `main` that calls `work(n)` inlines it, and it is
    // `work`'s loop that matters.
    let mut cur_osr: Option<OsrCtx> = osr;
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
    /// Give an inlined callee's frame back and become the caller again.
    // Back-edge counting for on-stack replacement, per frame: an inlined callee
    // gets its own count, because it is its own loop.
    let mut back_edges: u32 = 0;
    let mut osr_off = cur_osr.is_none();

    /// Push an inlined call: the callee's locals go above the caller's in the one
    /// frame vector, and the loop carries on in the callee's chunk.
    ///
    /// A plain call and a method call differ only in how the callee was found —
    /// a value on the stack, or a name looked up on a class — so they share this.
    macro_rules! push_inline {
        ($c:expr, $cchunk:expr, $n:expr, $at:expr) => {{
            let c = $c;
            let cchunk = $cchunk;
            let n = $n;
            let at = $at;

            // Tier 1 first: a hot compiled callee runs native, and it
            // needs its arguments, not a frame. It is done — nothing below this
            // should run.
            if let Some(v) = throwing!(i.jit_call(&cchunk, &c, &stack[stack.len() - n..])) {
                stack.truncate(at);
                stack.push(v);
                continue;
            }
            // Mersey's own depth limit, counting the frames in this loop
            // as well as the Rust ones below it (§5.2: hostile input must
            // not be able to end the process).
            if i.depth + calls.len() + 1 >= crate::MAX_CALL_DEPTH {
                let t = i.throw("RangeError", "maximum call depth exceeded");
                throwing!(Err::<(), _>(t));
            }
            let base = frame.len();
            frame.resize(base + cchunk.n_slots as usize, Value::Null);
            // The arguments are the top of the operand stack, and the
            // parameters are the first slots: one move, no marshalling.
            for (k, v) in stack.drain(stack.len() - n..).enumerate() {
                frame[base + k] = v;
            }
            stack.pop(); // the callee
            if let (Some(sl), Some(t)) = (cchunk.this_slot, &c.this) {
                frame[base + sl as usize] = t.clone();
            }
            let pushed_cls = match &c.cls {
                Some(cls) => {
                    i.class_stack_push(cls.clone());
                    true
                }
                None => false,
            };
            i.push_frame(&c.data.name, &cchunk.module);
            calls.push(InlineFrame {
                chunk: std::mem::replace(&mut chunk, cchunk),
                pc,
                frame_base,
                scopes_len: scopes.len(),
                handlers_len: handlers.len(),
                stack_base: stack.len(),
                pushed_cls,
                osr: cur_osr,
            });
            // The callee is now the frame that gets compiled if one of
            // *its* loops gets hot — which is the usual shape: a `main`
            // that calls `work(n)`, and it is `work`'s loop that matters.
            cur_osr = if i.jit_enabled() {
                Some(OsrCtx {
                    params: c.data.params,
                    ret: c.data.ret_num,
                    ret_bool: c.data.ret_bool,
                    ret_obj: i.ret_class(&c.data),
                    this: match &c.this {
                        Some(Value::Instance(inst)) => Some(inst.borrow().class.clone()),
                        _ => None,
                    },
                })
            } else {
                None
            };
            osr_off = cur_osr.is_none();
            back_edges = 0;
            scopes.push(c.env.clone());
            frame_base = base;
            pc = 0;
        }};
    }

    macro_rules! unwind_frame {
        ($f:expr) => {{
            let f = $f;
            frame.truncate(frame_base);
            scopes.truncate(f.scopes_len);
            handlers.truncate(f.handlers_len);
            stack.truncate(f.stack_base);
            if f.pushed_cls {
                i.class_stack_pop();
            }
            i.pop_frame();
            chunk = f.chunk;
            pc = f.pc;
            frame_base = f.frame_base;
            cur_osr = f.osr;
            osr_off = cur_osr.is_none();
        }};
    }

    /// A throw looks for a handler in the current frame; if there is none, the
    /// frame is given back and the *caller's* handlers are tried — the same
    /// propagation a nested Rust call got for free, now that the call is not one.
    macro_rules! throwing {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(t) => {
                    let mut thrown = t;
                    'unwind: loop {
                        let floor = calls.last().map(|f| f.handlers_len).unwrap_or(0);
                        if handlers.len() > floor {
                            let (hpc, sl, stl) = handlers.pop().expect("a handler");
                            scopes.truncate(sl);
                            stack.truncate(stl);
                            stack.push(thrown.0);
                            pc = hpc;
                            break 'unwind;
                        }
                        match calls.pop() {
                            Some(f) => {
                                let t2 = thrown;
                                unwind_frame!(f);
                                thrown = t2;
                            }
                            None => {
                                *pc_ref = pc;
                                return Err(thrown);
                            }
                        }
                    }
                    continue;
                }
            }
        };
    }

    // Back-edge counting for on-stack replacement. One increment and one
    // compare per loop iteration — the counter is a local, so a loop that never
    // gets hot pays no lookup at all. `osr_off` latches once the attempt has
    // been made and refused, so a function the backend cannot compile is not
    // asked again every five thousand iterations.

    // The loop got hot. Hand the rest of the function — the remaining
    // iterations and everything after the loop — to compiled code, resuming at
    // this loop's header with the locals the interpreter is holding.
    //
    // Only from a clean point: an empty operand stack (the header of any loop
    // the compiler will accept) and no live `try`, because returning straight
    // out of `exec` would step over a `finally` that has not run.
    macro_rules! back_edge {
        ($t:expr) => {
            // A loop is the one place a long-running program passes through
            // over and over while holding nothing in flight: the operand stack
            // is empty and no object is borrowed. It is the only safe point the
            // interpreter has *inside* a computation, and without collecting
            // here a loop that allocates cycles — every `for` body that makes a
            // closure — grew until the process was killed.
            //
            // The collection this runs derives liveness from reference counts
            // rather than from a root set, which is what makes it legal here at
            // all: the values in the interpreter's Rust locals are held by
            // `Rc`s, so they count themselves.
            if crate::gc::should_collect_cycles() {
                crate::gc::collect_cycles();
            }
            if !osr_off {
                back_edges += 1;
                if back_edges >= i.osr_threshold {
                    osr_off = true; // one attempt; refusal is final for this frame
                                    // A clean seam in the frame that is *running*: nothing of its
                                    // own in flight on the shared operand stack, and no live `try`
                                    // (returning past one would step over a `finally`).
                    let sfloor = calls.last().map(|f| f.stack_base).unwrap_or(0);
                    let hfloor = calls.last().map(|f| f.handlers_len).unwrap_or(0);
                    if stack.len() == sfloor && handlers.len() == hfloor {
                        let ctx = cur_osr
                            .clone()
                            .expect("osr_off is set when there is no context");
                        let slots = &frame[frame_base..frame_base + chunk.n_slots as usize];
                        let chunk_here = chunk.clone();
                        let out = i.try_osr(
                            &chunk_here,
                            ctx.params,
                            ctx.ret,
                            ctx.ret_bool,
                            ctx.ret_obj,
                            ctx.this,
                            $t,
                            slots,
                        );
                        if let Some(v) = throwing!(out) {
                            // The compiled code ran the rest of *this* function.
                            // If it was an inlined call, its caller is still here
                            // and wants the answer.
                            if let Some(f) = calls.pop() {
                                unwind_frame!(f);
                                stack.push(v);
                                continue;
                            }
                            *pc_ref = pc;
                            return Ok(Flow::Done(v));
                        }
                    }
                }
            }
        };
    }

    loop {
        let op = chunk.code[pc];
        i.set_site(chunk.pos_at(pc));
        let here = pc;
        pc += 1;
        match op {
            Op::Const(ci) => stack.push(chunk.consts[ci as usize].clone()),
            Op::Null => stack.push(Value::Null),
            // A local, by slot: an indexed read. No string, no hash, no walk up
            // the scope chain — which is what `LoadName` below still does, and
            // now only for globals, imports, and locals a closure captured.
            // A local, by slot — relative to this frame's base, because the
            // locals of every inlined call live in the one vector.
            Op::LoadSlot(slot) => stack.push(frame[frame_base + slot as usize].clone()),
            Op::StoreSlot(slot) => {
                frame[frame_base + slot as usize] = stack.pop().expect("store slot");
            }
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
                let v = throwing!(this_of(&chunk, frame, frame_base)
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
            Op::BinNum(op, num) => {
                let r = stack.pop().expect("bin r");
                let l = stack.pop().expect("bin l");
                // The two shapes that carry almost all arithmetic, straight
                // through. Anything else — and any value that is somehow not what
                // the checker said — goes the long way rather than being wrong.
                let v = match (num, &l, &r) {
                    (Num::Int(IntKind::I32), Value::I32(a), Value::I32(b)) => {
                        throwing!(i32_binop(i, op, *a, *b))
                    }
                    (Num::F64, Value::F64(a), Value::F64(b)) => f64_binop(op, *a, *b),
                    _ => match op {
                        BinOp::Eq | BinOp::Ne => {
                            let eq = throwing!(i.values_equal(&l, &r));
                            Value::Bool(if op == BinOp::Eq { eq } else { !eq })
                        }
                        _ => throwing!(i.numeric_binop(op, l, r)),
                    },
                };
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
            Op::Convert(to) => {
                let v = stack.pop().expect("convert");
                stack.push(convert_num(&v, to));
            }
            Op::Jump(t) => {
                pc = t;
                if t <= here {
                    back_edge!(t);
                }
            }
            Op::JumpIfFalse(t) => {
                let v = stack.pop().expect("jf");
                if !throwing!(i.value_truthy(&v)) {
                    pc = t;
                    if t <= here {
                        back_edge!(t);
                    }
                }
            }
            Op::JumpIfTrue(t) => {
                let v = stack.pop().expect("jt");
                if throwing!(i.value_truthy(&v)) {
                    pc = t;
                    if t <= here {
                        back_edge!(t);
                    }
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
                stack.push(Value::Str(Rc::new(crate::utf16(&(shown)))));
            }
            Op::TemplateJoin(n) => {
                let n = n as usize;
                let at = stack.len() - n;
                // One output buffer; the common part kinds append in place with
                // no intermediate String and no per-part allocation.
                let mut out: Vec<u16> = Vec::new();
                let mut failed: Option<Thrown> = None;
                for k in 0..n {
                    match &stack[at + k] {
                        Value::Str(s) => out.extend_from_slice(s),
                        Value::I32(v) => append_int_u16(&mut out, *v as i64),
                        Value::I64(v) => append_int_u16(&mut out, *v),
                        Value::Char(c) => out.extend(crate::char_utf16(*c)),
                        other => {
                            // Anything else (floats, bools, Display classes —
                            // which can run Mersey code and throw).
                            let other = other.clone();
                            match i.display(&other) {
                                Ok(shown) => out.extend(crate::utf16(&shown)),
                                Err(t) => {
                                    failed = Some(t);
                                    break;
                                }
                            }
                        }
                    }
                }
                stack.truncate(at);
                if let Some(t) = failed {
                    throwing!(Err(t));
                }
                stack.push(Value::Str(Rc::new(out)));
            }
            Op::Call(argc) => {
                let n = argc as usize;
                // A Mersey-to-Mersey call does not re-enter this function: it
                // pushes a frame and keeps going round the same loop. That is
                // most of what a call used to cost — a two-thousand-line Rust
                // function's prologue, a fresh `Exec`, a fresh operand stack —
                // for a callee that needs none of it.
                let at = stack.len() - n - 1;
                let inline = match &stack[at] {
                    Value::Closure(c) => {
                        let c = c.clone();
                        i.inlinable(&c, n).map(|ch| (c, ch))
                    }
                    _ => None,
                };
                if let Some((c, cchunk)) = inline {
                    push_inline!(c, cchunk, n, at);
                    continue;
                }
                let args = split_args(stack, n);
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
                let n = argc as usize;
                let at = stack.len() - n - 1;
                // An instance method: found once per call site, then inlined like
                // any other call. It used to walk the whole of `call_member` —
                // past iterators, promises, arrays, strings — and *then* search
                // the class chain, on every call, and then re-enter the
                // interpreter. A method call is what object-oriented code is made
                // of; it cost more than twice a plain one.
                let inline = if let Value::Instance(inst) = &stack[at] {
                    let (cid, cls) = {
                        let b = inst.borrow();
                        (b.class.id, b.class.clone())
                    };
                    let slot = &chunk.method_cache[ni as usize];
                    let hit = match &*slot.borrow() {
                        Some((c, d, k)) if *c == cid => Some((d.clone(), k.clone())),
                        _ => None,
                    };
                    let found = match hit {
                        Some(x) => Some(x),
                        None => match i.method_of(&cls, &chunk.names[ni as usize]) {
                            Some((d, k)) => {
                                *slot.borrow_mut() = Some((cid, d.clone(), k.clone()));
                                Some((d, k))
                            }
                            None => None, // a field holding a closure, a getter, a host member
                        },
                    };
                    found.and_then(|(data, defining)| {
                        let c = Rc::new(Closure {
                            data,
                            env: defining.env.clone().unwrap_or_else(|| i.globals_env()),
                            this: Some(stack[at].clone()),
                            cls: Some(defining),
                        });
                        i.inlinable(&c, n).map(|ch| (c, ch))
                    })
                } else {
                    None
                };
                if let Some((c, cchunk)) = inline {
                    push_inline!(c, cchunk, n, at);
                    continue;
                }
                let args = split_args(stack, n);
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
                let slot = &chunk.class_cache[ni as usize];
                let cached = slot.borrow().clone();
                let v = match cached {
                    Some(cls) => throwing!(i.instantiate(&cls, args)),
                    None => {
                        let env = cur!().clone();
                        let name = &chunk.names[ni as usize];
                        match i.resolve_class(name, &env) {
                            Some(cls) => {
                                *slot.borrow_mut() = Some(cls.clone());
                                throwing!(i.instantiate(&cls, args))
                            }
                            // A host constructor, a `Map`, a namespace path: not
                            // a Mersey class, and not cacheable as one.
                            None => throwing!(i.new_named(name, args, &env)),
                        }
                    }
                };
                stack.push(v);
            }
            Op::NewNamedV(ni) => {
                let args = pop_array(stack);
                let env = cur!().clone();
                let v = throwing!(i.new_named(&chunk.names[ni as usize], args, &env));
                stack.push(v);
            }
            // `super` needs `this`, and `this` is a frame slot now — not a name
            // in an environment the call may not even have.
            Op::SuperCall(argc) => {
                let args = split_args(stack, argc as usize);
                let this = this_of(&chunk, frame, frame_base);
                let v = throwing!(i.super_call(args, this));
                stack.push(v);
            }
            Op::SuperMember(ni) => {
                let this = this_of(&chunk, frame, frame_base);
                let v = throwing!(i.super_lookup(&chunk.names[ni as usize], this));
                stack.push(v);
            }
            Op::CallSuperMethod(ni, argc) => {
                let args = split_args(stack, argc as usize);
                let this = this_of(&chunk, frame, frame_base);
                let v = throwing!(i.call_super_method(&chunk.names[ni as usize], args, this));
                stack.push(v);
            }
            Op::SuperCallV => {
                let args = pop_array(stack);
                let this = this_of(&chunk, frame, frame_base);
                let v = throwing!(i.super_call(args, this));
                stack.push(v);
            }
            Op::CallSuperMethodV(ni) => {
                let args = pop_array(stack);
                let this = this_of(&chunk, frame, frame_base);
                let v = throwing!(i.call_super_method(&chunk.names[ni as usize], args, this));
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
            Op::MakeMap => stack.push(crate::new_map(Vec::new())),
            Op::MakeSet => stack.push(crate::new_set(Vec::new())),
            Op::MakeBytes => stack.push(Value::Bytes(Rc::new(RefCell::new(Vec::new())))),
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
                // An arrow's `this` is the enclosing function's, which now lives
                // in the frame rather than under a name in the environment.
                let this = chunk
                    .this_slot
                    .map(|s| frame[frame_base + s as usize].clone())
                    .filter(|v| !matches!(v, Value::Null));
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
                // An array iterates **live**: the loop below this op re-reads the
                // length every pass and the element at each index, so handing the
                // array itself over — not a copy — is what `for…of` means in JS,
                // and it deletes an O(n) allocation per loop. Everything else
                // (strings, host iterables, generators, `Iterable<T>` classes) is
                // snapshotted into a fresh array, as before.
                if let Value::Array(_) = &v {
                    stack.push(v);
                } else {
                    let items: Vec<Value> = throwing!(i.iter_values(&v));
                    stack.push(crate::new_array(items));
                }
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
                let v = stack.pop().expect("ret");
                if let Some(f) = calls.pop() {
                    unwind_frame!(f);
                    stack.push(v);
                    continue;
                }
                *pc_ref = pc;
                return Ok(Flow::Done(v));
            }
            Op::ReturnNull => {
                if let Some(f) = calls.pop() {
                    unwind_frame!(f);
                    stack.push(Value::Null);
                    continue;
                }
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
            Op::MakeMap | Op::MakeSet | Op::MakeBytes => (0, 1),
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
            Op::Bin(_) | Op::BinNum(..) => (2, 1),
            Op::Un(_) | Op::Truthy | Op::Convert(_) | Op::ToDisplayStr | Op::IterArray => (1, 1),
            Op::TemplateJoin(n) => (i32::from(n), 1),
            Op::LoadSlot(slot) => {
                bounds(slot, chunk.n_slots as usize, "slot")?;
                (0, 1)
            }
            Op::StoreSlot(slot) => {
                bounds(slot, chunk.n_slots as usize, "slot")?;
                (1, 0)
            }
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
