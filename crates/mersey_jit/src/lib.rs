//! Tier 1: Cranelift JIT (ROADMAP Phase 4).
//!
//! **Every value has a type, the bytecode says what it is, and the heap has a
//! shape.** Those three facts are what this tier is built out of.
//!
//! The first subset was *homogeneous*: a kernel was all-`int32` or all-`float64`.
//! The second was *typed*, and could mix them — the checker's types reached the
//! bytecode, so a slot with a type became a register and an ordinary numeric loop
//! stopped being a function the compiler had to refuse. But it still could not
//! touch the heap, and so it could not touch **objects**: a field read, an array
//! element, a method call, each of which is a single instruction on a machine and
//! was a trip round the interpreter's dispatch loop.
//!
//! Now it can. What makes that safe is not care, it is the language:
//!
//! - **Sealed shapes (§4.1).** A class's layout is fixed when it is declared, and
//!   a subclass's layout *begins with its base's*. So a field is a constant
//!   offset — and an offset computed for `Shape` is still right on a `Circle`.
//!   There are no hidden classes, no shape transitions, and no inline-cache
//!   misses, because there is nothing to miss.
//! - **Class hierarchy analysis.** The module graph is closed (§4.5) and there is
//!   no `eval` and no prototype patching, so "does anything override `area`?" has
//!   an answer. When the answer is no, `s.area()` is a **direct call** — no
//!   vtable, no guard, no deopt. A JS engine cannot ask this question.
//! - **A heap that does not move**, which is what keeping the safe `Rc` heap
//!   bought: compiled code can hold a raw pointer to an object and have it stay
//!   valid.
//!
//! And compiled code **never touches a reference count** — it reads scalars out
//! of heap cells and writes scalars back, and any object it holds is borrowed.
//! That single rule is why there is no write barrier, nothing to root, and no way
//! for the collector to run underneath it. See `heap`.
//!
//! Accepted:
//!
//! - **int32, int64, float64, bool** in any mixture, and conversions between them;
//! - **objects**: field loads and stores at a constant offset, on `this`, on a
//!   parameter, on a field, on an array element;
//! - **arrays** of those: element loads and stores, and `.length`;
//! - **calls** — to functions, and to methods, including recursion, all direct;
//! - **the things that can fault** — `x / 0`, an index out of bounds, a field of
//!   `null`, the recursion limit — which set a status word, say *where*, and
//!   return. Guard at the entry, trap at the edge, never deopt in the middle.
//!
//! Not accepted: **allocation**. `new`, an array literal, a string — anything
//! that makes a new reference-counted value — is where the rule above stops
//! holding, and a function that does one is interpreted. That is the next
//! project, and it is a real one: ownership of a value that native code created
//! has to be tracked along every path out of it.
//!
//! Code memory is W^X: cranelift-jit maps pages writable, then flips them to
//! read-execute at finalize (spec §5.2), and the code is hardened — see
//! `hardened_isa`.

mod heap;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlags, Value as ClValue};
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use mersey_front::ast::{BinOp, UnaryOp};
use mersey_front::check::{IntKind, Num};
use mersey_interp::vm::{analyze, Chunk, Op};
use mersey_interp::{
    repr, Arena, ClassDef, FieldTy, JitArg, JitCode, JitEnv, JitFn, JitKind, JitResult, JitSlot,
    Trap, TrapReason, Value, JIT_DEPTH_LIMIT,
};

/// The type of one value. Not of the whole kernel — of one value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    I32,
    I64,
    F64,
    /// A comparison's result. It is an `i32` in a register and a `bool` when it
    /// leaves, which is the only place the difference is visible.
    Bool,
    /// An instance of the group's class `n`, **or of any subclass** — the offsets
    /// are the same either way — or null.
    Obj(u32),
    /// An array of these.
    Arr(Elem),
}

/// What an array holds. Not `Ty`, because `Ty` would have to box itself: an array
/// of arrays is a thing this tier does not do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Elem {
    I32,
    I64,
    F64,
    Bool,
    Obj(u32),
}

impl Elem {
    fn ty(self) -> Ty {
        match self {
            Elem::I32 => Ty::I32,
            Elem::I64 => Ty::I64,
            Elem::F64 => Ty::F64,
            Elem::Bool => Ty::Bool,
            Elem::Obj(c) => Ty::Obj(c),
        }
    }
}

impl Ty {
    /// A scalar's machine type.
    fn cl(self) -> types::Type {
        match self {
            Ty::I32 | Ty::Bool => types::I32,
            Ty::I64 => types::I64,
            Ty::F64 => types::F64,
            Ty::Obj(_) | Ty::Arr(_) => types::I64,
        }
    }

    fn is_int(self) -> bool {
        matches!(self, Ty::I32 | Ty::I64 | Ty::Bool)
    }

    fn is_num(self) -> bool {
        matches!(self, Ty::I32 | Ty::I64 | Ty::F64 | Ty::Bool)
    }

    /// How many machine values one of these takes.
    ///
    /// An object is *three*: its address, the address of its fields (derived from
    /// the first by a call into the engine — once, where the object comes from,
    /// not on every access), and its **arena handle** — nonzero exactly when this
    /// value owns the arena's reference to the object, which is what a freshly
    /// allocated one does and a borrowed one does not. An array is also three:
    /// address, elements, length — arrays are never owned, because compiled code
    /// cannot allocate one.
    fn width(self) -> usize {
        match self {
            Ty::Obj(_) | Ty::Arr(_) => 3,
            _ => 1,
        }
    }

    /// The machine types one of these occupies, in order.
    fn parts(self) -> Vec<types::Type> {
        match self {
            Ty::Obj(_) | Ty::Arr(_) => vec![types::I64, types::I64, types::I64],
            t => vec![t.cl()],
        }
    }

    /// Where this type's payload sits inside a cell. Not the same for all of
    /// them: `repr(u8)` aligns a payload to itself (see `mersey_interp::repr`).
    /// Objects have none — compiled code never reads a pointer out of a cell
    /// itself; it asks the engine for one. See `heap::cell_obj`.
    fn payload(self) -> i32 {
        match self {
            Ty::Bool => repr::OFF_BOOL,
            Ty::I32 => repr::OFF_I32,
            Ty::I64 => repr::OFF_I64,
            _ => repr::OFF_F64,
        }
    }

    /// The tag a heap cell of this type carries.
    fn tag(self) -> u8 {
        match self {
            Ty::I32 => repr::TAG_I32,
            Ty::I64 => repr::TAG_I64,
            Ty::F64 => repr::TAG_F64,
            Ty::Bool => repr::TAG_BOOL,
            Ty::Obj(_) => repr::TAG_INSTANCE,
            Ty::Arr(_) => repr::TAG_ARRAY,
        }
    }
}

/// The declared numeric types this tier can hold in a register.
///
/// `uint32`/`uint64` are absent: the values that cross the boundary are `I32`,
/// `I64` and `F64`, and an unsigned kernel would have to marshal through one of
/// them and get the wrong answer at the edges. `int8`/`int16` promote to `int32`
/// in arithmetic (§3.3), but a *conversion* to one has to wrap, which is not a
/// register move — so a function that needs one is interpreted.
fn ty_of(n: Num) -> Option<Ty> {
    match n {
        Num::Int(IntKind::I32) => Some(Ty::I32),
        Num::Int(IntKind::I64) => Some(Ty::I64),
        Num::F64 => Some(Ty::F64),
        _ => None,
    }
}

/// Status word shared by every frame of one compiled call.
const ST_OK: i64 = 0;
const ST_NULL: i64 = 1;
const ST_TRAP: i64 = 3;

const ST_STATUS: i32 = 0;
const ST_DEPTH: i32 = 8;
const ST_REASON: i32 = 16;
const ST_PC: i32 = 24;
const ST_FUNC: i32 = 32;
const ST_A: i32 = 40;
const ST_B: i32 = 48;
const STATE_BYTES: u32 = 56;

const R_DIV0: i64 = 0;
const R_INTMIN: i64 = 1;
const R_DEPTH: i64 = 2;
const R_BOUNDS: i64 = 3;
const R_NULL: i64 = 4;
const R_TAG: i64 = 5;

/// "Start at the top". Loop headers are bytecode positions and 0 is a legal one,
/// so the normal entry cannot be 0.
const ENTRY_START: i64 = -1;

/// Every slot is 8 bytes at the *boundary*, whatever it holds: a number, or the
/// address of an object. Inside, a slot is one to three registers.
const SLOT_BYTES: usize = 8;

/// The largest call graph compiled as one unit, so a pathological program cannot
/// make the compiler chew through the whole module at a call site.
const GROUP_MAX: usize = 48;

/// The hook the interpreter calls: compile a call graph, or refuse it.
pub fn hook(env: &dyn JitEnv, root: &JitFn) -> Option<Rc<JitCode>> {
    compile_group(env, root)
}

// ---- the group ---------------------------------------------------------------

/// One function's signature, known before its body is looked at — which is what
/// recursion requires, and what a *method* call requires twice over: the callee's
/// receiver class decides what it compiles to.
#[derive(Clone)]
struct Sig {
    params: Vec<Ty>,
    /// May be `Ty::Obj`: a function can return an object it allocated, handing
    /// its arena handle to the caller.
    ret: Ty,
    /// The function returns nothing. Its result register is a placeholder.
    void: bool,
    n_slots: usize,
    this_slot: Option<usize>,
    this: Option<Ty>,
}

/// Everything one compilation is about: the functions it reaches, the classes
/// they touch, and the questions it had to ask the engine.
struct Group<'a> {
    env: &'a dyn JitEnv,
    fns: Vec<JitFn>,
    sigs: Vec<Sig>,
    /// The classes any of it mentions. `Ty::Obj` indexes this.
    classes: Vec<Rc<ClassDef>>,
    by_key: HashMap<(usize, u64), usize>,
    /// Does anything in the group **write** to the heap?
    ///
    /// This decides what a trap means. A group that only reads can be re-run by
    /// the interpreter to produce the error — running it twice has no
    /// consequence, and the interpreter's message and stack trace are better than
    /// anything native code could build. A group that has already written to an
    /// object cannot be re-run: it would write to it again. So it reports where
    /// it stopped instead, and the error is raised from that.
    writes: bool,
}

impl Group<'_> {
    fn class_idx(&mut self, c: &Rc<ClassDef>) -> u32 {
        if let Some(i) = self.classes.iter().position(|k| k.class_id() == c.class_id()) {
            return i as u32;
        }
        self.classes.push(c.clone());
        (self.classes.len() - 1) as u32
    }

    /// A boundary type as a register type.
    fn ty_of_slot(&mut self, s: &JitSlot) -> Option<Ty> {
        Some(match s {
            JitSlot::I32 => Ty::I32,
            JitSlot::I64 => Ty::I64,
            JitSlot::F64 => Ty::F64,
            JitSlot::Obj(c) => Ty::Obj(self.class_idx(c)),
            JitSlot::Arr(e) => Ty::Arr(self.elem_of(e)?),
        })
    }

    fn elem_of(&mut self, f: &FieldTy) -> Option<Elem> {
        Some(match f {
            FieldTy::Num(Num::Int(IntKind::I32)) => Elem::I32,
            FieldTy::Num(Num::Int(IntKind::I64)) => Elem::I64,
            FieldTy::Num(Num::F64) => Elem::F64,
            FieldTy::Bool => Elem::Bool,
            FieldTy::Obj(c) => Elem::Obj(self.class_idx(c)),
            // An array of arrays, or of anything this tier has no register for.
            _ => return None,
        })
    }

    /// A field's declared type as a register type.
    fn field_ty(&mut self, cls: u32, slot: usize) -> Option<Ty> {
        let tys = self.classes[cls as usize].field_types();
        Some(match tys.get(slot)? {
            FieldTy::Num(n) => ty_of(*n)?,
            FieldTy::Bool => Ty::Bool,
            FieldTy::Obj(c) => Ty::Obj(self.class_idx(c)),
            FieldTy::Arr(e) => Ty::Arr(self.elem_of(e)?),
            FieldTy::Opaque => return None,
        })
    }

    /// Add a function to the group, or find the one already there. The same body
    /// compiled against two receiver classes is two functions: its field offsets
    /// are the same, but what *its* calls resolve to need not be.
    fn add(&mut self, f: JitFn) -> Option<usize> {
        let key = (
            Rc::as_ptr(&f.chunk) as usize,
            f.this.as_ref().map_or(0, |c| c.class_id()),
        );
        if let Some(&i) = self.by_key.get(&key) {
            return Some(i);
        }
        if self.fns.len() >= GROUP_MAX {
            return None;
        }
        let sig = self.sig_of(&f)?;
        let i = self.fns.len();
        self.by_key.insert(key, i);
        self.fns.push(f);
        self.sigs.push(sig);
        Some(i)
    }

    /// A function's signature, from its *declarations* — never from its body. A
    /// recursive call cannot wait for the callee to be compiled to find out what
    /// comes back, and neither can a mutually recursive one.
    fn sig_of(&mut self, f: &JitFn) -> Option<Sig> {
        // The frame's first slots are the parameters, in order. The compiler hands
        // them out that way; if it ever did not, the marshalling would be silently
        // wrong, so check rather than assume.
        if f.chunk.param_slots.len() != f.params.len() {
            return None;
        }
        for (i, (_, slot)) in f.chunk.param_slots.iter().enumerate() {
            if *slot as usize != i {
                return None;
            }
        }
        let mut params = Vec::with_capacity(f.params.len());
        for i in 0..f.params.len() {
            // The declared type first — it is the only thing that knows an object
            // parameter's class. A numeric one the checker already typed.
            let t = match f.param_tys.get(i).and_then(|t| t.clone()) {
                Some(s) => self.ty_of_slot(&s)?,
                None => ty_of(f.chunk.slot_types.get(i).copied().flatten()?)?,
            };
            params.push(t);
        }
        let this = f.this.as_ref().map(|c| Ty::Obj(self.class_idx(c)));
        // A method whose body never says `this` has no slot for it, and does not
        // need one.
        let this_slot = f.chunk.this_slot.map(|s| s as usize);
        let void = f.ret.is_none() && !f.ret_bool && f.ret_obj.is_none();
        let ret = if void {
            Ty::I32 // a placeholder: nothing reads it
        } else if let Some(c) = &f.ret_obj {
            Ty::Obj(self.class_idx(c))
        } else if f.ret_bool {
            Ty::Bool
        } else {
            ty_of(f.ret?)?
        };
        Some(Sig {
            params,
            ret,
            void,
            n_slots: f.chunk.n_slots as usize,
            this_slot,
            this,
        })
    }
}

// ---- planning ----------------------------------------------------------------

/// One function, planned: what every slot holds, what every value on the operand
/// stack is, what each member access resolves to, and what comes back.
struct Plan {
    chunk: Rc<Chunk>,
    n_params: usize,
    n_slots: usize,
    /// The type of each frame slot — the *same* frame the interpreter uses.
    slots: Vec<Ty>,
    /// Where each slot's registers start. A slot is not one register any more.
    var_at: Vec<u32>,
    n_vars: u32,
    /// Slots that hold a value when the function is entered normally: the
    /// parameters, and `this`.
    entry_live: Vec<bool>,
    /// Name index → position of the called function in the group.
    callee: HashMap<u16, usize>,
    /// Bytecode position → the method it calls. Keyed by position, not by name: the
    /// same name at two sites can have two receivers.
    method_at: HashMap<usize, usize>,
    /// Bytecode position → (field offset, what it holds).
    field_at: HashMap<usize, (u32, Ty)>,
    /// Bytecode position of a `new` → (class, constructor in the group).
    new_at: HashMap<usize, (u32, Option<usize>)>,
    /// Stores and returns that must **clone** their object first: the value is
    /// borrowed through a re-assignable local, and the clone is what survives
    /// that local being overwritten. See [`Prov`].
    clone_at: std::collections::HashSet<usize>,
    /// Object-typed slots the body itself stores into. Overwriting one releases
    /// the arena's reference to its old value — and at an on-stack replacement,
    /// these are the slots whose interpreter values must be cloned *into* the
    /// arena so there is something to release.
    owned_slots: Vec<bool>,
    /// Bytecode positions of `arr.length`.
    length_at: Vec<usize>,
    depths: Vec<Option<i32>>,
    /// The operand-stack types at each jump target, so a block's parameters can
    /// be given the right ones.
    block_types: HashMap<usize, Vec<Ty>>,
    ret: Ty,
    void: bool,
}

/// Where a borrowed object came from — the one fact that decides whether it can
/// dangle.
///
/// Everything compiled code borrows is owned by something that outlives the
/// borrow, with a single exception: a **re-assignable local**. Overwriting an
/// object local releases the arena's reference to the old object, and a borrow
/// taken from that local — or reached *through* it, a field of a field — would
/// point at whatever is left. So a borrow rooted in a mutable slot is marked, and
/// two things happen to it: storing it anywhere first **clones** it (its own
/// arena reference, immune to the release), and carrying it across a jump edge is
/// refused (a block parameter has no provenance).
///
/// Borrows rooted anywhere else — a parameter, `this`, an array element, a field
/// of any of those, a fresh allocation (which the arena keeps until the call
/// ends) — cannot dangle, because compiled code cannot detach them: object fields
/// and object elements are never written by compiled code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prov {
    Stable,
    /// Borrowed from (or through) this re-assignable local.
    FromSlot(u16),
}

/// A value on the abstract operand stack, in the typing pass.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TSlot {
    Val(Ty, Prov),
    /// The literal `null`. It has no type of its own and no register — the only
    /// thing that may be done with it is compare a reference against it, which is
    /// what `if (n.left != null)` is, and which object code is *made* of.
    Null,
    Callee(usize),
}

fn tval(s: TSlot) -> Option<Ty> {
    match s {
        TSlot::Val(t, _) => Some(t),
        TSlot::Null | TSlot::Callee(_) => None,
    }
}

fn prov(s: TSlot) -> Prov {
    match s {
        TSlot::Val(_, p) => p,
        _ => Prov::Stable,
    }
}

/// Work out the type of every slot and every value, resolve every field and every
/// call, and reject anything outside the subset.
fn plan(g: &mut Group, me: usize) -> Option<Plan> {
    let chunk = g.fns[me].chunk.clone();
    let sig = g.sigs[me].clone();
    let n_slots = sig.n_slots;
    let n_params = sig.params.len();

    // What the checker said each slot holds. A slot it said nothing about — a
    // compiler temp, an object — has to be given a type by the code that stores
    // into it, or by the declaration it came from.
    let mut slots: Vec<Option<Ty>> = chunk
        .slot_types
        .iter()
        .map(|t| t.and_then(ty_of))
        .collect();
    slots.resize(n_slots, None);
    let mut entry_live = vec![false; n_slots];
    for (i, t) in sig.params.iter().enumerate() {
        slots[i] = Some(*t);
        entry_live[i] = true;
    }
    if let (Some(s), Some(t)) = (sig.this_slot, sig.this) {
        if s >= n_slots {
            return None;
        }
        slots[s] = Some(t);
        entry_live[s] = true;
    } else if sig.this_slot.is_some() {
        return None; // the body says `this` and we do not know what it is
    }

    let depths = analyze(&chunk).ok()?;
    let mut callee: HashMap<u16, usize> = HashMap::new();
    let mut method_at: HashMap<usize, usize> = HashMap::new();
    let mut field_at: HashMap<usize, (u32, Ty)> = HashMap::new();
    let mut new_at: HashMap<usize, (u32, Option<usize>)> = HashMap::new();
    let mut clone_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut length_at: Vec<usize> = Vec::new();
    let mut block_types: HashMap<usize, Vec<Ty>> = HashMap::new();
    let mut ret: Option<Ty> = if sig.void { None } else { Some(sig.ret) };

    // Which slots does the body itself store into? A store inside a loop runs
    // many times, so there is no distinction worth drawing between "assigned
    // once" and "reassigned" — any stored slot's old value may be released while
    // a borrow of it is still around, and that is what `Prov::FromSlot` guards.
    let mut stored: Vec<bool> = vec![false; n_slots];
    for op in chunk.code.iter() {
        if let Op::StoreSlot(sl) = *op {
            if let Some(f) = stored.get_mut(sl as usize) {
                *f = true;
            }
        }
    }

    // The same walk the code generator does, but over types.
    let mut stack: Vec<TSlot> = Vec::new();
    let mut reachable = true;
    for (pc, op) in chunk.code.iter().enumerate() {
        if let Some(want) = block_types.get(&pc) {
            stack = want.iter().map(|t| TSlot::Val(*t, Prov::Stable)).collect();
            reachable = true;
        }
        if !reachable {
            continue;
        }
        match *op {
            Op::Const(ci) => stack.push(match &chunk.consts[ci as usize] {
                Value::I32(_) => TSlot::Val(Ty::I32, Prov::Stable),
                Value::Bool(_) => TSlot::Val(Ty::Bool, Prov::Stable),
                Value::I64(_) => TSlot::Val(Ty::I64, Prov::Stable),
                Value::F64(_) => TSlot::Val(Ty::F64, Prov::Stable),
                // `null` is a constant, not an instruction — which is where the
                // `x != null` every tree and list is written with actually comes
                // from.
                Value::Null => TSlot::Null,
                _ => return None,
            }),
            Op::LoadSlot(s) => {
                let t = slots[s as usize]?;
                // A reference loaded from a slot the body stores into is the one
                // borrow that can dangle — see `Prov`.
                let pv = match t {
                    Ty::Obj(_) | Ty::Arr(_) if stored[s as usize] => Prov::FromSlot(s),
                    _ => Prov::Stable,
                };
                stack.push(TSlot::Val(t, pv));
            }
            Op::StoreSlot(s) => {
                let top = stack.pop()?;
                let t = tval(top)?;
                match slots[s as usize] {
                    Some(k) if k != t => {
                        // A `bool` and an `int32` share a register; anything else
                        // changing type under a slot means this is not a typed
                        // frame after all.
                        if !(k.is_int() && t.is_int() && k.cl() == t.cl()) {
                            return None;
                        }
                    }
                    Some(_) => {}
                    None => slots[s as usize] = Some(t),
                }
                if let Prov::FromSlot(src) = prov(top) {
                    match t {
                        // Storing a borrow that a later overwrite could kill:
                        // clone it, so it owns an arena reference of its own.
                        Ty::Obj(_) => {
                            clone_at.insert(pc);
                        }
                        // An array cannot be cloned into the arena (its handle
                        // has nowhere to live in three registers), so this rare
                        // shape is interpreted rather than risked.
                        Ty::Arr(_) => return None,
                        _ => {
                            let _ = src;
                        }
                    }
                }
                // Overwriting `s` releases its old object. Any borrow *of that
                // slot* still in flight would be a use of whatever is left.
                if stack
                    .iter()
                    .any(|v| prov(*v) == Prov::FromSlot(s))
                {
                    return None;
                }
            }
            Op::LoadName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                let f = g.env.function(name)?;
                let idx = g.add(f)?;
                callee.insert(ni, idx);
                stack.push(TSlot::Callee(idx)); // a function, not a value
            }
            Op::StoreName(_) | Op::DeclareName(_) => return None,
            Op::Null => stack.push(TSlot::Null),

            // Allocation. The engine allocates (a shim: the instance, its literal
            // field values, its fresh containers, its GC registration), the arena
            // owns what it made, and the constructor — an ordinary method body —
            // runs compiled. The class is resolved now and forever: a class name
            // cannot be reassigned (E0304), and a *new* class arriving by dynamic
            // import discards this code wholesale.
            Op::NewNamed(ni, argc) => {
                let name = chunk.names[ni as usize].as_str();
                let cls = g.env.class_for_new(name)?;
                let ci = g.class_idx(&cls);
                let ctor = g.env.ctor(&cls)?;
                let mut args: Vec<Ty> = Vec::new();
                for _ in 0..argc {
                    args.push(tval(stack.pop()?)?);
                }
                args.reverse();
                let ctor_idx = match ctor {
                    Some(f) => {
                        let idx = g.add(f)?;
                        if g.sigs[idx].params != args {
                            return None;
                        }
                        Some(idx)
                    }
                    None => {
                        if !args.is_empty() {
                            return None;
                        }
                        None
                    }
                };
                new_at.insert(pc, (ci, ctor_idx));
                stack.push(TSlot::Val(Ty::Obj(ci), Prov::Stable));
            }

            // Untyped `Bin` reaches compiled code from two places, and both are
            // narrow enough to take.
            //
            // A reference against `null`: the comparison every tree, list and
            // optional field is written with. `null` is a null pointer here, so
            // it is one compare against zero.
            //
            // And the `for…of` lowering, which builds its own index loop —
            // `idx < items.length`, `idx + 1` — out of untyped ops the checker
            // never saw. Both operands are known `int32` by this pass, and the
            // dynamic dispatch these ops would do at Tier 0 agrees exactly with
            // the int32 kernel for these operators (§3.3 promotes two int32s to
            // int32; add wraps). Division is *not* here: it can trap, and an
            // untyped one keeps its interpreter path.
            Op::Bin(op) => {
                let r = stack.pop()?;
                let l = stack.pop()?;
                match (l, r) {
                    (TSlot::Null, TSlot::Val(t, _)) | (TSlot::Val(t, _), TSlot::Null) => {
                        if !matches!(t, Ty::Obj(_) | Ty::Arr(_))
                            || !matches!(op, BinOp::Eq | BinOp::Ne)
                        {
                            return None;
                        }
                        stack.push(TSlot::Val(Ty::Bool, Prov::Stable));
                    }
                    (TSlot::Val(a, _), TSlot::Val(b, _)) if a == b && a.is_int() => match op {
                        BinOp::Add | BinOp::Sub => stack.push(TSlot::Val(a, Prov::Stable)),
                        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                            stack.push(TSlot::Val(Ty::Bool, Prov::Stable))
                        }
                        _ => return None,
                    },
                    _ => return None,
                }
            }

            // ---- the heap ------------------------------------------------------
            Op::GetMember(ni, _) => {
                let name = chunk.names[ni as usize].as_str();
                let base = stack.pop()?;
                match tval(base)? {
                    Ty::Obj(ci) => {
                        let cls = g.classes[ci as usize].clone();
                        // A getter is a *call*, not a load. So is a member of a
                        // host-backed object. Neither is this instruction.
                        if cls.is_accessor(name) || cls.is_host_backed() {
                            return None;
                        }
                        let slot = cls.field_slot(name)?;
                        let t = g.field_ty(ci, slot as usize)?;
                        field_at.insert(pc, (slot, t));
                        // A reference reached *through* a shaky base is exactly
                        // as shaky as the base.
                        let pv = match t {
                            Ty::Obj(_) | Ty::Arr(_) => prov(base),
                            _ => Prov::Stable,
                        };
                        stack.push(TSlot::Val(t, pv));
                    }
                    Ty::Arr(_) if name == "length" => {
                        length_at.push(pc);
                        stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                    }
                    _ => return None,
                }
            }
            Op::SetMember(ni, _) => {
                let name = chunk.names[ni as usize].as_str();
                let v = tval(stack.pop()?)?;
                let Ty::Obj(ci) = tval(stack.pop()?)? else {
                    return None;
                };
                let cls = g.classes[ci as usize].clone();
                if cls.is_accessor(name) || cls.is_host_backed() {
                    return None;
                }
                let slot = cls.field_slot(name)?;
                let t = g.field_ty(ci, slot as usize)?;
                // Only a **scalar** may be stored. Storing an object would replace
                // one reference-counted value with another — an owned reference
                // released and an owned reference taken — and compiled code does
                // not do that. It is the one rule the whole design rests on.
                if !t.is_num() || !assignable(v, t) {
                    return None;
                }
                g.writes = true;
                field_at.insert(pc, (slot, t));
                stack.push(TSlot::Val(v, Prov::Stable));
            }
            // The head of a lowered `for…of`. On an array it passes the array
            // through — live iteration — which is exactly what the interpreter
            // does with it now. On anything else it would *snapshot*, which is an
            // allocation, and allocating is the boundary of this tier.
            Op::IterArray => {
                let top = stack.pop()?;
                let t = tval(top)?;
                if !matches!(t, Ty::Arr(_)) {
                    return None;
                }
                stack.push(top);
            }
            Op::IndexGet => {
                let i = tval(stack.pop()?)?;
                let base = stack.pop()?;
                let Ty::Arr(e) = tval(base)? else {
                    return None;
                };
                if !i.is_int() {
                    return None;
                }
                let pv = match e.ty() {
                    Ty::Obj(_) | Ty::Arr(_) => prov(base),
                    _ => Prov::Stable,
                };
                stack.push(TSlot::Val(e.ty(), pv));
            }
            Op::IndexSet => {
                let v = tval(stack.pop()?)?;
                let i = tval(stack.pop()?)?;
                let Ty::Arr(e) = tval(stack.pop()?)? else {
                    return None;
                };
                if !i.is_int() || !e.ty().is_num() || !assignable(v, e.ty()) {
                    return None;
                }
                g.writes = true;
                stack.push(TSlot::Val(v, Prov::Stable));
            }
            Op::CallMethod(ni, n) => {
                let name = chunk.names[ni as usize].to_string();
                let mut args: Vec<Ty> = Vec::new();
                for _ in 0..n {
                    args.push(tval(stack.pop()?)?);
                }
                args.reverse();
                let Ty::Obj(ci) = tval(stack.pop()?)? else {
                    return None;
                };
                let cls = g.classes[ci as usize].clone();
                // The whole of dispatch. If the engine will not answer, it is
                // because something below this class overrides the method — and
                // then there is no one body to call.
                let f = g.env.method(&cls, &name)?;
                let idx = g.add(f)?;
                let sig = g.sigs[idx].clone();
                if sig.params != args {
                    return None;
                }
                method_at.insert(pc, idx);
                // An object result is fresh or cloned by the callee: stable.
                stack.push(TSlot::Val(sig.ret, Prov::Stable));
            }

            Op::BinNum(op, num) => {
                let t = ty_of(num)?;
                let b = tval(stack.pop()?)?;
                let a = tval(stack.pop()?)?;
                if a != t || b != t {
                    // The checker inserted the conversions that make both
                    // operands this type. If they are not, do not guess.
                    return None;
                }
                match (op, t) {
                    (
                        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
                        Ty::I32 | Ty::I64 | Ty::F64,
                    ) => {}
                    (
                        BinOp::Rem
                        | BinOp::Shl
                        | BinOp::Shr
                        | BinOp::BitAnd
                        | BinOp::BitOr
                        | BinOp::BitXor,
                        Ty::I32 | Ty::I64,
                    ) => {}
                    (BinOp::Rem, Ty::F64) => {}
                    (BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne, _) => {}
                    _ => return None,
                }
                stack.push(TSlot::Val(
                    match op {
                        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                            Ty::Bool
                        }
                        _ => t,
                    },
                    Prov::Stable,
                ));
            }
            Op::Convert(num) => {
                let t = tval(stack.pop()?)?;
                if !t.is_num() {
                    return None;
                }
                stack.push(TSlot::Val(ty_of(num)?, Prov::Stable));
            }
            Op::Un(u) => {
                let t = tval(stack.pop()?)?;
                stack.push(TSlot::Val(
                    match (u, t) {
                        (UnaryOp::Neg, Ty::I32 | Ty::I64 | Ty::F64) => t,
                        (UnaryOp::BitNot, Ty::I32 | Ty::I64) => t,
                        (UnaryOp::Not, t) if t.is_num() => Ty::Bool,
                        _ => return None,
                    },
                    Prov::Stable,
                ));
            }
            Op::Truthy => {
                let t = tval(stack.pop()?)?;
                if !t.is_num() {
                    return None;
                }
                stack.push(TSlot::Val(Ty::Bool, Prov::Stable));
            }
            Op::Call(n) => {
                let mut args: Vec<Ty> = Vec::new();
                for _ in 0..n {
                    args.push(tval(stack.pop()?)?);
                }
                args.reverse();
                let TSlot::Callee(f) = stack.pop()? else {
                    return None; // calling a value, not a function
                };
                let sig = g.sigs[f].clone();
                if sig.params != args || sig.this.is_some() {
                    return None;
                }
                stack.push(TSlot::Val(sig.ret, Prov::Stable));
            }
            Op::Pop => {
                stack.pop()?;
            }
            Op::Dup => {
                let t = *stack.last()?;
                stack.push(t);
            }
            Op::PushScope | Op::PopScope => {}
            Op::Return => {
                let top = stack.pop()?;
                if sig.void {
                    return None;
                }
                // `return null` from an object-returning function.
                if matches!(top, TSlot::Null) {
                    if !matches!(ret, Some(Ty::Obj(_))) {
                        return None;
                    }
                    reachable = false;
                    continue;
                }
                let t = tval(top)?;
                match (ret, t) {
                    // An object leaves with the caller owning it: fresh ones hand
                    // over their arena handle, borrowed ones are cloned first.
                    // The emitter promotes a handle-less return to an owned one
                    // at runtime, so provenance needs no special case here.
                    (Some(Ty::Obj(want)), Ty::Obj(have)) => {
                        if !g.classes[have as usize].descends_from(&g.classes[want as usize]) {
                            return None;
                        }
                    }
                    (Some(k), t) if t.is_num() => {
                        if k != t && !(k.is_int() && t.is_int() && k.cl() == t.cl()) {
                            return None;
                        }
                    }
                    (None, t) if t.is_num() => ret = Some(t),
                    _ => return None,
                }
                reachable = false;
            }
            Op::ReturnNull => {
                if !sig.void {
                    return None; // a value was promised and none is being given
                }
                reachable = false;
            }
            Op::Jump(t) => {
                record_block(&mut block_types, t, &stack)?;
                reachable = false;
            }
            Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                let c = tval(stack.pop()?)?;
                if !c.is_num() {
                    return None;
                }
                record_block(&mut block_types, t, &stack)?;
            }
            _ => return None,
        }
    }

    let slots: Vec<Ty> = slots.into_iter().map(|t| t.unwrap_or(Ty::I32)).collect();
    let owned_slots: Vec<bool> = slots
        .iter()
        .zip(stored.iter())
        .map(|(t, st)| matches!(t, Ty::Obj(_)) && *st)
        .collect();
    // Where each slot's registers begin. An object or array slot is three
    // registers, so a slot number is no longer a variable number.
    let mut var_at = Vec::with_capacity(n_slots);
    let mut n = 0u32;
    for t in &slots {
        var_at.push(n);
        n += t.width() as u32;
    }
    Some(Plan {
        chunk,
        n_params,
        n_slots,
        slots,
        var_at,
        n_vars: n,
        entry_live,
        callee,
        method_at,
        field_at,
        new_at,
        clone_at,
        owned_slots,
        length_at,
        depths,
        block_types,
        ret: ret.unwrap_or(sig.ret),
        void: sig.void,
    })
}

/// May a value of type `v` be stored where a `t` is expected? Only an integer of
/// the same width may stand in for another — a `bool` is an `i32` in a register.
fn assignable(v: Ty, t: Ty) -> bool {
    v == t || (v.is_int() && t.is_int() && v.cl() == t.cl())
}

/// The operand-stack types at a jump target. Every edge into a block has to
/// agree about them — a block parameter has one type.
fn record_block(map: &mut HashMap<usize, Vec<Ty>>, target: usize, stack: &[TSlot]) -> Option<()> {
    // A callee marker cannot cross an edge: it is not a value and has nothing to
    // be passed as. Neither can a borrow rooted in a re-assignable local — a
    // block parameter has no provenance, so the guard that keeps such a borrow
    // from dangling could not follow it.
    if stack.iter().any(|v| matches!(prov(*v), Prov::FromSlot(_))) {
        return None;
    }
    let tys: Option<Vec<Ty>> = stack.iter().map(|s| tval(*s)).collect();
    let tys = tys?;
    match map.get(&target) {
        Some(have) if *have != tys => None,
        _ => {
            map.insert(target, tys);
            Some(())
        }
    }
}

/// Loop headers: backward-jump targets where the operand stack is empty.
fn loop_headers(p: &Plan) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for (pc, op) in p.chunk.code.iter().enumerate() {
        let t = match *op {
            Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => t,
            _ => continue,
        };
        if t <= pc && p.depths.get(t).copied().flatten() == Some(0) && !out.contains(&t) {
            out.push(t);
        }
    }
    out.sort_unstable();
    out
}

// ---- compiling ---------------------------------------------------------------

/// The engine functions compiled code calls to reach the heap. See `heap`.
struct Shims {
    inst_slots: FuncId,
    arr_data: FuncId,
    arr_len: FuncId,
    cell_obj: FuncId,
    cell_arr: FuncId,
    alloc: FuncId,
    clone_obj: FuncId,
    release: FuncId,
}

fn compile_group(env: &dyn JitEnv, root: &JitFn) -> Option<Rc<JitCode>> {
    let mut g = Group {
        env,
        fns: Vec::new(),
        sigs: Vec::new(),
        classes: Vec::new(),
        by_key: HashMap::new(),
        writes: false,
    };
    g.add(root.clone())?;

    // Planning function *n* can discover functions after it — a call, a method —
    // so this is a worklist, not a map.
    let mut plans: Vec<Plan> = Vec::new();
    let mut i = 0;
    while i < g.fns.len() {
        plans.push(plan(&mut g, i)?);
        i += 1;
    }

    // Nothing that touches the heap is emitted until the shape of a value has
    // been proved to be the shape this compiler assumes.
    let touches_heap = !g.classes.is_empty();
    if touches_heap && !heap::layout_holds() {
        return None;
    }


    let osr_entries = loop_headers(&plans[0]);
    let root_slots: Vec<Ty> = plans[0].slots.clone();
    let root_sig = g.sigs[0].clone();

    let isa = hardened_isa()?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    builder.symbol("msy_inst_slots", heap::inst_slots as *const u8);
    builder.symbol("msy_arr_data", heap::arr_data as *const u8);
    builder.symbol("msy_arr_len", heap::arr_len as *const u8);
    builder.symbol("msy_cell_obj", heap::cell_obj as *const u8);
    builder.symbol("msy_cell_arr", heap::cell_arr as *const u8);
    builder.symbol("msy_alloc", heap::alloc as *const u8);
    builder.symbol("msy_clone_obj", heap::clone_obj as *const u8);
    builder.symbol("msy_release", heap::release as *const u8);
    let mut module = JITModule::new(builder);
    let ptr_ty = module.target_config().pointer_type();

    let shims = declare_shims(&mut module, ptr_ty)?;

    // Declared before any is defined, so a call can name a function that has not
    // been compiled yet — which is what recursion and mutual recursion are.
    let mut ids: Vec<FuncId> = Vec::new();
    for (n, p) in plans.iter().enumerate() {
        let mut sig = module.make_signature();
        for t in &p.slots {
            for part in t.parts() {
                sig.params.push(AbiParam::new(part));
            }
        }
        sig.params.push(AbiParam::new(types::I64)); // entry_pc
        sig.params.push(AbiParam::new(ptr_ty)); // shared status/depth
        sig.params.push(AbiParam::new(ptr_ty)); // the arena
        // An object comes back as its three registers, ownership included.
        for part in p.ret.parts() {
            sig.returns.push(AbiParam::new(part));
        }
        let id = module
            .declare_function(&format!("body{n}"), Linkage::Local, &sig)
            .ok()?;
        ids.push(id);
    }

    let mut ctx = module.make_context();
    for n in 0..plans.len() {
        ctx.func.signature = module
            .declarations()
            .get_function_decl(ids[n])
            .signature
            .clone();
        let mut fbc = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            translate(
                &mut b,
                &mut module,
                &plans,
                n,
                &ids,
                &shims,
                &g.classes,
                &osr_entries,
            )?;
            b.finalize();
        }
        module.define_function(ids[n], &mut ctx).ok()?;
        module.clear_context(&mut ctx);
    }

    // The two ways in: a normal call, and a resume at a loop header.
    let call_id = wrapper(
        &mut module,
        &mut ctx,
        ids[0],
        &plans[0],
        &shims,
        ptr_ty,
        false,
    )?;
    let osr_id = wrapper(&mut module, &mut ctx, ids[0], &plans[0], &shims, ptr_ty, true)?;

    module.finalize_definitions().ok()?; // W^X flip happens here
    let call_ptr = module.get_finalized_function(call_id);
    let osr_ptr = module.get_finalized_function(osr_id);
    Box::leak(Box::new(module)); // the code pages must outlive every call into them

    let n_slots = plans[0].n_slots;
    let n_params = plans[0].n_params;
    let this_slot = root_sig.this_slot;
    let root_ret = plans[0].ret;
    let root_void = plans[0].void;
    let root_owned = plans[0].owned_slots.clone();
    let writes = g.writes;
    #[allow(clippy::missing_transmute_annotations)]
    let call_fn: extern "C" fn(*const u8, *mut u8, *mut i64, *mut Arena) -> i64 =
        unsafe { std::mem::transmute(call_ptr) };
    #[allow(clippy::missing_transmute_annotations)]
    let osr_fn: extern "C" fn(*const u8, *mut u8, *mut i64, *mut Arena, i64) -> i64 =
        unsafe { std::mem::transmute(osr_ptr) };

    // The frame the wrappers read: two 8-byte cells per slot — the value, and
    // for an arena-owned object its handle. A normal call fills the parameters
    // and `this` and leaves the rest zero (a local is not live until it is
    // declared); an OSR fills all of them.
    let marshal = move |args: &[JitArg], slot_of: &dyn Fn(usize) -> usize| -> Vec<u8> {
        let mut buf = vec![0u8; n_slots * 2 * SLOT_BYTES];
        for (i, a) in args.iter().enumerate() {
            let at = slot_of(i) * 2 * SLOT_BYTES;
            let (bytes, handle): ([u8; 8], u64) = match a {
                JitArg::I32(v) => ((*v as i64).to_ne_bytes(), 0),
                JitArg::I64(v) => (v.to_ne_bytes(), 0),
                JitArg::F64(v) => (v.to_ne_bytes(), 0),
                JitArg::Ptr(p) => ((*p as usize as u64).to_ne_bytes(), 0),
                JitArg::Owned(p, h) => ((*p as usize as u64).to_ne_bytes(), *h),
            };
            buf[at..at + 8].copy_from_slice(&bytes);
            buf[at + 8..at + 16].copy_from_slice(&handle.to_ne_bytes());
        }
        buf
    };
    let marshal_osr = marshal;

    let read = move |tag: i64, out: [u8; 16], detail: [i64; 5], arena: &mut Arena| -> JitResult {
        match tag {
            ST_NULL => JitResult::Null,
            ST_OK if root_void => JitResult::Null,
            ST_OK => match root_ret {
                Ty::I32 | Ty::Bool => {
                    JitResult::I32(i32::from_ne_bytes(out[..4].try_into().expect("4 bytes")))
                }
                Ty::I64 => JitResult::I64(i64::from_ne_bytes(out[..8].try_into().expect("8"))),
                Ty::F64 => JitResult::F64(f64::from_ne_bytes(out[..8].try_into().expect("8"))),
                // An object: the compiled code made sure it owns what it returns
                // (a borrowed one is cloned at the return site), so its handle
                // names an arena slot holding the real `Rc` — take it out before
                // the arena is cleared.
                Ty::Obj(_) => {
                    let h = u64::from_ne_bytes(out[8..16].try_into().expect("8"));
                    match arena.take(h) {
                        Some(v) => JitResult::Val(v),
                        None => JitResult::Null,
                    }
                }
                _ => JitResult::Bail,
            },
            // It ran, and it stopped. If nothing was written, the interpreter can
            // simply run the call again and raise the error itself — with a better
            // message and a real stack trace. If something *was* written, running
            // it again would write it again, so the error is built from where it
            // stopped instead.
            _ if !writes => JitResult::Bail,
            _ => JitResult::Trap(Trap {
                reason: match detail[0] {
                    R_DIV0 => TrapReason::DivZero,
                    R_INTMIN => TrapReason::IntMinOverflow,
                    R_DEPTH => TrapReason::Depth,
                    R_BOUNDS => TrapReason::Bounds,
                    R_NULL => TrapReason::NullAccess,
                    _ => TrapReason::BadTag,
                },
                pc: detail[1] as usize,
                func: detail[2] as usize,
                a: detail[3],
                b: detail[4],
            }),
        }
    };
    let read_osr = read;

    Some(Rc::new(JitCode {
        kind: match root_ret {
            Ty::I64 => JitKind::I64,
            Ty::F64 => JitKind::F64,
            _ => JitKind::I32,
        },
        slot_kinds: root_slots.iter().map(|t| boundary(*t, &g.classes)).collect(),
        this_slot,
        call: Box::new(move |args: &[JitArg], arena: &mut Arena| {
            // The arguments, then the receiver — which goes to the slot the
            // compiler gave it, *after* the parameters, not before.
            let expect = n_params + usize::from(this_slot.is_some());
            if args.len() != expect {
                return JitResult::Bail;
            }
            let buf = marshal(args, &|i| if i < n_params {
                i
            } else {
                this_slot.unwrap_or(i)
            });
            let mut out = [0u8; 16];
            let mut detail = [0i64; 5];
            let tag = call_fn(buf.as_ptr(), out.as_mut_ptr(), detail.as_mut_ptr(), arena);
            read(tag, out, detail, arena)
        }),
        osr: Box::new(move |locals: &[JitArg], entry: usize, arena: &mut Arena| {
            if locals.len() != n_slots {
                return JitResult::Bail;
            }
            let buf = marshal_osr(locals, &|i| i);
            let mut out = [0u8; 16];
            let mut detail = [0i64; 5];
            let tag = osr_fn(
                buf.as_ptr(),
                out.as_mut_ptr(),
                detail.as_mut_ptr(),
                arena,
                entry as i64,
            );
            read_osr(tag, out, detail, arena)
        }),
        n_slots,
        osr_entries,
        owned_slots: root_owned,
        chunks: g.fns.iter().map(|f| f.chunk.clone()).collect(),
        bound: g.fns.iter().filter_map(|f| f.bind.clone()).collect(),
        classes: g.classes.clone(),
        n_classes: env.n_classes(),
    }))
}

/// A register type, as the interpreter's entry guard sees it.
fn boundary(t: Ty, classes: &[Rc<ClassDef>]) -> JitSlot {
    match t {
        Ty::I64 => JitSlot::I64,
        Ty::F64 => JitSlot::F64,
        Ty::Obj(c) => JitSlot::Obj(classes[c as usize].clone()),
        // The element type is not checked at the boundary — an array's *contents*
        // are checked where they are read, one cell at a time, because that is the
        // only place it can be done without walking the whole thing.
        Ty::Arr(_) => JitSlot::Arr(Rc::new(FieldTy::Opaque)),
        _ => JitSlot::I32,
    }
}

fn declare_shims(module: &mut JITModule, ptr_ty: types::Type) -> Option<Shims> {
    let mut one = |name: &str, ret: Option<types::Type>, args: usize| -> Option<FuncId> {
        let mut sig = module.make_signature();
        for _ in 0..args {
            sig.params.push(AbiParam::new(ptr_ty));
        }
        if let Some(r) = ret {
            sig.returns.push(AbiParam::new(r));
        }
        module.declare_function(name, Linkage::Import, &sig).ok()
    };
    Some(Shims {
        inst_slots: one("msy_inst_slots", Some(ptr_ty), 1)?,
        arr_data: one("msy_arr_data", Some(ptr_ty), 1)?,
        arr_len: one("msy_arr_len", Some(types::I64), 1)?,
        // (cell, out) -> writes 2 or 3 words
        cell_obj: one("msy_cell_obj", None, 2)?,
        cell_arr: one("msy_cell_arr", None, 2)?,
        // (class, arena, out) -> writes ptr, fields, handle
        alloc: one("msy_alloc", None, 3)?,
        // (ptr, arena) -> handle
        clone_obj: one("msy_clone_obj", Some(types::I64), 2)?,
        // (arena, handle)
        release: one("msy_release", None, 2)?,
    })
}

/// The interpreter-facing entry: `(slots, out, detail) -> status`, or with
/// `entry_pc` for the OSR one. It owns the status the whole call shares.
#[allow(clippy::too_many_arguments)]
fn wrapper(
    module: &mut JITModule,
    ctx: &mut cranelift_codegen::Context,
    body: FuncId,
    root: &Plan,
    shims: &Shims,
    ptr_ty: types::Type,
    is_osr: bool,
) -> Option<FuncId> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr_ty)); // slots in (value + handle per slot)
    sig.params.push(AbiParam::new(ptr_ty)); // value out (value + handle)
    sig.params.push(AbiParam::new(ptr_ty)); // trap detail out
    sig.params.push(AbiParam::new(ptr_ty)); // the arena
    if is_osr {
        sig.params.push(AbiParam::new(types::I64)); // the loop header to resume at
    }
    sig.returns.push(AbiParam::new(types::I64)); // status
    let id = module
        .declare_function(if is_osr { "osr" } else { "call" }, Linkage::Export, &sig)
        .ok()?;
    ctx.func.signature = sig;

    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let slots_ptr = b.block_params(entry)[0];
        let out_ptr = b.block_params(entry)[1];
        let detail_ptr = b.block_params(entry)[2];
        let arena_ptr = b.block_params(entry)[3];
        let entry_pc = if is_osr {
            b.block_params(entry)[4]
        } else {
            b.ins().iconst(types::I64, ENTRY_START)
        };

        let state = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            STATE_BYTES,
            3,
        ));
        let state_ptr = b.ins().stack_addr(ptr_ty, state, 0);
        let zero = b.ins().iconst(types::I64, 0);
        for off in [ST_STATUS, ST_DEPTH, ST_REASON, ST_PC, ST_FUNC, ST_A, ST_B] {
            b.ins().store(MemFlags::trusted(), zero, state_ptr, off);
        }

        // A normal call is handed only the parameters and the receiver; the rest
        // of the frame is zero, because a local is not live until its declaration
        // runs. An OSR is handed every slot, because all of them are.
        let inst_slots = module.declare_func_in_func(shims.inst_slots, b.func);
        let arr_data = module.declare_func_in_func(shims.arr_data, b.func);
        let arr_len = module.declare_func_in_func(shims.arr_len, b.func);
        let mut args: Vec<ClValue> = Vec::with_capacity(root.n_vars as usize + 2);
        for (i, t) in root.slots.iter().enumerate() {
            let live = is_osr || root.entry_live[i];
            if !live {
                for part in t.parts() {
                    args.push(match part {
                        types::F64 => b.ins().f64const(0.0),
                        p => b.ins().iconst(p, 0),
                    });
                }
                continue;
            }
            let at = (i * 2 * SLOT_BYTES) as i32;
            match t {
                Ty::Obj(_) => {
                    let p = b.ins().load(ptr_ty, MemFlags::trusted(), slots_ptr, at);
                    // Where its fields live. Once, here — not on every access.
                    let call = b.ins().call(inst_slots, &[p]);
                    let base = b.inst_results(call)[0];
                    // The handle: nonzero only from an OSR, whose interpreter
                    // locals were cloned into the arena so the compiled releases
                    // have something real to let go of.
                    let h = b.ins().load(types::I64, MemFlags::trusted(), slots_ptr, at + 8);
                    args.push(p);
                    args.push(base);
                    args.push(h);
                }
                Ty::Arr(_) => {
                    let p = b.ins().load(ptr_ty, MemFlags::trusted(), slots_ptr, at);
                    let d = b.ins().call(arr_data, &[p]);
                    let data = b.inst_results(d)[0];
                    let l = b.ins().call(arr_len, &[p]);
                    let len = b.inst_results(l)[0];
                    args.push(p);
                    args.push(data);
                    args.push(len);
                }
                t => args.push(b.ins().load(t.cl(), MemFlags::trusted(), slots_ptr, at)),
            }
        }
        args.push(entry_pc);
        args.push(state_ptr);
        args.push(arena_ptr);

        let fref = module.declare_func_in_func(body, b.func);
        let call = b.ins().call(fref, &args);
        let status = b
            .ins()
            .load(types::I64, MemFlags::trusted(), state_ptr, ST_STATUS);
        match root.ret {
            // An object result: its address, and the handle that owns it.
            Ty::Obj(_) => {
                let rp = b.inst_results(call)[0];
                let rh = b.inst_results(call)[2];
                b.ins().store(MemFlags::trusted(), rp, out_ptr, 0);
                b.ins().store(MemFlags::trusted(), rh, out_ptr, 8);
            }
            _ => {
                let result = b.inst_results(call)[0];
                b.ins().store(MemFlags::trusted(), result, out_ptr, 0);
            }
        }
        // Why it stopped, if it did: reason, position, function, and the two
        // numbers a bounds message needs.
        for (n, off) in [ST_REASON, ST_PC, ST_FUNC, ST_A, ST_B].iter().enumerate() {
            let v = b
                .ins()
                .load(types::I64, MemFlags::trusted(), state_ptr, *off);
            b.ins()
                .store(MemFlags::trusted(), v, detail_ptr, (n * 8) as i32);
        }
        b.ins().return_(&[status]);
        b.seal_all_blocks();
        b.finalize();
    }
    module.define_function(id, ctx).ok()?;
    module.clear_context(ctx);
    Some(id)
}

/// A value on the operand stack: one to three machine values, and what they mean.
#[derive(Clone, Copy)]
enum SlotV {
    Val(ClValue, Ty),
    /// An object: its address, the address of its fields, and its arena handle —
    /// nonzero exactly when *this value* owns the arena's reference. Copies of a
    /// value (a slot load, a `Dup`, a field read) carry handle 0: a handle lives
    /// in one place, which is what makes releasing it never a double-free.
    Obj(ClValue, ClValue, ClValue),
    /// An array: its address, the address of its elements, and how many.
    Arr(ClValue, ClValue, ClValue, Elem),
    /// The literal `null`. See `TSlot::Null`.
    Null,
    Callee(usize),
}

impl SlotV {
    /// The machine values, in the order the signature expects them.
    fn parts(self) -> Vec<ClValue> {
        match self {
            SlotV::Val(v, _) => vec![v],
            SlotV::Obj(p, b, h) => vec![p, b, h],
            SlotV::Arr(p, d, l, _) => vec![p, d, l],
            SlotV::Null | SlotV::Callee(_) => Vec::new(),
        }
    }

    /// Its address, if it is something that has one.
    fn addr(self) -> Option<ClValue> {
        match self {
            SlotV::Obj(p, _, _) => Some(p),
            SlotV::Arr(p, _, _, _) => Some(p),
            _ => None,
        }
    }

    /// Its arena handle, if it could own one.
    fn handle(self) -> Option<ClValue> {
        match self {
            SlotV::Obj(_, _, h) => Some(h),
            _ => None,
        }
    }
}

fn scalar(s: SlotV) -> Option<(ClValue, Ty)> {
    match s {
        SlotV::Val(v, t) => Some((v, t)),
        _ => None,
    }
}

fn flatten(stack: &[SlotV]) -> Option<Vec<ClValue>> {
    let mut out = Vec::new();
    for s in stack {
        // Neither a callee nor a bare `null` is a value: there is nothing for a
        // block parameter to carry it in.
        if matches!(s, SlotV::Callee(_) | SlotV::Null) {
            return None;
        }
        out.extend(s.parts());
    }
    Some(out)
}

/// Rebuild the abstract stack from a block's parameters.
fn unflatten(vals: &[ClValue], tys: &[Ty]) -> Vec<SlotV> {
    let mut out = Vec::with_capacity(tys.len());
    let mut i = 0;
    for t in tys {
        out.push(match *t {
            Ty::Obj(_) => SlotV::Obj(vals[i], vals[i + 1], vals[i + 2]),
            Ty::Arr(e) => SlotV::Arr(vals[i], vals[i + 1], vals[i + 2], e),
            t => SlotV::Val(vals[i], t),
        });
        i += t.width();
    }
    out
}

/// Lower one function's bytecode. The types are already known — `plan` worked
/// them out — so this only has to emit them.
#[allow(clippy::too_many_arguments)]
fn translate(
    b: &mut FunctionBuilder,
    module: &mut JITModule,
    plans: &[Plan],
    me: usize,
    ids: &[FuncId],
    shims: &Shims,
    class_rcs: &[Rc<ClassDef>],
    osr_entries: &[usize],
) -> Option<()> {
    let p = &plans[me];
    let is_root = me == 0;
    let chunk = p.chunk.clone();

    let shim = ShimRefs {
        cell_obj: module.declare_func_in_func(shims.cell_obj, b.func),
        cell_arr: module.declare_func_in_func(shims.cell_arr, b.func),
        alloc: module.declare_func_in_func(shims.alloc, b.func),
        clone_obj: module.declare_func_in_func(shims.clone_obj, b.func),
        release: module.declare_func_in_func(shims.release, b.func),
        scratch: b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            24,
            3,
        )),
    };

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    // One variable per machine value, not per slot: an object is two of them.
    {
        let mut k = 0usize;
        for (i, t) in p.slots.iter().enumerate() {
            for (j, part) in t.parts().into_iter().enumerate() {
                let var = Variable::from_u32(p.var_at[i] + j as u32);
                b.declare_var(var, part);
                let v = b.block_params(entry)[k];
                b.def_var(var, v);
                k += 1;
            }
        }
    }
    let entry_pc = b.block_params(entry)[p.n_vars as usize];
    let state_ptr = b.block_params(entry)[p.n_vars as usize + 1];
    let arena_ptr = b.block_params(entry)[p.n_vars as usize + 2];

    // A block at every jump target, its parameters typed by `plan`.
    let mut blocks: HashMap<usize, cranelift_codegen::ir::Block> = HashMap::new();
    for (&t, tys) in &p.block_types {
        let blk = b.create_block();
        for ty in tys {
            for part in ty.parts() {
                b.append_block_param(blk, part);
            }
        }
        blocks.insert(t, blk);
    }

    // Where a trap lands: the status word already says why, and where.
    let bail = b.create_block();

    let start = match blocks.get(&0) {
        Some(&blk) => blk,
        None => {
            let blk = b.create_block();
            blocks.insert(0, blk);
            blk
        }
    };
    if is_root && !osr_entries.is_empty() {
        for &e in osr_entries {
            let hit = b.ins().icmp_imm(IntCC::Equal, entry_pc, e as i64);
            let next = b.create_block();
            b.ins().brif(hit, blocks[&e], &[], next, &[]);
            b.switch_to_block(next);
            b.seal_block(next);
        }
    }
    b.ins().jump(start, &[]);

    let ctx = Ctx {
        state: state_ptr,
        bail,
        me,
    };

    let mut stack: Vec<SlotV> = Vec::new();
    let mut reachable = false;

    for (pc, op) in chunk.code.iter().enumerate() {
        if let Some(&blk) = blocks.get(&pc) {
            if reachable {
                let args = flatten(&stack)?;
                b.ins().jump(blk, &args);
            }
            b.switch_to_block(blk);
            let tys = p.block_types.get(&pc).cloned().unwrap_or_default();
            let params: Vec<ClValue> = b.block_params(blk).to_vec();
            stack = unflatten(&params, &tys);
            reachable = true;
        }
        if !reachable {
            continue;
        }
        match *op {
            Op::Const(ci) => {
                let s = match &chunk.consts[ci as usize] {
                    Value::I32(n) => SlotV::Val(b.ins().iconst(types::I32, *n as i64), Ty::I32),
                    Value::Bool(t) => SlotV::Val(b.ins().iconst(types::I32, *t as i64), Ty::Bool),
                    Value::I64(n) => SlotV::Val(b.ins().iconst(types::I64, *n), Ty::I64),
                    Value::F64(f) => SlotV::Val(b.ins().f64const(*f), Ty::F64),
                    Value::Null => SlotV::Null,
                    _ => unreachable!("plan"),
                };
                stack.push(s);
            }
            Op::LoadSlot(s) => {
                let t = p.slots[s as usize];
                let at = p.var_at[s as usize];
                let v = |j: u32, b: &mut FunctionBuilder| b.use_var(Variable::from_u32(at + j));
                stack.push(match t {
                    // A load is a borrow: the slot keeps its handle, the copy
                    // carries none.
                    Ty::Obj(_) => {
                        let ptr = v(0, b);
                        let fields = v(1, b);
                        let zero = b.ins().iconst(types::I64, 0);
                        SlotV::Obj(ptr, fields, zero)
                    }
                    Ty::Arr(e) => SlotV::Arr(v(0, b), v(1, b), v(2, b), e),
                    t => SlotV::Val(v(0, b), t),
                });
            }
            Op::StoreSlot(s) => {
                let mut v = stack.pop()?;
                let at = p.var_at[s as usize];
                if let SlotV::Obj(ptr, fields, h) = v {
                    // A borrow that a later overwrite of its *source* slot could
                    // kill: give it an arena reference of its own first.
                    let h = if p.clone_at.contains(&pc) {
                        let c = b.ins().call(shim.clone_obj, &[ptr, arena_ptr]);
                        b.inst_results(c)[0]
                    } else {
                        h
                    };
                    // Overwriting this slot is where its old object is let go —
                    // this, and the sweep at the end of the call, are the entire
                    // memory story. Releasing a borrow (handle 0) is a no-op.
                    let old = b.use_var(Variable::from_u32(p.var_at[s as usize] + 2));
                    release_if_owned(b, shim.release, arena_ptr, old);
                    v = SlotV::Obj(ptr, fields, h);
                }
                for (j, part) in v.parts().into_iter().enumerate() {
                    b.def_var(Variable::from_u32(at + j as u32), part);
                }
            }
            // The only name left in a compiled function: the one it calls.
            Op::LoadName(ni) => stack.push(SlotV::Callee(*p.callee.get(&ni)?)),
            Op::Null => stack.push(SlotV::Null),
            Op::Bin(binop) => {
                let r = stack.pop()?;
                let l = stack.pop()?;
                match (l, r) {
                    (SlotV::Null, x) | (x, SlotV::Null) => {
                        let ptr = x.addr()?;
                        let cc = match binop {
                            BinOp::Eq => IntCC::Equal,
                            BinOp::Ne => IntCC::NotEqual,
                            _ => return None,
                        };
                        let c = b.ins().icmp_imm(cc, ptr, 0);
                        let v = b.ins().uextend(types::I32, c);
                        stack.push(SlotV::Val(v, Ty::Bool));
                    }
                    (SlotV::Val(lv, lt), SlotV::Val(rv, _)) => {
                        // Two identical int types — `plan` proved it.
                        let (v, rt) = lower_bin(b, binop, lv, rv, lt);
                        stack.push(SlotV::Val(v, rt));
                    }
                    _ => return None,
                }
            }

            // ---- the heap ------------------------------------------------------
            Op::GetMember(_, _) if p.length_at.contains(&pc) => {
                let SlotV::Arr(_, _, len, _) = stack.pop()? else {
                    return None;
                };
                // A null array has no length: reading one is the same `TypeError`
                // the interpreter raises, not a number.
                let null = b.ins().icmp_imm(IntCC::SignedLessThan, len, 0);
                guard(b, ctx, null, R_NULL, pc, None);
                stack.push(SlotV::Val(b.ins().ireduce(types::I32, len), Ty::I32));
            }
            Op::GetMember(_, _) => {
                let (slot, t) = *p.field_at.get(&pc)?;
                let SlotV::Obj(_, base, _) = stack.pop()? else {
                    return None;
                };
                let null = b.ins().icmp_imm(IntCC::Equal, base, 0);
                guard(b, ctx, null, R_NULL, pc, None);
                let at = (slot as usize * repr::SIZE) as i32;
                let v = load_cell(b, ctx, pc, base, at, t, &shim);
                stack.push(v);
            }
            Op::SetMember(_, _) => {
                let (slot, t) = *p.field_at.get(&pc)?;
                let (v, _) = scalar(stack.pop()?)?;
                let SlotV::Obj(_, base, _) = stack.pop()? else {
                    return None;
                };
                let null = b.ins().icmp_imm(IntCC::Equal, base, 0);
                guard(b, ctx, null, R_NULL, pc, None);
                let at = (slot as usize * repr::SIZE) as i32;
                store_cell(b, ctx, pc, base, at, t, v);
                stack.push(SlotV::Val(v, t));
            }
            // Live iteration: the array itself, not a copy.
            Op::IterArray => {}
            Op::IndexGet => {
                let (i, it) = scalar(stack.pop()?)?;
                let SlotV::Arr(_, data, len, e) = stack.pop()? else {
                    return None;
                };
                let at = elem_addr(b, ctx, pc, data, len, i, it);
                let v = load_cell(b, ctx, pc, at, 0, e.ty(), &shim);
                stack.push(v);
            }
            Op::IndexSet => {
                let (v, t) = scalar(stack.pop()?)?;
                let (i, it) = scalar(stack.pop()?)?;
                let SlotV::Arr(_, data, len, e) = stack.pop()? else {
                    return None;
                };
                let at = elem_addr(b, ctx, pc, data, len, i, it);
                store_cell(b, ctx, pc, at, 0, e.ty(), v);
                stack.push(SlotV::Val(v, t));
            }

            Op::Call(n) | Op::CallMethod(_, n) => {
                let is_method = matches!(op, Op::CallMethod(..));
                let f = if is_method {
                    *p.method_at.get(&pc)?
                } else {
                    0 // filled below from the callee marker
                };
                let mut args: Vec<SlotV> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    args.push(stack.pop()?);
                }
                args.reverse();
                let (f, this) = if is_method {
                    let recv = stack.pop()?;
                    let SlotV::Obj(_, base, _) = recv else {
                        return None;
                    };
                    let null = b.ins().icmp_imm(IntCC::Equal, base, 0);
                    guard(b, ctx, null, R_NULL, pc, None);
                    (f, Some(recv))
                } else {
                    match stack.pop()? {
                        SlotV::Callee(f) => (f, None),
                        _ => return None,
                    }
                };

                // Recursion is bounded here, not by the hardware. Native frames
                // would otherwise run off the end of the stack, and the guard page
                // would turn a `RangeError` the language promises into a crash.
                let depth = b
                    .ins()
                    .load(types::I64, MemFlags::trusted(), state_ptr, ST_DEPTH);
                let deeper = b.ins().iadd_imm(depth, 1);
                let over = b
                    .ins()
                    .icmp_imm(IntCC::SignedGreaterThan, deeper, JIT_DEPTH_LIMIT);
                guard(b, ctx, over, R_DEPTH, pc, None);
                b.ins()
                    .store(MemFlags::trusted(), deeper, state_ptr, ST_DEPTH);

                // The callee's frame, slot by slot: its parameters, its receiver
                // where *it* put it, and zeros for the locals it has not declared.
                // An object crosses as a **borrow** — handle 0 — whoever owned it
                // before the call still owns it after; a callee that wants to keep
                // one takes its own reference.
                let callee = &plans[f];
                let borrowed = |b: &mut FunctionBuilder, v: SlotV| -> Vec<ClValue> {
                    match v {
                        SlotV::Obj(ptr, fields, _) => {
                            let zero = b.ins().iconst(types::I64, 0);
                            vec![ptr, fields, zero]
                        }
                        other => other.parts(),
                    }
                };
                let mut cargs: Vec<ClValue> = Vec::with_capacity(callee.n_vars as usize + 3);
                for (k, t) in callee.slots.iter().enumerate() {
                    if k < args.len() {
                        let a = borrowed(b, args[k]);
                        cargs.extend(a);
                    } else if Some(k) == callee_this_slot(callee) && this.is_some() {
                        let a = borrowed(b, this.expect("a receiver"));
                        cargs.extend(a);
                    } else {
                        for part in t.parts() {
                            cargs.push(match part {
                                types::F64 => b.ins().f64const(0.0),
                                q => b.ins().iconst(q, 0),
                            });
                        }
                    }
                }
                let start_pc = b.ins().iconst(types::I64, ENTRY_START);
                cargs.push(start_pc);
                cargs.push(state_ptr);
                cargs.push(arena_ptr);

                let fref = module.declare_func_in_func(ids[f], b.func);
                let call = b.ins().call(fref, &cargs);
                let results: Vec<ClValue> = b.inst_results(call).to_vec();
                b.ins()
                    .store(MemFlags::trusted(), depth, state_ptr, ST_DEPTH);

                // Owned temporaries passed as arguments are done with: the callee
                // borrowed them, and nothing of ours refers to them any more. Let
                // them go now rather than at the end of the call — a loop that
                // allocates its arguments must not hold every iteration's.
                for a in args.iter().chain(this.iter()) {
                    if let Some(h) = a.handle() {
                        release_if_owned(b, shim.release, arena_ptr, h);
                    }
                }

                // The callee may have stopped. Its result is not a value, and this
                // call cannot continue.
                let status = b
                    .ins()
                    .load(types::I64, MemFlags::trusted(), state_ptr, ST_STATUS);
                let failed = b.ins().icmp_imm(IntCC::NotEqual, status, ST_OK);
                let cont = b.create_block();
                b.ins().brif(failed, bail, &[], cont, &[]);
                b.switch_to_block(cont);
                b.seal_block(cont);
                match callee.ret {
                    // An object comes back owned: the callee cloned or allocated
                    // it, and its handle is now this frame's to spend.
                    Ty::Obj(_) => stack.push(SlotV::Obj(results[0], results[1], results[2])),
                    t => stack.push(SlotV::Val(results[0], t)),
                }
            }
            // Allocation: the engine makes the instance (fields folded, containers
            // fresh, GC informed), the arena owns it, and the constructor runs as
            // an ordinary compiled call on the new object.
            Op::NewNamed(_, n) => {
                let (ci, ctor) = *p.new_at.get(&pc)?;
                let mut args: Vec<SlotV> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    args.push(stack.pop()?);
                }
                args.reverse();

                // The class, baked in. `JitCode::classes` keeps it alive for as
                // long as this code exists; a class binding cannot be reassigned
                // (E0304), and a class *added* later discards the code.
                let cls_ptr = b.ins().iconst(
                    types::I64,
                    Rc::as_ptr(&class_rcs[ci as usize]) as i64,
                );
                let out = b.ins().stack_addr(types::I64, shim.scratch, 0);
                b.ins().call(shim.alloc, &[cls_ptr, arena_ptr, out]);
                let ptr = b.ins().load(types::I64, MemFlags::trusted(), out, 0);
                let fields = b.ins().load(types::I64, MemFlags::trusted(), out, 8);
                let handle = b.ins().load(types::I64, MemFlags::trusted(), out, 16);

                if let Some(f) = ctor {
                    // The constructor is a method call on the fresh object,
                    // bounded like any other call.
                    let depth = b
                        .ins()
                        .load(types::I64, MemFlags::trusted(), state_ptr, ST_DEPTH);
                    let deeper = b.ins().iadd_imm(depth, 1);
                    let over = b
                        .ins()
                        .icmp_imm(IntCC::SignedGreaterThan, deeper, JIT_DEPTH_LIMIT);
                    guard(b, ctx, over, R_DEPTH, pc, None);
                    b.ins()
                        .store(MemFlags::trusted(), deeper, state_ptr, ST_DEPTH);

                    let callee = &plans[f];
                    let zero = b.ins().iconst(types::I64, 0);
                    let mut cargs: Vec<ClValue> = Vec::with_capacity(callee.n_vars as usize + 3);
                    for (k, t) in callee.slots.iter().enumerate() {
                        if k < args.len() {
                            let a = match args[k] {
                                SlotV::Obj(p2, f2, _) => vec![p2, f2, zero],
                                other => other.parts(),
                            };
                            cargs.extend(a);
                        } else if Some(k) == callee_this_slot(callee) {
                            cargs.extend([ptr, fields, zero]);
                        } else {
                            for part in t.parts() {
                                cargs.push(match part {
                                    types::F64 => b.ins().f64const(0.0),
                                    q => b.ins().iconst(q, 0),
                                });
                            }
                        }
                    }
                    let start_pc = b.ins().iconst(types::I64, ENTRY_START);
                    cargs.push(start_pc);
                    cargs.push(state_ptr);
                    cargs.push(arena_ptr);
                    let fref = module.declare_func_in_func(ids[f], b.func);
                    b.ins().call(fref, &cargs);
                    b.ins()
                        .store(MemFlags::trusted(), depth, state_ptr, ST_DEPTH);
                    for a in &args {
                        if let Some(h) = a.handle() {
                            release_if_owned(b, shim.release, arena_ptr, h);
                        }
                    }
                    let status = b
                        .ins()
                        .load(types::I64, MemFlags::trusted(), state_ptr, ST_STATUS);
                    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, ST_OK);
                    let cont = b.create_block();
                    b.ins().brif(failed, bail, &[], cont, &[]);
                    b.switch_to_block(cont);
                    b.seal_block(cont);
                }
                stack.push(SlotV::Obj(ptr, fields, handle));
            }
            Op::Pop => {
                // A discarded expression result. If it owned its object — a
                // `new` used as a statement, a method result nobody read — the
                // reference goes with it. Nothing can have borrowed from an
                // unconsumed temporary, so this cannot strand anyone.
                if let Some(h) = stack.pop()?.handle() {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
            }
            Op::Dup => {
                let s = *stack.last()?;
                match s {
                    // Duplicating an object duplicates its *ownership*: the two
                    // copies part ways — one is stored, the other discarded, in
                    // either order — and each must survive the other's release.
                    // A handle-0 copy here was a real use-after-free: `acc =
                    // acc.add(v)` stored the borrow and released the owned
                    // original out from under it. Borrows (handle 0) still copy
                    // for free — the branch only clones what is owned.
                    // Duplicating an object duplicates its *ownership*: the two
                    // copies part ways — one is stored, the other discarded, in
                    // either order — and each must survive the other's release.
                    // A handle-0 copy here was a real use-after-free: `acc =
                    // acc.add(v)` stored the borrow and released the owned
                    // original out from under it. Borrows (handle 0) still copy
                    // for free — the branch only clones what is owned.
                    SlotV::Obj(ptr, fields, h) => {
                        let owned = b.ins().icmp_imm(IntCC::NotEqual, h, 0);
                        let take = b.create_block();
                        let done = b.create_block();
                        b.append_block_param(done, types::I64);
                        let zero = b.ins().iconst(types::I64, 0);
                        b.ins().brif(owned, take, &[], done, &[zero]);
                        b.switch_to_block(take);
                        b.seal_block(take);
                        let cl = b.ins().call(shim.clone_obj, &[ptr, arena_ptr]);
                        let cloned = b.inst_results(cl)[0];
                        b.ins().jump(done, &[cloned]);
                        b.switch_to_block(done);
                        b.seal_block(done);
                        let h2 = b.block_params(done)[0];
                        stack.push(SlotV::Obj(ptr, fields, h2));
                    }
                    other => stack.push(other),
                }
            }
            Op::PushScope | Op::PopScope => {}
            Op::Convert(num) => {
                let (v, from) = scalar(stack.pop()?)?;
                let to = ty_of(num)?;
                let out = convert(b, v, from, to);
                stack.push(SlotV::Val(out, to));
            }
            Op::BinNum(binop, num) => {
                let t = ty_of(num)?;
                let (r, _) = scalar(stack.pop()?)?;
                let (l, _) = scalar(stack.pop()?)?;
                // Integer division can fault (spec §3.6).
                if t.is_int() && matches!(binop, BinOp::Div | BinOp::Rem) {
                    let zero = b.ins().icmp_imm(IntCC::Equal, r, 0);
                    guard(b, ctx, zero, R_DIV0, pc, None);
                    let int_min = if t == Ty::I64 {
                        i64::MIN
                    } else {
                        i32::MIN as i64
                    };
                    let min = b.ins().icmp_imm(IntCC::Equal, l, int_min);
                    let neg1 = b.ins().icmp_imm(IntCC::Equal, r, -1);
                    let overflow = b.ins().band(min, neg1);
                    guard(b, ctx, overflow, R_INTMIN, pc, None);
                    let v = if binop == BinOp::Div {
                        b.ins().sdiv(l, r)
                    } else {
                        b.ins().srem(l, r)
                    };
                    stack.push(SlotV::Val(v, t));
                } else {
                    let (v, rt) = lower_bin(b, binop, l, r, t);
                    stack.push(SlotV::Val(v, rt));
                }
            }
            Op::Un(u) => {
                let (v, t) = scalar(stack.pop()?)?;
                let out = match (u, t) {
                    (UnaryOp::Neg, Ty::F64) => SlotV::Val(b.ins().fneg(v), t),
                    (UnaryOp::Neg, _) => SlotV::Val(b.ins().ineg(v), t),
                    (UnaryOp::BitNot, _) => SlotV::Val(b.ins().bnot(v), t),
                    (UnaryOp::Not, _) => {
                        let c = truthy(b, v, t);
                        let n = b.ins().icmp_imm(IntCC::Equal, c, 0);
                        SlotV::Val(b.ins().uextend(types::I32, n), Ty::Bool)
                    }
                    _ => return None,
                };
                stack.push(out);
            }
            Op::Truthy => {
                let (v, t) = scalar(stack.pop()?)?;
                let c = truthy(b, v, t);
                stack.push(SlotV::Val(c, Ty::Bool));
            }
            Op::Jump(t) => {
                let args = flatten(&stack)?;
                b.ins().jump(blocks[&t], &args);
                reachable = false;
            }
            Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                let (v, vt) = scalar(stack.pop()?)?;
                let cond = truthy(b, v, vt);
                let fall = b.create_block();
                let taken = flatten(&stack)?;
                if matches!(op, Op::JumpIfFalse(_)) {
                    b.ins().brif(cond, fall, &[], blocks[&t], &taken);
                } else {
                    b.ins().brif(cond, blocks[&t], &taken, fall, &[]);
                }
                b.switch_to_block(fall);
                b.seal_block(fall);
            }
            Op::Return => match stack.pop()? {
                // An object leaves owned. A fresh one hands over its handle; a
                // borrow takes an arena reference of its own first — the caller
                // cannot be given something it would have no way to keep.
                SlotV::Obj(ptr, fields, h) => {
                    // A real branch, not a `select`: the clone must happen only
                    // on the borrowed path, or every owned return would leave an
                    // extra reference in the arena — one per call, for the whole
                    // life of a loop.
                    let no_handle = b.ins().icmp_imm(IntCC::Equal, h, 0);
                    let is_real = b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
                    let promote = b.ins().band(no_handle, is_real);
                    let borrow = b.create_block();
                    let done = b.create_block();
                    b.append_block_param(done, types::I64);
                    b.ins().brif(promote, borrow, &[], done, &[h]);
                    b.switch_to_block(borrow);
                    b.seal_block(borrow);
                    let cl = b.ins().call(shim.clone_obj, &[ptr, arena_ptr]);
                    let cloned = b.inst_results(cl)[0];
                    b.ins().jump(done, &[cloned]);
                    b.switch_to_block(done);
                    b.seal_block(done);
                    let h = b.block_params(done)[0];
                    b.ins().return_(&[ptr, fields, h]);
                    reachable = false;
                }
                SlotV::Null if matches!(p.ret, Ty::Obj(_)) => {
                    let z = b.ins().iconst(types::I64, 0);
                    b.ins().return_(&[z, z, z]);
                    reachable = false;
                }
                v => {
                    let (v, _) = scalar(v)?;
                    b.ins().return_(&[v]);
                    reachable = false;
                }
            },
            Op::ReturnNull => {
                ret_null(b, p, state_ptr);
                reachable = false;
            }
            _ => unreachable!("plan filtered"),
        }
    }
    if reachable {
        ret_null(b, p, state_ptr);
    }

    b.switch_to_block(bail);
    let zs = zeros_of(b, p.ret);
    b.ins().return_(&zs);

    b.seal_all_blocks();
    Some(())
}

/// A `void` function returns nothing, and says so only at the root — an inner
/// call's result is a placeholder nobody reads, and marking the *shared* status
/// would tell the caller its own call had produced no value.
fn ret_null(b: &mut FunctionBuilder, p: &Plan, state_ptr: ClValue) {
    if !p.void {
        let n = b.ins().iconst(types::I64, ST_NULL);
        b.ins().store(MemFlags::trusted(), n, state_ptr, ST_STATUS);
    }
    let zs = zeros_of(b, p.ret);
    b.ins().return_(&zs);
}

/// Release an arena handle, if there is one. The zero test is inlined — a
/// handle is usually 0 (a borrow), and a C call per borrowed store is what this
/// branch buys back.
fn release_if_owned(
    b: &mut FunctionBuilder,
    release: cranelift_codegen::ir::FuncRef,
    arena_ptr: ClValue,
    h: ClValue,
) {
    let owned = b.ins().icmp_imm(IntCC::NotEqual, h, 0);
    let doit = b.create_block();
    let done = b.create_block();
    b.ins().brif(owned, doit, &[], done, &[]);
    b.switch_to_block(doit);
    b.seal_block(doit);
    b.ins().call(release, &[arena_ptr, h]);
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    b.seal_block(done);
}

/// A zero for every register the return type occupies.
fn zeros_of(b: &mut FunctionBuilder, t: Ty) -> Vec<ClValue> {
    t.parts()
        .into_iter()
        .map(|part| match part {
            types::F64 => b.ins().f64const(0.0),
            q => b.ins().iconst(q, 0),
        })
        .collect()
}

fn callee_this_slot(p: &Plan) -> Option<usize> {
    p.chunk.this_slot.map(|s| s as usize)
}

/// What a trap needs to know: where to write why, and where to go.
#[derive(Clone, Copy)]
struct Ctx {
    state: ClValue,
    bail: cranelift_codegen::ir::Block,
    me: usize,
}

/// The engine functions a compiled body may call, resolved.
struct ShimRefs {
    cell_obj: cranelift_codegen::ir::FuncRef,
    cell_arr: cranelift_codegen::ir::FuncRef,
    alloc: cranelift_codegen::ir::FuncRef,
    clone_obj: cranelift_codegen::ir::FuncRef,
    release: cranelift_codegen::ir::FuncRef,
    /// Where the engine writes an object's address and its fields back to.
    scratch: cranelift_codegen::ir::StackSlot,
}

/// Stop here, and say why. `a`/`b` carry the index and the length, for the one
/// message that needs them.
fn trap(
    b: &mut FunctionBuilder,
    ctx: Ctx,
    why: i64,
    pc: usize,
    ab: Option<(ClValue, ClValue)>,
) {
    let st = b.ins().iconst(types::I64, ST_TRAP);
    b.ins().store(MemFlags::trusted(), st, ctx.state, ST_STATUS);
    let r = b.ins().iconst(types::I64, why);
    b.ins().store(MemFlags::trusted(), r, ctx.state, ST_REASON);
    let pcv = b.ins().iconst(types::I64, pc as i64);
    b.ins().store(MemFlags::trusted(), pcv, ctx.state, ST_PC);
    let f = b.ins().iconst(types::I64, ctx.me as i64);
    b.ins().store(MemFlags::trusted(), f, ctx.state, ST_FUNC);
    if let Some((av, bv)) = ab {
        b.ins().store(MemFlags::trusted(), av, ctx.state, ST_A);
        b.ins().store(MemFlags::trusted(), bv, ctx.state, ST_B);
    }
    b.ins().jump(ctx.bail, &[]);
}

/// Trap when `cond`, and carry on where it is false.
fn guard(
    b: &mut FunctionBuilder,
    ctx: Ctx,
    cond: ClValue,
    why: i64,
    pc: usize,
    ab: Option<(ClValue, ClValue)>,
) {
    let bad = b.create_block();
    let ok = b.create_block();
    b.ins().brif(cond, bad, &[], ok, &[]);
    b.switch_to_block(bad);
    b.seal_block(bad);
    trap(b, ctx, why, pc, ab);
    b.switch_to_block(ok);
    b.seal_block(ok);
}

/// The address of `a[i]`, bounds-checked. A negative length is a null array —
/// which is not an index error but a `TypeError`, exactly as the interpreter says.
fn elem_addr(
    b: &mut FunctionBuilder,
    ctx: Ctx,
    pc: usize,
    data: ClValue,
    len: ClValue,
    i: ClValue,
    it: Ty,
) -> ClValue {
    let idx = if it == Ty::I64 {
        i
    } else {
        b.ins().sextend(types::I64, i)
    };
    let null = b.ins().icmp_imm(IntCC::SignedLessThan, len, 0);
    guard(b, ctx, null, R_NULL, pc, None);
    let lo = b.ins().icmp_imm(IntCC::SignedLessThan, idx, 0);
    let hi = b.ins().icmp(IntCC::SignedGreaterThanOrEqual, idx, len);
    let bad = b.ins().bor(lo, hi);
    guard(b, ctx, bad, R_BOUNDS, pc, Some((idx, len)));
    let off = b.ins().imul_imm(idx, repr::SIZE as i64);
    b.ins().iadd(data, off)
}

/// Load one heap cell — a field, an element — as the type it is declared to hold.
///
/// The tag is checked. The type system says it cannot be wrong, and this checks it
/// anyway: the alternative to checking is reading an integer as a pointer, and a
/// language whose whole claim is memory safety does not get to do that on the word
/// of a checker. It is one compare against a byte already in cache.
#[allow(clippy::too_many_arguments)]
fn load_cell(
    b: &mut FunctionBuilder,
    ctx: Ctx,
    pc: usize,
    base: ClValue,
    at: i32,
    t: Ty,
    shim: &ShimRefs,
) -> SlotV {
    let tag = b.ins().load(types::I8, MemFlags::trusted(), base, at);
    let pay = at + t.payload();
    match t {
        Ty::Obj(_) | Ty::Arr(_) => {
            // An object field may hold `null`, and legitimately: a leaf's `left`.
            // So the tag may be the type's, or it may be null, and nothing else —
            // and *nothing else* is the part that matters, because the alternative
            // is to hand an integer to something that will treat it as an address.
            let is_t = b.ins().icmp_imm(IntCC::Equal, tag, t.tag() as i64);
            let is_null = b.ins().icmp_imm(IntCC::Equal, tag, repr::TAG_NULL as i64);
            let ok = b.ins().bor(is_t, is_null);
            let bad = b.ins().icmp_imm(IntCC::Equal, ok, 0);
            guard(b, ctx, bad, R_TAG, pc, None);
            // The *cell*, not the pointer in it: see `heap::cell_obj`.
            let cell = b.ins().iadd_imm(base, at as i64);
            let out = b.ins().stack_addr(types::I64, shim.scratch, 0);
            match t {
                Ty::Obj(_) => {
                    b.ins().call(shim.cell_obj, &[cell, out]);
                    let ptr = b.ins().load(types::I64, MemFlags::trusted(), out, 0);
                    let fields = b.ins().load(types::I64, MemFlags::trusted(), out, 8);
                    // Borrowed from the field that holds it, which compiled code
                    // cannot detach: no handle.
                    let zero = b.ins().iconst(types::I64, 0);
                    SlotV::Obj(ptr, fields, zero)
                }
                Ty::Arr(e) => {
                    b.ins().call(shim.cell_arr, &[cell, out]);
                    let ptr = b.ins().load(types::I64, MemFlags::trusted(), out, 0);
                    let data = b.ins().load(types::I64, MemFlags::trusted(), out, 8);
                    let len = b.ins().load(types::I64, MemFlags::trusted(), out, 16);
                    SlotV::Arr(ptr, data, len, e)
                }
                _ => unreachable!(),
            }
        }
        // A number, on the other hand, has to be *exactly* what it says: there is
        // no null in an `f64`, and a field that is still null is one this cannot
        // read. That is a bail, and the interpreter runs the call instead.
        t => {
            let bad = b.ins().icmp_imm(IntCC::NotEqual, tag, t.tag() as i64);
            guard(b, ctx, bad, R_TAG, pc, None);
            let v = match t {
                // A `bool`'s payload is one byte, not four. Loading four would read
                // three bytes of padding and call them part of the number.
                Ty::Bool => {
                    let raw = b.ins().load(types::I8, MemFlags::trusted(), base, pay);
                    b.ins().uextend(types::I32, raw)
                }
                t => b.ins().load(t.cl(), MemFlags::trusted(), base, pay),
            };
            SlotV::Val(v, t)
        }
    }
}

/// Store a scalar into a heap cell.
///
/// The old value is checked first, and this is not paranoia either: overwriting a
/// reference-counted value with a number would drop a reference without releasing
/// it. It cannot be one — the field is declared `float64` — so the check never
/// fires, and if the engine ever made it fire, it fails closed instead of leaking.
#[allow(clippy::too_many_arguments)]
fn store_cell(
    b: &mut FunctionBuilder,
    ctx: Ctx,
    pc: usize,
    base: ClValue,
    at: i32,
    t: Ty,
    v: ClValue,
) {
    let tag = b.ins().load(types::I8, MemFlags::trusted(), base, at);
    let is_t = b.ins().icmp_imm(IntCC::Equal, tag, t.tag() as i64);
    let is_null = b
        .ins()
        .icmp_imm(IntCC::Equal, tag, repr::TAG_NULL as i64);
    let ok = b.ins().bor(is_t, is_null);
    let bad = b.ins().icmp_imm(IntCC::Equal, ok, 0);
    guard(b, ctx, bad, R_TAG, pc, None);

    let want = b.ins().iconst(types::I8, t.tag() as i64);
    b.ins().store(MemFlags::trusted(), want, base, at);
    let pay = at + t.payload();
    match t {
        // A `bool`'s payload is one byte, not four.
        Ty::Bool => {
            let byte = b.ins().ireduce(types::I8, v);
            b.ins().store(MemFlags::trusted(), byte, base, pay);
        }
        _ => {
            b.ins().store(MemFlags::trusted(), v, base, pay);
        }
    }
}


/// A value as a condition: an `i32` that is 0 or not. Mersey's conditions accept
/// any number, testing `!= 0` — the C convention (§3.4).
fn truthy(b: &mut FunctionBuilder, v: ClValue, t: Ty) -> ClValue {
    match t {
        Ty::Bool => v,
        Ty::F64 => {
            let z = b.ins().f64const(0.0);
            let c = b.ins().fcmp(FloatCC::NotEqual, v, z);
            b.ins().uextend(types::I32, c)
        }
        _ => {
            let c = b.ins().icmp_imm(IntCC::NotEqual, v, 0);
            b.ins().uextend(types::I32, c)
        }
    }
}

/// The C conversions (§3.3), as instructions rather than as a reason to refuse
/// the function. An integer widens or truncates; a float rounds toward zero, and
/// saturates rather than trapping on a value no integer can hold.
fn convert(b: &mut FunctionBuilder, v: ClValue, from: Ty, to: Ty) -> ClValue {
    if from == to || (from.is_int() && to.is_int() && from.cl() == to.cl()) {
        return v;
    }
    match (from, to) {
        (Ty::I32 | Ty::Bool, Ty::I64) => b.ins().sextend(types::I64, v),
        (Ty::I64, Ty::I32) => b.ins().ireduce(types::I32, v),
        (Ty::I32 | Ty::Bool | Ty::I64, Ty::F64) => b.ins().fcvt_from_sint(types::F64, v),
        (Ty::F64, Ty::I32) => b.ins().fcvt_to_sint_sat(types::I32, v),
        (Ty::F64, Ty::I64) => b.ins().fcvt_to_sint_sat(types::I64, v),
        _ => v,
    }
}

/// The operator, and the type of what comes out: a comparison yields a `bool`,
/// everything else yields what went in.
fn lower_bin(b: &mut FunctionBuilder, op: BinOp, l: ClValue, r: ClValue, t: Ty) -> (ClValue, Ty) {
    if t == Ty::F64 {
        let fcmp = |b: &mut FunctionBuilder, cc: FloatCC, l, r| {
            let c = b.ins().fcmp(cc, l, r);
            b.ins().uextend(types::I32, c)
        };
        return match op {
            BinOp::Add => (b.ins().fadd(l, r), t),
            BinOp::Sub => (b.ins().fsub(l, r), t),
            BinOp::Mul => (b.ins().fmul(l, r), t),
            BinOp::Div => (b.ins().fdiv(l, r), t), // IEEE: inf/NaN, never traps
            BinOp::Lt => (fcmp(b, FloatCC::LessThan, l, r), Ty::Bool),
            BinOp::Gt => (fcmp(b, FloatCC::GreaterThan, l, r), Ty::Bool),
            BinOp::Le => (fcmp(b, FloatCC::LessThanOrEqual, l, r), Ty::Bool),
            BinOp::Ge => (fcmp(b, FloatCC::GreaterThanOrEqual, l, r), Ty::Bool),
            BinOp::Eq => (fcmp(b, FloatCC::Equal, l, r), Ty::Bool),
            BinOp::Ne => (fcmp(b, FloatCC::NotEqual, l, r), Ty::Bool),
            _ => unreachable!("plan filtered"),
        };
    }
    let cmp = |b: &mut FunctionBuilder, cc: IntCC, l, r| {
        let c = b.ins().icmp(cc, l, r);
        b.ins().uextend(types::I32, c)
    };
    match op {
        BinOp::Add => (b.ins().iadd(l, r), t),
        BinOp::Sub => (b.ins().isub(l, r), t),
        BinOp::Mul => (b.ins().imul(l, r), t),
        BinOp::BitAnd => (b.ins().band(l, r), t),
        BinOp::BitOr => (b.ins().bor(l, r), t),
        BinOp::BitXor => (b.ins().bxor(l, r), t),
        // Shift counts are masked to the width (§3.6) — ishl/sshr do that.
        BinOp::Shl => (b.ins().ishl(l, r), t),
        BinOp::Shr => (b.ins().sshr(l, r), t),
        BinOp::Lt => (cmp(b, IntCC::SignedLessThan, l, r), Ty::Bool),
        BinOp::Gt => (cmp(b, IntCC::SignedGreaterThan, l, r), Ty::Bool),
        BinOp::Le => (cmp(b, IntCC::SignedLessThanOrEqual, l, r), Ty::Bool),
        BinOp::Ge => (cmp(b, IntCC::SignedGreaterThanOrEqual, l, r), Ty::Bool),
        BinOp::Eq => (cmp(b, IntCC::Equal, l, r), Ty::Bool),
        BinOp::Ne => (cmp(b, IntCC::NotEqual, l, r), Ty::Bool),
        _ => unreachable!("plan filtered"),
    }
}

/// The ISA the JIT compiles for, with the hardening spec §5.2 asks for.
///
/// A JIT is the softest target an engine has: it turns attacker-influenced
/// input into executable memory. W^X (cranelift-jit maps pages writable, then
/// flips them to read-execute at finalize) stops the pages from being rewritten
/// after the fact; these settings harden the code that lands in them.
///
/// * **Stack probes.** A function with a large frame otherwise moves the stack
///   pointer past the guard page in one step and writes *beyond* it, turning a
///   clean fault into memory corruption. Probing touches each page in turn, so
///   the guard page is always the first thing hit. This is what makes a guard
///   page a guarantee rather than a hope.
/// * **Pointer authentication (aarch64).** Return addresses are signed on entry
///   and authenticated on return, so an overwritten return address faults
///   instead of transferring control — backward-edge CFI, the ROP defence.
/// * **Branch Target Identification (aarch64).** An indirect branch may only
///   land on a `bti` instruction, so a corrupted pointer cannot jump into the
///   middle of a function and use its tail as a gadget — forward-edge CFI.
///
/// Both PAC and BTI live in ARM's hint space: on a CPU without them the
/// instructions are NOPs, so this is safe to enable unconditionally and costs
/// nothing where it is not supported.
///
/// On x86-64 the equivalent (CET/`endbr64`) is not exposed as a Cranelift
/// setting in the version we build against, so forward-edge CFI there is
/// honestly *not* in place yet — see SECURITY-REVIEW.md rather than assume it.
fn hardened_isa() -> Option<Arc<dyn TargetIsa>> {
    // Non-PIC, no colocated libcalls: required for JIT on aarch64 (no PLT).
    let mut flags = settings::builder();
    flags.set("use_colocated_libcalls", "false").ok()?;
    flags.set("is_pic", "false").ok()?;
    flags.set("opt_level", "speed").ok()?;

    // Guard pages: never step over one.
    flags.set("enable_probestack", "true").ok()?;
    flags.set("probestack_strategy", "inline").ok()?;

    let mut isa = cranelift_native::builder().ok()?;
    if cfg!(target_arch = "aarch64") {
        // Backward-edge CFI (PAC) and forward-edge CFI (BTI).
        let _ = isa.set("sign_return_address", "true");
        let _ = isa.set("sign_return_address_all", "true");
        let _ = isa.set("use_bti", "true");
    }
    isa.finish(settings::Flags::new(flags)).ok()
}

/// Which hardening is actually on, for the security review and its test.
pub fn hardening() -> Vec<(&'static str, bool)> {
    let Some(isa) = hardened_isa() else {
        return Vec::new();
    };
    let flags = isa.flags();
    let mut out = vec![
        ("W^X code pages", true), // cranelift-jit flips at finalize
        ("stack probes (guard pages)", flags.enable_probestack()),
        // The heap is only reached through a layout this compiler has *proved*
        // against a real value, not one it assumed from a comment.
        ("value layout verified before heap access", heap::layout_holds()),
    ];
    // The ISA-specific ones are reported by name in the ISA's flag list.
    let isa_flags: Vec<String> = isa.isa_flags().iter().map(|f| f.to_string()).collect();
    let on = |name: &str| isa_flags.iter().any(|f| f == &format!("{name}=1"));
    if cfg!(target_arch = "aarch64") {
        out.push((
            "pointer authentication (backward-edge CFI)",
            on("sign_return_address"),
        ));
        out.push((
            "branch target identification (forward-edge CFI)",
            on("use_bti"),
        ));
    }
    if cfg!(target_arch = "x86_64") {
        // Reported as a *row that is off*, not left out. A gap that nothing
        // mentions is indistinguishable from a gap nobody noticed, and this one
        // is real: Cranelift does not expose CET/`endbr64` as a setting (checked
        // against 0.116 and 0.123), so forward-edge CFI is genuinely not in
        // place on x86-64. See `KNOWN_GAPS` and SECURITY-REVIEW.md.
        out.push(("forward-edge CFI (CET/endbr64)", false));
    }
    out
}

/// Hardening that is knowingly absent, and why. A gap listed here is one we
/// have looked at; anything else being off is a regression.
pub const KNOWN_GAPS: &[(&str, &str)] = &[(
    "forward-edge CFI (CET/endbr64)",
    "Cranelift exposes no CET setting (checked 0.116, 0.123); x86-64 only",
)];
