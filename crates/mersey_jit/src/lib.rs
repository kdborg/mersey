// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

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

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::LazyLock;

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
    NameKind, Trap, TrapReason, Value, JIT_DEPTH_LIMIT,
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
    /// A UTF-16 string: a data pointer, a length, and an arena handle (nonzero
    /// only for a *built* string this value owns; zero for one borrowed from the
    /// const pool). Immutable, so it is never a field or an array element — only
    /// a temporary, a local, or a web-call argument.
    Str,
    /// A host-object handle (a `JsRef`): one machine word, the handle id. What
    /// `createElement`/`new URL` return and a local holds — and what a web call
    /// or property access on that local uses as its receiver. The host owns the
    /// object; the handle is just an id, so no arena ownership.
    Web,
    /// An engine value the tier does not model — a `Bytes`, a `Url`, anything a
    /// `std:` native returns. Two words: the arena handle that names it, and the
    /// handle this value *owns* (zero when borrowed). Same discipline as `Str`
    /// and `Obj`: a handle lives in exactly one place, so releasing it is never
    /// a double free.
    Val,
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
            Ty::Obj(_) | Ty::Arr(_) | Ty::Str | Ty::Web | Ty::Val => types::I64,
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
            Ty::Obj(_) | Ty::Arr(_) | Ty::Str => 3,
            Ty::Val => 2,
            _ => 1,
        }
    }

    /// The machine types one of these occupies, in order.
    fn parts(self) -> Vec<types::Type> {
        match self {
            Ty::Obj(_) | Ty::Arr(_) | Ty::Str => vec![types::I64, types::I64, types::I64],
            Ty::Val => vec![types::I64, types::I64],
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
            Ty::Str => repr::TAG_STRING,
            Ty::Web => repr::TAG_JSREF,
            // Unreachable: an opaque is only ever a temporary, a local or a
            // shim argument — never the contents of a heap cell, so nothing
            // ever tag-checks one. `TAG_NULL` is the fail-closed answer if that
            // ever stops being true: no cell carries it, so the guard bails to
            // the interpreter instead of reading the cell as something it isn't.
            Ty::Val => repr::TAG_NULL,
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
const R_HOST: i64 = 6;

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
/// `MERSEY_JIT_TRACE=1` — print every opcode the two passes accept.
///
/// Tier-1 refuses a *function* for one op it cannot type, and `None` carries no
/// word about which one, so a workload that mysteriously runs at interpreted
/// speed gives nothing to go on. Both passes log each op as they take it, which
/// makes the bail the instruction *after* the last line printed. `MERSEY_JIT=0`
/// says whether a function is compiled; this says where it stopped being
/// compilable. Note both passes must be watched: the analysis can pass
/// end-to-end and codegen still decline.
static TRACE: LazyLock<bool> =
    LazyLock::new(|| std::env::var("MERSEY_JIT_TRACE").is_ok_and(|v| v != "0"));

pub fn hook(env: &dyn JitEnv, root: &JitFn) -> Option<Rc<JitCode>> {
    let code = compile_group(env, root);
    if *TRACE {
        // Neither a chunk nor a `JitFn` carries a name, so say what can be
        // said: the global binding for a plain function, and for a method only
        // that it is one (a `ClassDef`'s name is private to the interpreter).
        let who = match (&root.bind, &root.this) {
            (Some((n, _)), _) => n.clone(),
            (None, Some(_)) => "<method>".to_string(),
            (None, None) => "<anonymous>".to_string(),
        };
        eprintln!(
            "jit: {} {who} ({} ops)",
            if code.is_some() {
                "COMPILED"
            } else {
                "refused"
            },
            root.chunk.code.len()
        );
    }
    code
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
        if let Some(i) = self
            .classes
            .iter()
            .position(|k| k.class_id() == c.class_id())
        {
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
            JitSlot::Str => Ty::Str,
            JitSlot::Web => Ty::Web,
            JitSlot::Val => Ty::Val,
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
            // A string or opaque *field* still interprets: reading one means
            // pulling it out of a `Value` cell, and `load_cell` has no case for
            // either yet. A *parameter* is a different matter — it arrives already
            // in registers, or as a handle (see `param_types`).
            FieldTy::Str | FieldTy::Val => return None,
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
        let void =
            f.ret.is_none() && !f.ret_bool && f.ret_obj.is_none() && !f.ret_str && !f.ret_val;
        let ret = if void {
            Ty::I32 // a placeholder: nothing reads it
        } else if let Some(c) = &f.ret_obj {
            Ty::Obj(self.class_idx(c))
        } else if f.ret_str {
            Ty::Str
        } else if f.ret_val {
            Ty::Val
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
    /// Program counters where a `GetMember` is really a getter call. Codegen
    /// rewrites the op to `CallMethod(name, 0)` and the ordinary call path
    /// emits it — a getter *is* a zero-argument method, so it needs no
    /// machinery of its own. The target function lives in `method_at`.
    getter_pc: HashSet<usize>,
    /// `LoadName` sites that name a `std:` namespace routed through the native
    /// shim, and the namespace's name.
    std_ns_names: HashMap<u16, &'static str>,
    /// `LoadName` sites naming a top-level binding held opaquely (`Ty::Val`).
    /// Read once at entry, like a web global: the binding cannot be reassigned
    /// from compiled code, and each read would otherwise park another handle in
    /// the arena.
    opaque_globals: HashMap<u16, &'static str>,
    /// Name ids bound to a top-level *string*, read (and parked) once at entry.
    str_globals: HashMap<u16, &'static str>,
    /// `CallMethod` sites that are a native call, and the full member name.
    native_at: HashMap<usize, (&'static str, u32)>,
    /// `random.fill(buf)` sites, lowered to a direct shim rather than the general
    /// native path — see `Interp::jit_random_fill`.
    rand_fill_at: HashSet<usize>,
    /// String method sites: the method's name, and the register shape of its
    /// result (`STR_METHODS`).
    str_method_at: HashMap<usize, (&'static str, Ty)>,
    /// Method calls on an *opaque* receiver — `a.push(v)` on an array built here.
    val_method_at: HashMap<usize, (&'static str, Ty)>,
    /// `b[i]` / `b[i] = v` sites where the receiver is an opaque (a `Bytes`).
    val_index_at: HashSet<usize>,
    /// `[]` and `a.push(v)` sites, which build an array as an opaque.
    array_at: HashSet<usize>,
    /// `GetMember` sites reading a numeric property off an opaque.
    val_prop_at: HashMap<usize, &'static str>,
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
    /// Bytecode position of a `time.now()`/`time.monotonic()` → `true` for the
    /// epoch clock (`now`), `false` for monotonic. A numeric host call.
    time_at: HashMap<usize, bool>,
    /// Name ids that load the `std:time` namespace (a host-call receiver).
    time_ns_names: std::collections::HashSet<u16>,
    /// Name id → the web global's name, for the ones a `LoadName` reads as a
    /// host object. The leaked name backs the string constant codegen embeds.
    web_globals: HashMap<u16, &'static str>,
    /// Bytecode position of a numeric web method call → (method name, argc). A
    /// discarded-result call on a host object (`ctx.fillRect(...)`).
    web_call_at: HashMap<usize, (&'static str, u8)>,
    /// The subset of `web_call_at` whose (method, argc) has a typed binding id
    /// (`webbind::numeric`): those emit the lean `web_bind` call instead of the
    /// interned `web_call_num` one.
    web_bind_at: HashMap<usize, u32>,
    /// A web method call with mixed argument kinds (a handle or string arg, not
    /// just numbers) whose result is discarded → the typed `web_call_v` path.
    /// Carries the method name and each argument's kind.
    /// (method name, pre-interned id or `u32::MAX`, argument kinds).
    web_call_v_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)>,
    /// A web call whose string result is captured (`getItem`) → `web_call_str_v`.
    web_call_str_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)>,
    /// A web call whose *handle* result is captured (`createElement`) — same
    /// `web_call_str_v` shim, the handle read from the first out word.
    web_call_ref_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)>,
    /// A numeric-valued web property read (`buf.length`) → `web_get_num`:
    /// (property name, pre-interned id or `u32::MAX`).
    web_get_at: HashMap<usize, (&'static str, u32)>,
    /// A string-valued web property read (`url.pathname`) → `web_get_str_v`,
    /// captured as a `Ty::Str`. (property name, pre-interned id or `u32::MAX`).
    web_get_str_at: HashMap<usize, (&'static str, u32)>,
    /// A string web property read whose result flows straight into `.length`
    /// (`url.pathname.length`) → `web_get_str_len_v`: the length crosses back
    /// without the string being kept. The following `GetMember(length)` is folded
    /// in and skipped (see `folded`). (property name, pre-interned id).
    web_get_str_len_at: HashMap<usize, (&'static str, u32)>,
    /// Op indices folded into a preceding op and skipped by both passes (the
    /// `.length` consumed by `web_get_str_len_at`).
    folded: std::collections::HashSet<usize>,
    /// A host-constructor `new` (`new URL(s)`) → `web_new_v`, result captured as
    /// a `Ty::Web` handle. (constructor name, pre-interned id or `u32::MAX`,
    /// argument kinds).
    web_new_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)>,
    /// A cast of a host handle to a reference type (`createElement(…) as
    /// HTMLElement`): a runtime no-op, the handle passes straight through.
    cast_web: std::collections::HashSet<usize>,
    /// A web property set (`el.textContent = str`): (property name, id, value
    /// type — `Str` or a numeric).
    web_set_at: HashMap<usize, (&'static str, u32, Ty)>,
    /// Name ids bound to the `std:math` namespace (a `LoadName` reads it as a
    /// bare receiver marker, not a value).
    math_ns_names: std::collections::HashSet<u16>,
    /// Bytecode position of a `std:math` call → the instruction it lowers to.
    math_at: HashMap<usize, MathOp>,
    /// Bytecode positions of an explicit `as float64` cast — a widening cast
    /// that cannot throw, so compiled code performs it directly.
    cast_f64: std::collections::HashSet<usize>,
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
    /// The `std:time` namespace: not a value, only a receiver for the numeric
    /// host calls `now()` / `monotonic()`.
    TimeNs,
    /// A host object (`JsRef`) read from a top-level web global — a receiver for
    /// a numeric web method call. Carries the name id so codegen can read the
    /// live handle.
    Web(u16),
    /// The `std:math` namespace: a receiver whose numeric methods lower to
    /// machine instructions, not calls.
    MathNs,
    /// A `std:` namespace whose members go through the native shim. Carries the
    /// namespace name so the call site can name the member in full.
    StdNs(&'static str),
}

fn tval(s: TSlot) -> Option<Ty> {
    match s {
        TSlot::Val(t, _) => Some(t),
        TSlot::Null
        | TSlot::Callee(_)
        | TSlot::TimeNs
        | TSlot::Web(_)
        | TSlot::MathNs
        | TSlot::StdNs(_) => None,
    }
}

/// The kind of an argument crossing to a typed web call (`web_call_v`), in the
/// order the descriptor packs them.
#[derive(Clone, Copy, PartialEq)]
enum ArgKind {
    Num,
    Ref,
    Str,
}

/// Web methods whose result is a (nullable) string, so a compiled call captures
/// it as a `Ty::Str`. Named methods only — the host still verifies the receiver,
/// and a same-named method returning something else would produce a null string,
/// so the set is kept to ones that unambiguously return a string.
fn web_returns_string(method: &str) -> bool {
    matches!(method, "getItem" | "getAttribute")
}

/// Web methods whose result is a host-object handle, captured as a `Ty::Web`
/// value. As `web_returns_string`, the host verifies the receiver, and a method
/// that returned something else would come back as a null handle.
fn web_returns_handle(method: &str) -> bool {
    matches!(
        method,
        "createElement" | "appendChild" | "getElementById" | "querySelector"
    )
}

/// A primitive type name a cast converts to or rejects (numbers, `string`,
/// `bool`, `char`, `bigint`, `bigdec`). Anything else is a reference name (a
/// class or a web interface), and casting a host handle to one is a runtime
/// no-op — the interpreter passes the handle straight through.
fn is_scalar_cast_target(name: &str) -> bool {
    matches!(
        name,
        "int8"
            | "int16"
            | "int32"
            | "int"
            | "int64"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint"
            | "uint64"
            | "float32"
            | "float64"
            | "string"
            | "bool"
            | "char"
            | "bigint"
            | "bigdec"
    )
}

/// Web properties whose value is a string, so a compiled read captures a
/// `Ty::Str`. As `web_returns_string`, the host verifies the receiver and a
/// same-named property returning something else would come back a null string —
/// so the set is the URL/Location components, which are unambiguously strings.
fn web_prop_is_string(name: &str) -> bool {
    matches!(
        name,
        "pathname"
            | "search"
            | "href"
            | "hash"
            | "host"
            | "hostname"
            | "protocol"
            | "port"
            | "origin"
            | "username"
            | "password"
    )
}

/// A web-call/get/set receiver: a hoisted global (`TSlot::Web`) or a handle
/// value (`Ty::Web`, e.g. an element from `createElement`).
fn is_web_recv(t: &TSlot) -> bool {
    matches!(t, TSlot::Web(_)) || tval(*t) == Some(Ty::Web)
}

/// A `std:math` call the JIT lowers to instructions instead of a host call.
/// The set is exactly the operations with an exact, IEEE-clean match to the
/// interpreter: the single-instruction rounders and `sqrt`, `abs` for `float64`
/// only (integer `abs` wraps, a different result), and `min`/`max` for two
/// `float64`s lowered as the interpreter's own `<`-fold so NaN and ±0 agree.
/// `round` (ties-away) and the libm calls (`exp`, `sin`, …) are deliberately
/// absent — they have no single instruction that matches the interpreter.
#[derive(Clone, Copy, PartialEq)]
enum MathOp {
    /// `float64` argument, `float64` result — a widening arg is converted first.
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    /// `float64` argument only (integer `abs` wraps).
    Abs,
    /// Two `float64` arguments; lowered as `arg1 < arg0 ? …` per the interpreter.
    Min,
    Max,
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
    let scope = g.fns[me].scope.clone();
    let sig = g.sigs[me].clone();
    let n_slots = sig.n_slots;
    let n_params = sig.params.len();

    // What the checker said each slot holds. A slot it said nothing about — a
    // compiler temp, an object — has to be given a type by the code that stores
    // into it, or by the declaration it came from.
    let mut slots: Vec<Option<Ty>> = chunk.slot_types.iter().map(|t| t.and_then(ty_of)).collect();
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
    let mut getter_pc: HashSet<usize> = HashSet::new();
    let mut std_ns_names: HashMap<u16, &'static str> = HashMap::new();
    let mut opaque_globals: HashMap<u16, &'static str> = HashMap::new();
    let mut str_globals: HashMap<u16, &'static str> = HashMap::new();
    let mut native_at: HashMap<usize, (&'static str, u32)> = HashMap::new();
    let mut rand_fill_at: HashSet<usize> = HashSet::new();
    let mut str_method_at: HashMap<usize, (&'static str, Ty)> = HashMap::new();
    let mut val_method_at: HashMap<usize, (&'static str, Ty)> = HashMap::new();
    let mut val_index_at: HashSet<usize> = HashSet::new();
    let mut array_at: HashSet<usize> = HashSet::new();
    let mut val_prop_at: HashMap<usize, &'static str> = HashMap::new();
    let mut field_at: HashMap<usize, (u32, Ty)> = HashMap::new();
    let mut new_at: HashMap<usize, (u32, Option<usize>)> = HashMap::new();
    let mut clone_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut length_at: Vec<usize> = Vec::new();
    let mut time_at: HashMap<usize, bool> = HashMap::new();
    let mut time_ns_names: std::collections::HashSet<u16> = std::collections::HashSet::new();
    let mut web_globals: HashMap<u16, &'static str> = HashMap::new();
    let mut web_call_at: HashMap<usize, (&'static str, u8)> = HashMap::new();
    let mut web_bind_at: HashMap<usize, u32> = HashMap::new();
    let mut web_call_v_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)> = HashMap::new();
    let mut web_call_str_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)> = HashMap::new();
    let mut web_call_ref_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)> = HashMap::new();
    let mut web_get_at: HashMap<usize, (&'static str, u32)> = HashMap::new();
    let mut web_get_str_at: HashMap<usize, (&'static str, u32)> = HashMap::new();
    let mut web_get_str_len_at: HashMap<usize, (&'static str, u32)> = HashMap::new();
    let mut folded: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut web_new_at: HashMap<usize, (&'static str, u32, Vec<ArgKind>)> = HashMap::new();
    let mut web_set_at: HashMap<usize, (&'static str, u32, Ty)> = HashMap::new();
    let mut math_ns_names: std::collections::HashSet<u16> = std::collections::HashSet::new();
    let mut math_at: HashMap<usize, MathOp> = HashMap::new();
    let mut cast_f64: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut cast_web: std::collections::HashSet<usize> = std::collections::HashSet::new();
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
        if *TRACE {
            // A name id says nothing on its own, and a refusal on `LoadName` is
            // one of the commonest — so resolve it here.
            match *op {
                Op::LoadName(ni) | Op::CallMethod(ni, _) | Op::GetMember(ni, _) => {
                    eprintln!("jit: analyze {pc} {op:?} `{}`", chunk.names[ni as usize])
                }
                _ => eprintln!("jit: analyze {pc} {op:?}"),
            }
        }
        // Folded into the previous op (a `.length` a web-string read absorbed).
        if folded.contains(&pc) {
            continue;
        }
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
                // A string literal: borrowed from the const pool, which the
                // compiled code keeps alive. Stable — the buffer never moves.
                Value::Str(_) => TSlot::Val(Ty::Str, Prov::Stable),
                _ => return None,
            }),
            Op::LoadSlot(s) => {
                let t = slots[s as usize]?;
                // A reference loaded from a slot the body stores into is the one
                // borrow that can dangle — see `Prov`.
                let pv = match t {
                    Ty::Obj(_) | Ty::Arr(_) | Ty::Val if stored[s as usize] => Prov::FromSlot(s),
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
                        // Nor an opaque: there is no `clone_val` shim, and a
                        // borrow whose owning slot is overwritten would be left
                        // pointing at a released arena entry. Slot-to-slot copies
                        // of a native's result are rare; correctness is not.
                        Ty::Val => return None,
                        _ => {
                            let _ = src;
                        }
                    }
                }
                // Overwriting `s` releases its old object. Any borrow *of that
                // slot* still in flight would be a use of whatever is left.
                if stack.iter().any(|v| prov(*v) == Prov::FromSlot(s)) {
                    return None;
                }
            }
            Op::LoadName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                // Resolved in *this function's* scope, not the globals — see
                // `DefScope`. A module's `import { bytes } from "std:bytes"` is
                // invisible from the entry module's scope, which is what made
                // every std-library function refuse on its first `LoadName`.
                match g.env.name_kind(scope.as_ref(), name) {
                    NameKind::TimeNs => {
                        time_ns_names.insert(ni);
                        stack.push(TSlot::TimeNs); // a host-call receiver, not a value
                    }
                    NameKind::MathNs => {
                        math_ns_names.insert(ni);
                        stack.push(TSlot::MathNs); // an intrinsic receiver, not a value
                    }
                    NameKind::StdNs(ns) => {
                        std_ns_names.insert(ni, ns);
                        stack.push(TSlot::StdNs(ns)); // a native-call receiver
                    }
                    NameKind::Opaque => {
                        opaque_globals
                            .entry(ni)
                            .or_insert_with(|| Box::leak(name.to_string().into_boxed_str()));
                        stack.push(TSlot::Val(Ty::Val, Prov::Stable));
                    }
                    NameKind::StrGlobal => {
                        str_globals
                            .entry(ni)
                            .or_insert_with(|| Box::leak(name.to_string().into_boxed_str()));
                        stack.push(TSlot::Val(Ty::Str, Prov::Stable));
                    }
                    NameKind::Web => {
                        web_globals
                            .entry(ni)
                            .or_insert_with(|| Box::leak(name.to_string().into_boxed_str()));
                        stack.push(TSlot::Web(ni)); // a host-object receiver
                    }
                    NameKind::Other => {
                        let f = g.env.function(scope.as_ref(), name)?;
                        let idx = g.add(f)?;
                        callee.insert(ni, idx);
                        stack.push(TSlot::Callee(idx)); // a function, not a value
                    }
                }
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
                // A host constructor (`new URL(s)`): not a Mersey class, so it
                // takes the `web_new` path and hands back a `Ty::Web` handle. The
                // arguments cross as `WebArg`s, exactly like a `web_call_v` call.
                if g.env.class_for_new(name).is_none() && g.env.new_is_web(name) {
                    let mut arg_slots: Vec<TSlot> = Vec::with_capacity(argc as usize);
                    for _ in 0..argc {
                        arg_slots.push(stack.pop()?);
                    }
                    arg_slots.reverse();
                    let mut kinds: Vec<ArgKind> = Vec::with_capacity(argc as usize);
                    for a in &arg_slots {
                        let k = if matches!(a, TSlot::Web(_)) || tval(*a) == Some(Ty::Web) {
                            ArgKind::Ref
                        } else if tval(*a) == Some(Ty::Str) {
                            ArgKind::Str
                        } else if tval(*a).map(|t| t.is_num()).unwrap_or(false) {
                            ArgKind::Num
                        } else {
                            return None;
                        };
                        kinds.push(k);
                    }
                    let nm: &'static str = Box::leak(name.to_string().into_boxed_str());
                    let id = g.env.interned_web(nm).unwrap_or(u32::MAX);
                    web_new_at.insert(pc, (nm, id, kinds));
                    stack.push(TSlot::Val(Ty::Web, Prov::Stable));
                    continue;
                }
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
            // `` `…${i}…` `` — build one string from its parts. Only string and
            // integer parts are lowered (they append with no `display` call and
            // cannot throw); a float, bool, or Display class part bails.
            Op::TemplateJoin(n) => {
                for _ in 0..n {
                    let t = tval(stack.pop()?)?;
                    if !matches!(t, Ty::Str | Ty::I32 | Ty::I64) {
                        return None;
                    }
                }
                stack.push(TSlot::Val(Ty::Str, Prov::Stable));
            }
            Op::Bin(op) => {
                let r = stack.pop()?;
                let l = stack.pop()?;
                match (l, r) {
                    (TSlot::Null, TSlot::Val(t, _)) | (TSlot::Val(t, _), TSlot::Null) => {
                        // `Ty::Val` joins the reference types here: a native
                        // that returned null parked handle 0, so the comparison
                        // is the same handle-is-zero test.
                        if !matches!(t, Ty::Obj(_) | Ty::Arr(_) | Ty::Str | Ty::Val)
                            || !matches!(op, BinOp::Eq | BinOp::Ne)
                        {
                            return None;
                        }
                        stack.push(TSlot::Val(Ty::Bool, Prov::Stable));
                    }
                    // Two strings: a comparison of code units, which is all the
                    // language means by `==` on them.
                    (TSlot::Val(Ty::Str, _), TSlot::Val(Ty::Str, _)) => {
                        if !matches!(op, BinOp::Eq | BinOp::Ne) {
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
                // A property of a host object (`buf.length`). Only integer-valued
                // properties are lowered — a string or handle property would need
                // the checker's type here; `length` is always an integer.
                if is_web_recv(&base) {
                    if name == "length" {
                        let nm: &'static str = Box::leak(name.to_string().into_boxed_str());
                        let id = g.env.interned_web(nm).unwrap_or(u32::MAX);
                        web_get_at.insert(pc, (nm, id));
                        stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                        continue;
                    }
                    // A string component of a host object (`url.pathname`).
                    if web_prop_is_string(name) {
                        let nm: &'static str = Box::leak(name.to_string().into_boxed_str());
                        let id = g.env.interned_web(nm).unwrap_or(u32::MAX);
                        // If the very next op reads `.length` off this string and
                        // nothing else, fold the two into a length-only read — the
                        // host's string is never kept in the arena.
                        if let Some(Op::GetMember(ni2, _)) = chunk.code.get(pc + 1) {
                            // Not if a jump lands on the `.length` — folding it away
                            // would leave that block without its operand.
                            if chunk.names[*ni2 as usize] == "length"
                                && !block_types.contains_key(&(pc + 1))
                            {
                                web_get_str_len_at.insert(pc, (nm, id));
                                folded.insert(pc + 1);
                                stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                                continue;
                            }
                        }
                        web_get_str_at.insert(pc, (nm, id));
                        stack.push(TSlot::Val(Ty::Str, Prov::Stable));
                        continue;
                    }
                    return None;
                }
                if tval(base) == Some(Ty::Val) {
                    // `length`, and only `length`: its type is `int32`, and the
                    // type has to be right or the arithmetic around it refuses
                    // the function. Anything else about an opaque stays
                    // interpreted — this tier knows nothing about what it holds.
                    if name != "length" {
                        return None;
                    }
                    val_prop_at.insert(pc, "length");
                    stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                    continue;
                }
                match tval(base)? {
                    Ty::Obj(ci) => {
                        let cls = g.classes[ci as usize].clone();
                        // A getter is a *call*, not a load — so compile the call.
                        // Bailing here is what made one accessor read drop the
                        // whole function back to the interpreter.
                        if cls.is_accessor(name) && !cls.is_host_backed() {
                            let f = g.env.getter(&cls, name)?;
                            let idx = g.add(f)?;
                            let sig = g.sigs[idx].clone();
                            if !sig.params.is_empty() {
                                return None;
                            }
                            method_at.insert(pc, idx);
                            getter_pc.insert(pc);
                            stack.push(TSlot::Val(sig.ret, Prov::Stable));
                            continue;
                        }
                        // A member of a host-backed object is not this
                        // instruction either.
                        if cls.is_host_backed() {
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
                    // A non-null string's code-unit count: the middle machine
                    // value, no null check (a nullable string is not this tier's).
                    Ty::Str if name == "length" => {
                        length_at.push(pc);
                        stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                    }
                    _ => return None,
                }
            }
            Op::SetMember(ni, _) => {
                let name = chunk.names[ni as usize].as_str();
                let vslot = stack.pop()?;
                let recv = stack.pop()?;
                // A web property set (`el.textContent = str`). String or scalar
                // value; the receiver is a global or a `Ty::Web` handle.
                if is_web_recv(&recv) {
                    let vt = tval(vslot)?;
                    if vt != Ty::Str && !vt.is_num() {
                        return None;
                    }
                    let nm: &'static str = Box::leak(name.to_string().into_boxed_str());
                    let id = g.env.interned_web(nm).unwrap_or(u32::MAX);
                    web_set_at.insert(pc, (nm, id, vt));
                    g.writes = true;
                    stack.push(vslot);
                    continue;
                }
                let v = tval(vslot)?;
                let Ty::Obj(ci) = tval(recv)? else {
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
            // `[]` — an array built here. It is carried as an *opaque*, not as
            // `Ty::Arr`: that shape caches the element buffer's address and its
            // length, and a `push` moves both, so it is the wrong shape for an
            // array that grows. Reading, writing and `length` already go through
            // the same shims a `Bytes` uses.
            Op::MakeArray => {
                array_at.insert(pc);
                stack.push(TSlot::Val(Ty::Val, Prov::Stable));
            }
            // `a.push(v)` — and the array stays on the stack, as the interpreter
            // leaves it.
            Op::ArrayPush1 => {
                let v = tval(stack.pop()?)?;
                if !v.is_num() {
                    return None;
                }
                if tval(*stack.last()?) != Some(Ty::Val) {
                    return None;
                }
                array_at.insert(pc);
                g.writes = true;
            }
            Op::IndexGet => {
                let i = tval(stack.pop()?)?;
                let base = stack.pop()?;
                // `b[i]` on an opaque — a `Bytes`, the only thing the language
                // lets you index that is not an array or a string. It gives an
                // `int32`, and the shim reuses the interpreter's own bounds check
                // so the `RangeError` reads the same, length included.
                if tval(base) == Some(Ty::Val) && i.is_int() {
                    val_index_at.insert(pc);
                    stack.push(TSlot::Val(Ty::I32, Prov::Stable));
                    continue;
                }
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
                let base = stack.pop()?;
                if tval(base) == Some(Ty::Val) && i.is_int() && v.is_int() {
                    val_index_at.insert(pc);
                    g.writes = true;
                    stack.push(TSlot::Val(v, Prov::Stable));
                    continue;
                }
                let Ty::Arr(e) = tval(base)? else {
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
                let mut arg_slots: Vec<TSlot> = Vec::new();
                for _ in 0..n {
                    arg_slots.push(stack.pop()?);
                }
                arg_slots.reverse();
                let recv = stack.pop()?;
                // A method on a string. The receiver and every argument cross as
                // arena handles, and the result comes back in the shape
                // `STR_METHODS` promised — so the analysis can type the use site
                // without asking the checker, which is not here.
                if tval(recv) == Some(Ty::Str) {
                    let ret = str_method(&name, n)?;
                    // Only what can be boxed: a string, or a number that fits one
                    // of the `box_num` kinds.
                    if !arg_slots
                        .iter()
                        .all(|a| tval(*a) == Some(Ty::Str) || tval(*a).is_some_and(|t| t.is_num()))
                    {
                        return None;
                    }
                    let nm: &'static str = Box::leak(name.clone().into_boxed_str());
                    str_method_at.insert(pc, (nm, ret));
                    stack.push(TSlot::Val(ret, Prov::Stable));
                    continue;
                }
                // …and on an opaque. The receiver is already a handle, so unlike
                // a string it needs no parking.
                if tval(recv) == Some(Ty::Val) {
                    let ret = val_method(&name, n)?;
                    if !arg_slots
                        .iter()
                        .all(|a| tval(*a).is_some_and(|t| t.is_num() || t == Ty::Val))
                    {
                        return None;
                    }
                    let nm: &'static str = Box::leak(name.clone().into_boxed_str());
                    val_method_at.insert(pc, (nm, ret));
                    stack.push(TSlot::Val(ret, Prov::Stable));
                    continue;
                }
                if recv == TSlot::TimeNs {
                    // A numeric host call: `time.now()` / `time.monotonic()`,
                    // no arguments, an `f64` result.
                    if n != 0 || !(name == "now" || name == "monotonic") {
                        return None;
                    }
                    time_at.insert(pc, name == "now");
                    stack.push(TSlot::Val(Ty::F64, Prov::Stable));
                    continue;
                }
                if let TSlot::StdNs(ns) = recv {
                    // A `std:` native. Arguments cross as arena handles, so any
                    // value this tier can turn into one is allowed; the result
                    // comes back opaque, which is what makes the *rest* of the
                    // function compilable rather than the call itself fast.
                    // An argument has to become an arena handle. An opaque
                    // already is one; a string or a number is boxed into one at
                    // the call site. Anything else — an object, an array — stays
                    // interpreted.
                    if !arg_slots.iter().all(|a| {
                        matches!(tval(*a), Some(Ty::Val | Ty::Str))
                            || tval(*a).is_some_and(|t| t.is_num())
                    }) {
                        return None;
                    }
                    // `random.fill(buf)`: one opaque argument, no result, and the
                    // tightest loop a native appears in. It gets a direct shim.
                    if ns == "random" && name == "fill" && arg_slots.len() == 1 {
                        if tval(arg_slots[0]) != Some(Ty::Val) {
                            return None;
                        }
                        rand_fill_at.insert(pc);
                        stack.push(TSlot::Val(Ty::Val, Prov::Stable));
                        continue;
                    }
                    let full: &'static str = Box::leak(format!("{ns}.{name}").into_boxed_str());
                    native_at.insert(pc, (full, mersey_interp::Interp::native_fast_id(full)));
                    stack.push(TSlot::Val(Ty::Val, Prov::Stable));
                    continue;
                }
                if recv == TSlot::MathNs {
                    // A `std:math` intrinsic. The result is always `float64`; the
                    // argument-type rules keep the compiled result identical to
                    // the interpreter's (see `MathOp`).
                    let argt = |k: usize| tval(arg_slots[k]);
                    let op = match (name.as_str(), n) {
                        // Coerce any numeric arg to f64 — as the interpreter does.
                        ("sqrt", 1) if argt(0).is_some_and(|t| t.is_num()) => MathOp::Sqrt,
                        ("floor", 1) if argt(0).is_some_and(|t| t.is_num()) => MathOp::Floor,
                        ("ceil", 1) if argt(0).is_some_and(|t| t.is_num()) => MathOp::Ceil,
                        ("trunc", 1) if argt(0).is_some_and(|t| t.is_num()) => MathOp::Trunc,
                        // f64 only: integer `abs`/`min`/`max` keep integer type.
                        ("abs", 1) if argt(0) == Some(Ty::F64) => MathOp::Abs,
                        ("min", 2) if argt(0) == Some(Ty::F64) && argt(1) == Some(Ty::F64) => {
                            MathOp::Min
                        }
                        ("max", 2) if argt(0) == Some(Ty::F64) && argt(1) == Some(Ty::F64) => {
                            MathOp::Max
                        }
                        _ => return None,
                    };
                    math_at.insert(pc, op);
                    stack.push(TSlot::Val(Ty::F64, Prov::Stable));
                    continue;
                }
                // A web receiver is a hoisted global (`TSlot::Web`) or a handle
                // value (`Ty::Web` — the result of an earlier `createElement`).
                if is_web_recv(&recv) {
                    // A web method call on a host object. Each argument is a
                    // number, a host handle (global or `Ty::Web` value), or a
                    // string — the kinds that cross as a `WebArg`. All-numeric
                    // takes the lean typed-binding path; anything with a handle or
                    // string arg takes the general `web_call_v` path. The result
                    // is captured as a string / handle where the method returns
                    // one, else discarded.
                    let mut kinds: Vec<ArgKind> = Vec::with_capacity(n as usize);
                    for a in &arg_slots {
                        let k = if matches!(a, TSlot::Web(_)) || tval(*a) == Some(Ty::Web) {
                            ArgKind::Ref
                        } else if tval(*a) == Some(Ty::Str) {
                            ArgKind::Str
                        } else if tval(*a).map(|t| t.is_num()).unwrap_or(false) {
                            ArgKind::Num
                        } else {
                            return None;
                        };
                        kinds.push(k);
                    }
                    let nm: &'static str = Box::leak(name.into_boxed_str());
                    // The interned id is already known (warmup interned it); carry
                    // it so the compiled call skips the per-call intern.
                    let id = g.env.interned_web(nm).unwrap_or(u32::MAX);
                    if web_returns_string(nm) {
                        web_call_str_at.insert(pc, (nm, id, kinds));
                        stack.push(TSlot::Val(Ty::Str, Prov::Stable));
                    } else if web_returns_handle(nm) {
                        web_call_ref_at.insert(pc, (nm, id, kinds));
                        stack.push(TSlot::Val(Ty::Web, Prov::Stable));
                    } else if kinds.iter().all(|k| *k == ArgKind::Num) {
                        web_call_at.insert(pc, (nm, n));
                        if let Some(bid) = mersey_interp::webbind::numeric(nm, n) {
                            web_bind_at.insert(pc, bid);
                        }
                        stack.push(TSlot::Null);
                    } else {
                        web_call_v_at.insert(pc, (nm, id, kinds));
                        stack.push(TSlot::Null);
                    }
                    continue;
                }
                let mut args: Vec<Ty> = Vec::new();
                for a in arg_slots {
                    args.push(tval(a)?);
                }
                let Ty::Obj(ci) = tval(recv)? else {
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
            // An explicit `x as float64`: a widening/rounding cast that cannot
            // throw. Int targets (which `as` may throw or wrap on) and float32
            // (no register type) are left to the interpreter.
            Op::CastOp(ti, _) => {
                let base = stack.pop()?;
                // A host handle cast to a reference type (`el as HTMLElement`) is
                // a no-op: the interpreter passes the handle through unchanged.
                // Only a scalar target would convert or throw — leave those to it.
                if matches!(base, TSlot::Web(_)) || tval(base) == Some(Ty::Web) {
                    match chunk.types[ti as usize] {
                        mersey_front::ast::TypeExpr::Named { name, .. }
                            if !is_scalar_cast_target(name) =>
                        {
                            cast_web.insert(pc);
                            stack.push(TSlot::Val(Ty::Web, Prov::Stable));
                            continue;
                        }
                        _ => return None,
                    }
                }
                let src = tval(base)?;
                if !src.is_num() {
                    return None;
                }
                match chunk.types[ti as usize] {
                    mersey_front::ast::TypeExpr::Named { name, .. } if name == "float64" => {}
                    _ => return None,
                }
                cast_f64.insert(pc);
                stack.push(TSlot::Val(Ty::F64, Prov::Stable));
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
                    // `string?` is a null data pointer and `Bytes?` is handle 0,
                    // the same idea as a null object.
                    if !matches!(ret, Some(Ty::Obj(_) | Ty::Str | Ty::Val)) {
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
                    // A string leaves owned, exactly as an object does — and so
                    // does an opaque.
                    (Some(Ty::Str), Ty::Str) => {}
                    (Some(Ty::Val), Ty::Val) => {}
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
        // A string slot is owned too: on OSR its interpreter value is cloned into
        // the arena (the frame is abandoned), so the compiled release on
        // overwrite has a real reference to let go of.
        .map(|(t, st)| matches!(t, Ty::Obj(_) | Ty::Str) && *st)
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
        getter_pc,
        std_ns_names,
        opaque_globals,
        str_globals,
        native_at,
        rand_fill_at,
        str_method_at,
        val_method_at,
        val_index_at,
        array_at,
        val_prop_at,
        field_at,
        new_at,
        clone_at,
        owned_slots,
        length_at,
        time_at,
        time_ns_names,
        web_globals,
        web_call_at,
        web_bind_at,
        web_call_v_at,
        web_call_str_at,
        web_call_ref_at,
        web_get_at,
        web_get_str_at,
        web_get_str_len_at,
        folded,
        web_new_at,
        web_set_at,
        math_ns_names,
        math_at,
        cast_f64,
        cast_web,
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
    host_time: FuncId,
    global_web: FuncId,
    web_call_num: FuncId,
    global_val: FuncId,
    global_str: FuncId,
    clone_val: FuncId,
    array_new: FuncId,
    array_push: FuncId,
    val_index_get: FuncId,
    val_index_set: FuncId,
    native_call: FuncId,
    random_fill: FuncId,
    str_eq: FuncId,
    member_val: FuncId,
    str_num: FuncId,
    str_str: FuncId,
    val_len: FuncId,
    box_str: FuncId,
    box_num: FuncId,
    str_join: FuncId,
    str_vec_parts: FuncId,
    web_get_num: FuncId,
    web_get_str_v: FuncId,
    web_get_str_len_v: FuncId,
    web_new_v: FuncId,
    web_call_v: FuncId,
    web_call_str_v: FuncId,
    web_set_v: FuncId,
    web_bind_call: FuncId,
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
    builder.symbol("msy_host_time", heap::host_time as *const u8);
    builder.symbol("msy_global_web", heap::global_web as *const u8);
    builder.symbol("msy_web_call_num", heap::web_call_num as *const u8);
    builder.symbol("msy_global_val", heap::global_val as *const u8);
    builder.symbol("msy_global_str", heap::global_str as *const u8);
    builder.symbol("msy_clone_val", heap::clone_val as *const u8);
    builder.symbol("msy_array_new", heap::array_new as *const u8);
    builder.symbol("msy_array_push", heap::array_push as *const u8);
    builder.symbol("msy_val_index_get", heap::val_index_get as *const u8);
    builder.symbol("msy_val_index_set", heap::val_index_set as *const u8);
    builder.symbol("msy_native_call", heap::native_call as *const u8);
    builder.symbol("msy_random_fill", heap::random_fill as *const u8);
    builder.symbol("msy_str_eq", heap::str_eq as *const u8);
    builder.symbol("msy_member_val", heap::member_val as *const u8);
    builder.symbol("msy_str_num", heap::str_num as *const u8);
    builder.symbol("msy_str_str", heap::str_str as *const u8);
    builder.symbol("msy_val_len", heap::val_len as *const u8);
    builder.symbol("msy_box_str", heap::box_str as *const u8);
    builder.symbol("msy_box_num", heap::box_num as *const u8);
    builder.symbol("msy_web_bind_call", heap::web_bind_call as *const u8);
    builder.symbol("msy_str_join", heap::str_join as *const u8);
    builder.symbol("msy_str_vec_parts", heap::str_vec_parts as *const u8);
    builder.symbol("msy_web_get_num", heap::web_get_num as *const u8);
    builder.symbol("msy_web_get_str_v", heap::web_get_str_v as *const u8);
    builder.symbol(
        "msy_web_get_str_len_v",
        heap::web_get_str_len_v as *const u8,
    );
    builder.symbol("msy_web_new_v", heap::web_new_v as *const u8);
    builder.symbol("msy_web_call_v", heap::web_call_v as *const u8);
    builder.symbol("msy_web_call_str_v", heap::web_call_str_v as *const u8);
    builder.symbol("msy_web_set_v", heap::web_set_v as *const u8);
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
        // Cranelift's verifier runs here, and a malformed function is a bug in
        // this compiler rather than a shape it declined — so under the tracer say
        // so, instead of letting `.ok()?` make it look like an ordinary refusal.
        if let Err(e) = module.define_function(ids[n], &mut ctx) {
            if *TRACE {
                eprintln!("jit: define_function failed for fn {n}: {e:?}");
                eprintln!("{}", ctx.func.display());
            }
            return None;
        }
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
    let osr_id = wrapper(
        &mut module,
        &mut ctx,
        ids[0],
        &plans[0],
        &shims,
        ptr_ty,
        true,
    )?;

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
                // The same handle in both cells: the entry the body reads, and
                // the reference it owns.
                JitArg::Val(h) => (h.to_ne_bytes(), *h),
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
                // Both come back by the handle of the arena slot that owns them;
                // a null one has handle 0, which `take` reports as absent.
                Ty::Obj(_) | Ty::Str | Ty::Val => {
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
                    R_HOST => TrapReason::HostError,
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
        scope: root.scope.clone(),
        kind: match root_ret {
            Ty::I64 => JitKind::I64,
            Ty::F64 => JitKind::F64,
            _ => JitKind::I32,
        },
        slot_kinds: root_slots
            .iter()
            .map(|t| boundary(*t, &g.classes))
            .collect(),
        this_slot,
        call: Box::new(move |args: &[JitArg], arena: &mut Arena| {
            // The arguments, then the receiver — which goes to the slot the
            // compiler gave it, *after* the parameters, not before.
            let expect = n_params + usize::from(this_slot.is_some());
            if args.len() != expect {
                return JitResult::Bail;
            }
            let buf = marshal(args, &|i| {
                if i < n_params {
                    i
                } else {
                    this_slot.unwrap_or(i)
                }
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
        Ty::Str => JitSlot::Str,
        Ty::Web => JitSlot::Web,
        Ty::Val => JitSlot::Val,
        _ => JitSlot::I32,
    }
}

/// The string methods Tier 1 emits, with what each gives back and how many
/// arguments it takes.
///
/// The types are the checker's (`Type::Str`'s member table in `check.rs`) — this
/// is the same information in the terms this tier works in, because the analysis
/// has to know the result's *register shape* before it looks at the use site.
/// Absent on purpose: `codePointAt` and `at` return nullable scalars and `split`
/// returns an array, none of which has a shape here yet; they keep interpreting.
///
/// (name, result, min args, max args)
const STR_METHODS: &[(&str, Ty, u8, u8)] = &[
    ("indexOf", Ty::I32, 1, 1),
    ("lastIndexOf", Ty::I32, 1, 1),
    ("contains", Ty::Bool, 1, 1),
    ("startsWith", Ty::Bool, 1, 1),
    ("endsWith", Ty::Bool, 1, 1),
    ("slice", Ty::Str, 1, 2),
    ("substring", Ty::Str, 1, 2),
    ("charAt", Ty::Str, 1, 1),
    ("repeat", Ty::Str, 1, 1),
    ("padStart", Ty::Str, 1, 2),
    ("padEnd", Ty::Str, 1, 2),
    ("replace", Ty::Str, 2, 2),
    ("replaceAll", Ty::Str, 2, 2),
    ("concat", Ty::Str, 0, 4),
    ("toUpperCase", Ty::Str, 0, 0),
    ("toLowerCase", Ty::Str, 0, 0),
    ("trim", Ty::Str, 0, 0),
    ("trimStart", Ty::Str, 0, 0),
    ("trimEnd", Ty::Str, 0, 0),
    ("toString", Ty::Str, 0, 0),
];

/// Methods on an *opaque* receiver. Small on purpose: an array built in compiled
/// code is carried as one, and this is what such an array is asked to do. `push`
/// is void, and the interpreter's nothing is `null` — handle 0 — which is why its
/// result is typed as an opaque and discarded by the `Pop` that follows.
///
/// (name, result, min args, max args)
const VAL_METHODS: &[(&str, Ty, u8, u8)] = &[("push", Ty::Val, 1, 1), ("clear", Ty::Val, 0, 0)];

fn val_method(name: &str, argc: u8) -> Option<Ty> {
    VAL_METHODS
        .iter()
        .find(|(n, _, lo, hi)| *n == name && argc >= *lo && argc <= *hi)
        .map(|(_, t, _, _)| *t)
}

fn str_method(name: &str, argc: u8) -> Option<Ty> {
    STR_METHODS
        .iter()
        .find(|(n, _, lo, hi)| *n == name && argc >= *lo && argc <= *hi)
        .map(|(_, t, _, _)| *t)
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
        // (arena, epoch) -> milliseconds; the JIT's first host call.
        host_time: one("msy_host_time", Some(types::F64), 2)?,
        // (arena, name_ptr, name_len) -> handle
        global_web: one("msy_global_web", Some(types::I64), 3)?,
        // (arena, target, name_ptr, name_len, args_ptr, argc) -> 0 ok / 1 threw
        web_call_num: one("msy_web_call_num", Some(types::I64), 6)?,
        // (arena, name_ptr, name_len) -> arena handle, 0 if not an opaque
        global_val: one("msy_global_val", Some(types::I64), 3)?,
        // (arena, name_ptr, name_len, out) -> writes (data, len)
        global_str: one("msy_global_str", None, 4)?,
        // (arena, handle, index) -> the byte / i64::MIN
        // (arena, handle) -> a fresh handle to the same value
        clone_val: one("msy_clone_val", Some(types::I64), 2)?,
        // (arena) -> handle of a fresh array
        array_new: one("msy_array_new", Some(types::I64), 1)?,
        // (arena, handle, kind, bits) -> 0 ok / 1 not an array
        array_push: one("msy_array_push", Some(types::I64), 4)?,
        val_index_get: one("msy_val_index_get", Some(types::I64), 3)?,
        // (arena, handle, index, value) -> 0 ok / 1 threw
        val_index_set: one("msy_val_index_set", Some(types::I64), 4)?,
        // (arena, name_ptr, name_len, args_ptr, argc, fast_id) -> handle / 0 / u64::MAX
        native_call: one("msy_native_call", Some(types::I64), 6)?,
        // (arena, handle) -> 0 ok / 1 threw
        random_fill: one("msy_random_fill", Some(types::I64), 2)?,
        // (arena, recv, name_ptr, name_len, args_ptr, argc) -> the number / i64::MIN
        // (a_ptr, a_len, b_ptr, b_len) -> 1 / 0
        str_eq: one("msy_str_eq", Some(types::I64), 4)?,
        // (arena, recv, name_ptr, name_len, args_ptr, argc) -> handle / 0 / MAX
        member_val: one("msy_member_val", Some(types::I64), 6)?,
        str_num: one("msy_str_num", Some(types::I64), 6)?,
        // (…, out) -> 0 ok / 1 threw; `out` gets (data, len, handle)
        str_str: one("msy_str_str", Some(types::I64), 7)?,
        // (arena, handle) -> the length, or -1
        val_len: one("msy_val_len", Some(types::I64), 2)?,
        // (arena, ptr, len) -> handle of a Value::Str
        box_str: one("msy_box_str", Some(types::I64), 4)?,
        // (arena, kind, bits) -> handle of a Value::I32 / Value::F64
        box_num: one("msy_box_num", Some(types::I64), 3)?,
        // (arena, target, bind_id, name_ptr, name_len, args_ptr, argc) -> 0/1
        web_bind_call: one("msy_web_bind_call", Some(types::I64), 7)?,
        // (arena, parts_ptr, n, out) -> writes (ptr, len, handle)
        str_join: one("msy_str_join", None, 4)?,
        str_vec_parts: one("msy_str_vec_parts", None, 2)?,
        web_get_num: one("msy_web_get_num", Some(types::I64), 5)?,
        // (arena, target, id, name_ptr, name_len, out) -> writes (ptr, len, handle)
        web_get_str_v: one("msy_web_get_str_v", Some(types::I64), 6)?,
        // (arena, target, id, name_ptr, name_len) -> length, or i64::MIN if threw
        web_get_str_len_v: one("msy_web_get_str_len_v", Some(types::I64), 5)?,
        // (arena, id, name_ptr, name_len, desc, argc, out) -> writes the handle
        web_new_v: one("msy_web_new_v", Some(types::I64), 7)?,
        web_call_v: one("msy_web_call_v", Some(types::I64), 7)?,
        web_call_str_v: one("msy_web_call_str_v", Some(types::I64), 8)?,
        web_set_v: one("msy_web_set_v", Some(types::I64), 8)?,
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
        let str_vec_parts = module.declare_func_in_func(shims.str_vec_parts, b.func);
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
                    let h = b
                        .ins()
                        .load(types::I64, MemFlags::trusted(), slots_ptr, at + 8);
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
                // A string: the frame holds the `Rc<Vec<u16>>` address; derive its
                // data pointer and length (once, here) into a scratch slot, and
                // carry the arena handle from the second cell.
                Ty::Str => {
                    let p = b.ins().load(ptr_ty, MemFlags::trusted(), slots_ptr, at);
                    let scratch =
                        b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                            16,
                            3,
                        ));
                    let sp = b.ins().stack_addr(ptr_ty, scratch, 0);
                    b.ins().call(str_vec_parts, &[p, sp]);
                    let data = b.ins().load(ptr_ty, MemFlags::trusted(), sp, 0);
                    let len = b.ins().load(types::I64, MemFlags::trusted(), sp, 8);
                    let h = b
                        .ins()
                        .load(types::I64, MemFlags::trusted(), slots_ptr, at + 8);
                    args.push(data);
                    args.push(len);
                    args.push(h);
                }
                // An opaque: no address to derive anything from, just the arena
                // entry naming it. Two registers, straight from the two cells —
                // and it must be spelled out, because the catch-all below hands
                // over one register and a `Ty::Val` is two, which the entry
                // wrapper's signature notices and Cranelift rejects.
                Ty::Val => {
                    let v = b.ins().load(types::I64, MemFlags::trusted(), slots_ptr, at);
                    let h = b
                        .ins()
                        .load(types::I64, MemFlags::trusted(), slots_ptr, at + 8);
                    args.push(v);
                    args.push(h);
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
            // An object or a string result: its address, and the handle that owns
            // it — the third register in both cases (ptr, fields|len, handle).
            // An opaque is two registers, so its owning handle is the second.
            Ty::Val => {
                let rp = b.inst_results(call)[0];
                let rh = b.inst_results(call)[1];
                b.ins().store(MemFlags::trusted(), rp, out_ptr, 0);
                b.ins().store(MemFlags::trusted(), rh, out_ptr, 8);
            }
            Ty::Obj(_) | Ty::Str => {
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
    /// The `std:time` namespace receiver (see `TSlot::TimeNs`): no registers.
    TimeNs,
    /// A host-object receiver: its handle, live in a register.
    Web(ClValue),
    /// The `std:math` namespace receiver (see `TSlot::MathNs`): no registers.
    MathNs,
    /// A `std:` namespace receiver (see `TSlot::StdNs`): no registers, and no
    /// name either — codegen looks the member up by program counter, so by the
    /// time it sees this the marker only has to *be* one.
    StdNs,
    /// A UTF-16 string: data pointer, length, and arena handle (nonzero only for
    /// a built string this value owns). See `Ty::Str`.
    Str(ClValue, ClValue, ClValue),
    /// An opaque engine value: the arena handle naming it, and the handle this
    /// value owns (zero when borrowed). See `Ty::Val`.
    ValRef(ClValue, ClValue),
}

impl SlotV {
    /// The machine values, in the order the signature expects them.
    fn parts(self) -> Vec<ClValue> {
        match self {
            SlotV::Val(v, _) => vec![v],
            SlotV::Obj(p, b, h) => vec![p, b, h],
            SlotV::Arr(p, d, l, _) => vec![p, d, l],
            SlotV::Str(p, l, h) => vec![p, l, h],
            SlotV::ValRef(v, h) => vec![v, h],
            SlotV::Null
            | SlotV::Callee(_)
            | SlotV::TimeNs
            | SlotV::Web(_)
            | SlotV::MathNs
            | SlotV::StdNs => Vec::new(),
        }
    }

    /// Its address, if it is something that has one.
    fn addr(self) -> Option<ClValue> {
        match self {
            SlotV::Obj(p, _, _) => Some(p),
            SlotV::Arr(p, _, _, _) => Some(p),
            // A string's data pointer is 0 exactly when the string is null, so it
            // is what a `str != null` compares.
            SlotV::Str(p, _, _) => Some(p),
            // An opaque's arena handle, which `jit_native_call` leaves 0 for a
            // native that returned null — the same "0 is null" convention.
            SlotV::ValRef(v, _) => Some(v),
            _ => None,
        }
    }

    /// Its arena handle, if it could own one.
    fn handle(self) -> Option<ClValue> {
        match self {
            SlotV::Obj(_, _, h) => Some(h),
            SlotV::Str(_, _, h) => Some(h),
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
            // A string and an opaque are three and two registers wide. The
            // catch-all below takes one, and `i` still advanced by the full width —
            // so a string live across a branch came back as a scalar and the
            // function's `return` handed over one value where its signature
            // promised three. Cranelift's verifier caught it; nothing else would
            // have.
            Ty::Str => SlotV::Str(vals[i], vals[i + 1], vals[i + 2]),
            Ty::Val => SlotV::ValRef(vals[i], vals[i + 1]),
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
        host_time: module.declare_func_in_func(shims.host_time, b.func),
        global_web: module.declare_func_in_func(shims.global_web, b.func),
        web_call_num: module.declare_func_in_func(shims.web_call_num, b.func),
        global_val: module.declare_func_in_func(shims.global_val, b.func),
        global_str: module.declare_func_in_func(shims.global_str, b.func),
        clone_val: module.declare_func_in_func(shims.clone_val, b.func),
        array_new: module.declare_func_in_func(shims.array_new, b.func),
        array_push: module.declare_func_in_func(shims.array_push, b.func),
        val_index_get: module.declare_func_in_func(shims.val_index_get, b.func),
        val_index_set: module.declare_func_in_func(shims.val_index_set, b.func),
        native_call: module.declare_func_in_func(shims.native_call, b.func),
        random_fill: module.declare_func_in_func(shims.random_fill, b.func),
        str_eq: module.declare_func_in_func(shims.str_eq, b.func),
        member_val: module.declare_func_in_func(shims.member_val, b.func),
        str_num: module.declare_func_in_func(shims.str_num, b.func),
        str_str: module.declare_func_in_func(shims.str_str, b.func),
        val_len: module.declare_func_in_func(shims.val_len, b.func),
        box_str: module.declare_func_in_func(shims.box_str, b.func),
        box_num: module.declare_func_in_func(shims.box_num, b.func),
        web_bind_call: module.declare_func_in_func(shims.web_bind_call, b.func),
        str_join: module.declare_func_in_func(shims.str_join, b.func),
        web_get_num: module.declare_func_in_func(shims.web_get_num, b.func),
        web_get_str_v: module.declare_func_in_func(shims.web_get_str_v, b.func),
        web_get_str_len_v: module.declare_func_in_func(shims.web_get_str_len_v, b.func),
        web_new_v: module.declare_func_in_func(shims.web_new_v, b.func),
        web_call_v: module.declare_func_in_func(shims.web_call_v, b.func),
        web_call_str_v: module.declare_func_in_func(shims.web_call_str_v, b.func),
        web_set_v: module.declare_func_in_func(shims.web_set_v, b.func),
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

    // Hoist the web-global receivers. A global cannot be reassigned inside
    // compiled code — the JIT bails on every `StoreName` — so `ctx`'s handle is
    // invariant for the whole call. Read each one once, here in the entry block
    // (which dominates every body and OSR-target block), instead of paying the
    // `global_web` shim on every loop iteration. `SlotV::Web` has no machine
    // parts, so it never crossed a block edge anyway; the cached value, defined
    // in `entry`, is in scope wherever `LoadName` later pushes it.
    let web_handles: HashMap<u16, ClValue> = p
        .web_globals
        .iter()
        .map(|(&ni, name)| {
            let (nptr, nlen) = str_const(b, name);
            let call = b.ins().call(shim.global_web, &[arena_ptr, nptr, nlen]);
            (ni, b.inst_results(call)[0])
        })
        .collect();

    // The same reasoning as the web-global hoist above, and a stronger reason:
    // `global_val` parks the value in the arena, so reading it per iteration
    // would leak a handle per iteration.
    // Read once at entry, as the opaque globals are: the interpreter parks the
    // value so the buffer outlives any reassignment during the call, and this is
    // a borrow of it (handle 0 — nothing here releases it).
    let str_globals: HashMap<u16, (ClValue, ClValue)> = p
        .str_globals
        .iter()
        .map(|(&ni, name)| {
            let (nptr, nlen) = str_const(b, name);
            let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                16,
                3,
            ));
            let out_ptr = b.ins().stack_addr(types::I64, out, 0);
            b.ins()
                .call(shim.global_str, &[arena_ptr, nptr, nlen, out_ptr]);
            let d = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
            let l = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 8);
            (ni, (d, l))
        })
        .collect();

    let opaque_handles: HashMap<u16, ClValue> = p
        .opaque_globals
        .iter()
        .map(|(&ni, name)| {
            let (nptr, nlen) = str_const(b, name);
            let call = b.ins().call(shim.global_val, &[arena_ptr, nptr, nlen]);
            (ni, b.inst_results(call)[0])
        })
        .collect();

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
        // Folded into the previous op during analysis (a `.length` absorbed by a
        // web-string read); it emits nothing of its own.
        if p.folded.contains(&pc) {
            continue;
        }
        if *TRACE {
            eprintln!("jit: emit {pc} {:?}", chunk.code[pc]);
        }
        // A getter read, restated as the zero-argument method call it is, so the
        // ordinary call path below emits it. Analysis put the target in
        // `method_at`, which is where that path looks.
        let op = &match (*op, p.getter_pc.contains(&pc)) {
            (Op::GetMember(ni, _), true) => Op::CallMethod(ni, 0),
            (other, _) => other,
        };
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
                    // A string literal: its buffer lives in the const pool, which
                    // `JitCode.chunks` keeps alive for the code's whole lifetime,
                    // so its address is a constant. Handle 0 — a borrow, nothing
                    // to release.
                    Value::Str(rc) => {
                        let units: &[u16] = rc;
                        let ptr = b.ins().iconst(types::I64, units.as_ptr() as i64);
                        let len = b.ins().iconst(types::I64, units.len() as i64);
                        let h = b.ins().iconst(types::I64, 0);
                        SlotV::Str(ptr, len, h)
                    }
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
                    // A load is a borrow, as for an object: ptr and len, but the
                    // copy carries handle 0 — the slot keeps its own.
                    Ty::Str => {
                        let ptr = v(0, b);
                        let len = v(1, b);
                        let zero = b.ins().iconst(types::I64, 0);
                        SlotV::Str(ptr, len, zero)
                    }
                    // A borrow, as for a string: the arena handle, and 0 for
                    // the copy's own — the slot keeps the one reference.
                    Ty::Val => {
                        let val = v(0, b);
                        let zero = b.ins().iconst(types::I64, 0);
                        SlotV::ValRef(val, zero)
                    }
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
                if let SlotV::Str(ptr, len, h) = v {
                    // A built string owns an arena handle; overwriting the slot
                    // releases the one it held. A borrowed (const) string carries
                    // handle 0, so this is a no-op for it.
                    let old = b.use_var(Variable::from_u32(p.var_at[s as usize] + 2));
                    release_if_owned(b, shim.release, arena_ptr, old);
                    v = SlotV::Str(ptr, len, h);
                }
                if matches!(v, SlotV::ValRef(..)) {
                    // As for a string, but an opaque is two registers wide, so
                    // its owned handle is the *second* one.
                    let old = b.use_var(Variable::from_u32(p.var_at[s as usize] + 1));
                    release_if_owned(b, shim.release, arena_ptr, old);
                }
                for (j, part) in v.parts().into_iter().enumerate() {
                    b.def_var(Variable::from_u32(at + j as u32), part);
                }
            }
            // The only name left in a compiled function: the one it calls.
            Op::LoadName(ni) => {
                if p.time_ns_names.contains(&ni) {
                    stack.push(SlotV::TimeNs);
                } else if p.math_ns_names.contains(&ni) {
                    stack.push(SlotV::MathNs);
                } else if p.std_ns_names.contains_key(&ni) {
                    stack.push(SlotV::StdNs);
                } else if let Some(&(d, l)) = str_globals.get(&ni) {
                    let zero = b.ins().iconst(types::I64, 0);
                    stack.push(SlotV::Str(d, l, zero));
                } else if let Some(&h) = opaque_handles.get(&ni) {
                    // Borrowed: the global owns it, so this value's own handle
                    // is 0 and nothing here releases it.
                    let zero = b.ins().iconst(types::I64, 0);
                    stack.push(SlotV::ValRef(h, zero));
                } else if let Some(&h) = web_handles.get(&ni) {
                    // The handle was read once at entry (see the hoist above);
                    // reuse it rather than calling the shim each iteration.
                    stack.push(SlotV::Web(h));
                } else {
                    stack.push(SlotV::Callee(*p.callee.get(&ni)?));
                }
            }
            // `` `…${i}…` ``: write each part as a `StrPart` (kind, a, b) into a
            // stack buffer and call `str_join`, which allocates the result into
            // the arena and returns (ptr, len, handle). The parts are consumed;
            // an owned one (a nested built string) is released after the join.
            Op::TemplateJoin(n) => {
                let n = n as usize;
                let mut parts: Vec<(i64, ClValue, ClValue)> = Vec::with_capacity(n);
                let mut consumed: Vec<ClValue> = Vec::new();
                for _ in 0..n {
                    match stack.pop()? {
                        SlotV::Str(ptr, len, h) => {
                            consumed.push(h);
                            parts.push((0, ptr, len));
                        }
                        SlotV::Val(v, t) if t == Ty::I32 || t == Ty::I64 => {
                            let v64 = if t == Ty::I64 {
                                v
                            } else {
                                b.ins().sextend(types::I64, v)
                            };
                            let zero = b.ins().iconst(types::I64, 0);
                            parts.push((1, v64, zero));
                        }
                        _ => return None,
                    }
                }
                parts.reverse();
                let desc = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (n.max(1) as u32) * 24,
                    3,
                ));
                let desc_ptr = b.ins().stack_addr(types::I64, desc, 0);
                for (k, (kind, a, bb)) in parts.iter().enumerate() {
                    let off = (k * 24) as i32;
                    let kv = b.ins().iconst(types::I64, *kind);
                    b.ins().store(MemFlags::trusted(), kv, desc_ptr, off);
                    b.ins().store(MemFlags::trusted(), *a, desc_ptr, off + 8);
                    b.ins().store(MemFlags::trusted(), *bb, desc_ptr, off + 16);
                }
                let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    24,
                    3,
                ));
                let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                let nv = b.ins().iconst(types::I64, n as i64);
                b.ins()
                    .call(shim.str_join, &[arena_ptr, desc_ptr, nv, out_ptr]);
                for h in consumed {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
                let ptr = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                let len = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 8);
                let h = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 16);
                stack.push(SlotV::Str(ptr, len, h));
            }
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
                    (SlotV::Str(ap, al, _), SlotV::Str(bp, bl, _)) => {
                        let call = b.ins().call(shim.str_eq, &[ap, al, bp, bl]);
                        let eq = b.inst_results(call)[0];
                        let v = match binop {
                            BinOp::Eq => b.ins().icmp_imm(IntCC::NotEqual, eq, 0),
                            BinOp::Ne => b.ins().icmp_imm(IntCC::Equal, eq, 0),
                            _ => return None,
                        };
                        let v = b.ins().uextend(types::I32, v);
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
            // A numeric web property read (`buf.length`): the receiver's handle,
            // the property name as a string constant, the integer back. A thrown
            // read comes back as i64::MIN and traps like an interpreted one.
            Op::GetMember(_, _) if p.web_get_at.contains_key(&pc) => {
                let (name, id) = *p.web_get_at.get(&pc)?;
                let SlotV::Web(handle) = stack.pop()? else {
                    return None;
                };
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let call = b
                    .ins()
                    .call(shim.web_get_num, &[arena_ptr, handle, id_v, nptr, nlen]);
                let v = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::Equal, v, i64::MIN);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(SlotV::Val(b.ins().ireduce(types::I32, v), Ty::I32));
            }
            // `url.pathname.length`: the string read and the `.length` folded into
            // one — the host returns just the code-unit count, nothing is kept.
            Op::GetMember(_, _) if p.web_get_str_len_at.contains_key(&pc) => {
                let (name, id) = *p.web_get_str_len_at.get(&pc)?;
                let handle = match stack.pop()? {
                    SlotV::Web(h) => h,
                    SlotV::Val(h, Ty::Web) => h,
                    _ => return None,
                };
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let call = b.ins().call(
                    shim.web_get_str_len_v,
                    &[arena_ptr, handle, id_v, nptr, nlen],
                );
                let v = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::Equal, v, i64::MIN);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(SlotV::Val(b.ins().ireduce(types::I32, v), Ty::I32));
            }
            // A string-valued web property (`url.pathname`): the shim writes the
            // captured string's (ptr, len, arena handle) into a 3-word out slot.
            Op::GetMember(_, _) if p.web_get_str_at.contains_key(&pc) => {
                let (name, id) = *p.web_get_str_at.get(&pc)?;
                let handle = match stack.pop()? {
                    SlotV::Web(h) => h,
                    SlotV::Val(h, Ty::Web) => h,
                    _ => return None,
                };
                let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    24,
                    3,
                ));
                let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let call = b.ins().call(
                    shim.web_get_str_v,
                    &[arena_ptr, handle, id_v, nptr, nlen, out_ptr],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                let sptr = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                let slen = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 8);
                let sh = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 16);
                stack.push(SlotV::Str(sptr, slen, sh));
            }
            // `length` on an opaque. This has to precede the generic
            // `GetMember` arm below, which matches every remaining member read
            // and would otherwise take this one and fail on it.
            Op::GetMember(_, _) if p.val_prop_at.contains_key(&pc) => {
                let SlotV::ValRef(v, _) = stack.pop()? else {
                    return None;
                };
                let call = b.ins().call(shim.val_len, &[arena_ptr, v]);
                let out = b.inst_results(call)[0];
                let bad = b.ins().icmp_imm(IntCC::SignedLessThan, out, 0);
                guard(b, ctx, bad, R_HOST, pc, None);
                let narrowed = b.ins().ireduce(types::I32, out);
                stack.push(SlotV::Val(narrowed, Ty::I32));
            }
            Op::GetMember(_, _) if p.length_at.contains(&pc) => {
                let len = match stack.pop()? {
                    // A null array has no length: reading one is the same
                    // `TypeError` the interpreter raises, not a number.
                    SlotV::Arr(_, _, len, _) => {
                        let null = b.ins().icmp_imm(IntCC::SignedLessThan, len, 0);
                        guard(b, ctx, null, R_NULL, pc, None);
                        len
                    }
                    // A (non-null) string's code-unit count: the middle value.
                    // A built temporary owns its handle — reading its length is
                    // the end of it, so let it go (a borrow's handle 0 no-ops).
                    SlotV::Str(_, len, h) => {
                        release_if_owned(b, shim.release, arena_ptr, h);
                        len
                    }
                    _ => return None,
                };
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
            // A web property set (`el.textContent = str`): the value crosses as a
            // string (kind 2) or a number (kind 0); the receiver is a global or a
            // `Ty::Web` handle. The assignment's value is pushed back as its
            // result (a later `Pop` releases a built string).
            Op::SetMember(_, _) if p.web_set_at.contains_key(&pc) => {
                let (name, id, _vt) = *p.web_set_at.get(&pc)?;
                let vslot = stack.pop()?;
                let recv = match stack.pop()? {
                    SlotV::Web(h) => h,
                    SlotV::Val(h, Ty::Web) => h,
                    _ => return None,
                };
                let zero = b.ins().iconst(types::I64, 0);
                let (kind, a, bb) = match vslot {
                    SlotV::Str(ptr, len, _) => (2i64, ptr, len),
                    SlotV::Val(v, t) if t.is_num() => {
                        let f = convert(b, v, t, Ty::F64);
                        (0i64, b.ins().bitcast(types::I64, MemFlags::new(), f), zero)
                    }
                    _ => return None,
                };
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let kind_v = b.ins().iconst(types::I64, kind);
                let call = b.ins().call(
                    shim.web_set_v,
                    &[arena_ptr, recv, id_v, nptr, nlen, kind_v, a, bb],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(vslot);
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
            Op::MakeArray if p.array_at.contains(&pc) => {
                let call = b.ins().call(shim.array_new, &[arena_ptr]);
                let h = b.inst_results(call)[0];
                // Owned: overwriting the slot it is stored into releases it, and
                // the sweep at the end of the call takes the rest.
                stack.push(SlotV::ValRef(h, h));
            }
            Op::ArrayPush1 if p.array_at.contains(&pc) => {
                let (v, t) = scalar(stack.pop()?)?;
                let (kind, bits) = if t == Ty::F64 {
                    (1i64, b.ins().bitcast(types::I64, MemFlags::new(), v))
                } else if t == Ty::I64 {
                    (0i64, v)
                } else {
                    (0i64, b.ins().sextend(types::I64, v))
                };
                let SlotV::ValRef(h, _) = *stack.last()? else {
                    return None;
                };
                let k = b.ins().iconst(types::I64, kind);
                let call = b.ins().call(shim.array_push, &[arena_ptr, h, k, bits]);
                let status = b.inst_results(call)[0];
                let bad = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                guard(b, ctx, bad, R_TAG, pc, None);
            }
            Op::IndexGet if p.val_index_at.contains(&pc) => {
                let (i, it) = scalar(stack.pop()?)?;
                let i = convert(b, i, it, Ty::I64);
                let SlotV::ValRef(h, _) = stack.pop()? else {
                    return None;
                };
                let call = b.ins().call(shim.val_index_get, &[arena_ptr, h, i]);
                let v = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::Equal, v, i64::MIN);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(SlotV::Val(b.ins().ireduce(types::I32, v), Ty::I32));
            }
            Op::IndexSet if p.val_index_at.contains(&pc) => {
                let (v, vt) = scalar(stack.pop()?)?;
                let v64 = convert(b, v, vt, Ty::I64);
                let (i, it) = scalar(stack.pop()?)?;
                let i = convert(b, i, it, Ty::I64);
                let SlotV::ValRef(h, _) = stack.pop()? else {
                    return None;
                };
                let call = b.ins().call(shim.val_index_set, &[arena_ptr, h, i, v64]);
                let status = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(SlotV::Val(v, vt));
            }
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

            // A numeric web method call whose result is discarded
            // (`ctx.fillRect(x, y, w, h)`): pop the numeric args as f64 into a
            // stack buffer, read the receiver's live handle, and call the shim,
            // which reuses the interpreter's own web-call path. A thrown error
            // comes back as 1 and traps here, so it surfaces exactly as an
            // interpreted call's would.
            // A web call with a handle or string argument, result discarded:
            // pack each argument into a `WebArgDesc` (kind, a, b) on a stack
            // buffer and call `web_call_v`. An owned string argument (a built
            // template) is released after the call.
            // A web call whose string result is captured (`getItem(k)`): the same
            // argument descriptor as `web_call_v`, but the reply is read back as a
            // (possibly null) string — data pointer, length, arena handle.
            Op::CallMethod(_, _) if p.web_call_str_at.contains_key(&pc) => {
                let (name, id, kinds) = p.web_call_str_at.get(&pc)?.clone();
                let (recv, desc_ptr, n, owned) = build_web_desc(b, &mut stack, &kinds)?;
                let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    24,
                    3,
                ));
                let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let nv = b.ins().iconst(types::I64, n as i64);
                let call = b.ins().call(
                    shim.web_call_str_v,
                    &[arena_ptr, recv, id_v, nptr, nlen, desc_ptr, nv, out_ptr],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in owned {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
                let sptr = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                let slen = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 8);
                let sh = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 16);
                stack.push(SlotV::Str(sptr, slen, sh));
            }
            // A web call whose handle result is captured (`createElement`): the
            // same shim writes the handle id to the first out word.
            Op::CallMethod(_, _) if p.web_call_ref_at.contains_key(&pc) => {
                let (name, id, kinds) = p.web_call_ref_at.get(&pc)?.clone();
                let (recv, desc_ptr, n, owned) = build_web_desc(b, &mut stack, &kinds)?;
                let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    24,
                    3,
                ));
                let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let nv = b.ins().iconst(types::I64, n as i64);
                let call = b.ins().call(
                    shim.web_call_str_v,
                    &[arena_ptr, recv, id_v, nptr, nlen, desc_ptr, nv, out_ptr],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in owned {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
                let handle = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                stack.push(SlotV::Val(handle, Ty::Web));
            }
            Op::CallMethod(_, _) if p.web_call_v_at.contains_key(&pc) => {
                let (name, id, kinds) = p.web_call_v_at.get(&pc)?.clone();
                let (recv, desc_ptr, n, owned) = build_web_desc(b, &mut stack, &kinds)?;
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let nv = b.ins().iconst(types::I64, n as i64);
                let call = b.ins().call(
                    shim.web_call_v,
                    &[arena_ptr, recv, id_v, nptr, nlen, desc_ptr, nv],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in owned {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
                stack.push(SlotV::Null);
            }
            Op::CallMethod(_, _) if p.web_call_at.contains_key(&pc) => {
                let (name, argc) = *p.web_call_at.get(&pc)?;
                let mut fargs: Vec<ClValue> = Vec::with_capacity(argc as usize);
                for _ in 0..argc {
                    let (v, t) = scalar(stack.pop()?)?;
                    fargs.push(convert(b, v, t, Ty::F64));
                }
                fargs.reverse();
                let SlotV::Web(handle) = stack.pop()? else {
                    return None;
                };
                let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (argc as u32).max(1) * 8,
                    3,
                ));
                let args_ptr = b.ins().stack_addr(types::I64, slot, 0);
                for (k, a) in fargs.iter().enumerate() {
                    b.ins()
                        .store(MemFlags::trusted(), *a, args_ptr, (k * 8) as i32);
                }
                let (nptr, nlen) = str_const(b, name);
                let argc_v = b.ins().iconst(types::I64, argc as i64);
                // A method with a typed binding id takes the lean `web_bind`
                // route (id + raw args, no interned name, no marshalling); the
                // name is still passed so the host can fall back if it has no
                // typed binding. Everything else stays on the interned path.
                let call = match p.web_bind_at.get(&pc) {
                    Some(&bid) => {
                        let bid_v = b.ins().iconst(types::I64, bid as i64);
                        b.ins().call(
                            shim.web_bind_call,
                            &[arena_ptr, handle, bid_v, nptr, nlen, args_ptr, argc_v],
                        )
                    }
                    None => b.ins().call(
                        shim.web_call_num,
                        &[arena_ptr, handle, nptr, nlen, args_ptr, argc_v],
                    ),
                };
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                stack.push(SlotV::Null);
            }
            // A numeric host call: `time.now()` / `time.monotonic()`. The
            // receiver is the time-namespace marker (no registers); the shim
            // reads the host the interpreter set on the arena and returns f64.
            Op::CallMethod(_, _) if p.time_at.contains_key(&pc) => {
                let SlotV::TimeNs = stack.pop()? else {
                    return None;
                };
                let epoch = b
                    .ins()
                    .iconst(types::I64, if *p.time_at.get(&pc)? { 1 } else { 0 });
                let call = b.ins().call(shim.host_time, &[arena_ptr, epoch]);
                let ms = b.inst_results(call)[0];
                stack.push(SlotV::Val(ms, Ty::F64));
            }
            // A string method. Receiver and arguments go out as arena handles —
            // a *built* string already owns one and `box_str` hands that same
            // handle back rather than copying, so only constants and borrows are
            // parked. Everything parked here is released the moment the call
            // returns; the result, when it is a string, is this value's to hold.
            Op::CallMethod(_, n) if p.str_method_at.contains_key(&pc) => {
                let (name, ret) = *p.str_method_at.get(&pc)?;
                let mut handles: Vec<ClValue> = Vec::with_capacity(n as usize);
                let mut boxed: Vec<ClValue> = Vec::new();
                for _ in 0..n {
                    handles.push(box_arg(b, &shim, arena_ptr, stack.pop()?, &mut boxed)?);
                }
                handles.reverse();
                let SlotV::Str(rptr, rlen, rh) = stack.pop()? else {
                    return None;
                };
                let rc = b.ins().call(shim.box_str, &[arena_ptr, rptr, rlen, rh]);
                let recv = b.inst_results(rc)[0];

                let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (n as u32).max(1) * 8,
                    3,
                ));
                let args_ptr = b.ins().stack_addr(types::I64, slot, 0);
                for (k, h) in handles.iter().enumerate() {
                    b.ins()
                        .store(MemFlags::trusted(), *h, args_ptr, (k * 8) as i32);
                }
                let (nptr, nlen) = str_const(b, name);
                let argc = b.ins().iconst(types::I64, n as i64);

                let result = if ret == Ty::Str {
                    let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                        24,
                        3,
                    ));
                    let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                    let call = b.ins().call(
                        shim.str_str,
                        &[arena_ptr, recv, nptr, nlen, args_ptr, argc, out_ptr],
                    );
                    let status = b.inst_results(call)[0];
                    let threw = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                    guard(b, ctx, threw, R_HOST, pc, None);
                    let d = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                    let l = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 8);
                    let h = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 16);
                    SlotV::Str(d, l, h)
                } else {
                    let call = b
                        .ins()
                        .call(shim.str_num, &[arena_ptr, recv, nptr, nlen, args_ptr, argc]);
                    let v = b.inst_results(call)[0];
                    let threw = b.ins().icmp_imm(IntCC::Equal, v, i64::MIN);
                    guard(b, ctx, threw, R_HOST, pc, None);
                    SlotV::Val(b.ins().ireduce(types::I32, v), ret)
                };
                for h in boxed {
                    b.ins().call(shim.release, &[arena_ptr, h]);
                }
                stack.push(result);
            }
            // A method on an opaque: the receiver's handle straight through, the
            // arguments parked as for any shim that takes handles.
            Op::CallMethod(_, n) if p.val_method_at.contains_key(&pc) => {
                let (name, _) = *p.val_method_at.get(&pc)?;
                let mut handles: Vec<ClValue> = Vec::with_capacity(n as usize);
                let mut boxed: Vec<ClValue> = Vec::new();
                for _ in 0..n {
                    handles.push(box_arg(b, &shim, arena_ptr, stack.pop()?, &mut boxed)?);
                }
                handles.reverse();
                let SlotV::ValRef(recv, _) = stack.pop()? else {
                    return None;
                };
                let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (n as u32).max(1) * 8,
                    3,
                ));
                let args_ptr = b.ins().stack_addr(types::I64, slot, 0);
                for (k, h) in handles.iter().enumerate() {
                    b.ins()
                        .store(MemFlags::trusted(), *h, args_ptr, (k * 8) as i32);
                }
                let (nptr, nlen) = str_const(b, name);
                let argc = b.ins().iconst(types::I64, n as i64);
                let call = b.ins().call(
                    shim.member_val,
                    &[arena_ptr, recv, nptr, nlen, args_ptr, argc],
                );
                let out = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::Equal, out, -1); // u64::MAX
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in boxed {
                    b.ins().call(shim.release, &[arena_ptr, h]);
                }
                stack.push(SlotV::ValRef(out, out));
            }
            // `random.fill(buf)`: the buffer's handle straight to a shim. No name,
            // no argument array, no lend/give-back, no `Result` to unwrap.
            Op::CallMethod(_, _) if p.rand_fill_at.contains(&pc) => {
                let SlotV::ValRef(h, _) = stack.pop()? else {
                    return None;
                };
                let SlotV::StdNs = stack.pop()? else {
                    return None;
                };
                let call = b.ins().call(shim.random_fill, &[arena_ptr, h]);
                let status = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                // `fill` returns nothing; the call's value is the null a `Pop`
                // will discard, and it owns no handle.
                let zero = b.ins().iconst(types::I64, 0);
                stack.push(SlotV::ValRef(zero, zero));
            }
            // A `std:` native. Arguments go out as an array of arena handles;
            // the result comes back as one. `u64::MAX` means it threw — the
            // interpreter stashed the error and this traps so `after_jit` can
            // raise it where it happened.
            Op::CallMethod(_, n) if p.native_at.contains_key(&pc) => {
                let (name, fast_id) = *p.native_at.get(&pc)?;
                // Each argument becomes an arena handle. A handle we *made*
                // here is ours to release the moment the call returns — the
                // native has taken what it needs by then.
                let mut handles: Vec<ClValue> = Vec::with_capacity(n as usize);
                let mut boxed: Vec<ClValue> = Vec::new();
                for _ in 0..n {
                    let h = match stack.pop()? {
                        SlotV::ValRef(v, _) => v,
                        SlotV::Str(ptr, len, have) => {
                            // A built string already owns an arena entry holding
                            // exactly this `Value::Str`, and `box_str` hands that
                            // same handle back rather than copying; a constant is
                            // parked in the interpreter's memo, which owns it.
                            // Either way it is not ours to release.
                            let c = b.ins().call(shim.box_str, &[arena_ptr, ptr, len, have]);
                            b.inst_results(c)[0]
                        }
                        SlotV::Val(v, t) if t.is_num() => {
                            let (kind, bits) = if t == Ty::F64 {
                                (1i64, b.ins().bitcast(types::I64, MemFlags::new(), v))
                            } else if t == Ty::I64 {
                                (0i64, v)
                            } else {
                                (0i64, b.ins().sextend(types::I64, v))
                            };
                            let k = b.ins().iconst(types::I64, kind);
                            let c = b.ins().call(shim.box_num, &[arena_ptr, k, bits]);
                            let h = b.inst_results(c)[0];
                            boxed.push(h);
                            h
                        }
                        _ => return None,
                    };
                    handles.push(h);
                }
                handles.reverse();
                let SlotV::StdNs = stack.pop()? else {
                    return None;
                };
                let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    (handles.len().max(1) * 8) as u32,
                    3,
                ));
                let args_ptr = b.ins().stack_addr(types::I64, slot, 0);
                for (k, h) in handles.iter().enumerate() {
                    b.ins()
                        .store(MemFlags::trusted(), *h, args_ptr, (k * 8) as i32);
                }
                let (nptr, nlen) = str_const(b, name);
                let argc = b.ins().iconst(types::I64, handles.len() as i64);
                let id_v = b.ins().iconst(types::I64, fast_id as i64);
                let call = b.ins().call(
                    shim.native_call,
                    &[arena_ptr, nptr, nlen, args_ptr, argc, id_v],
                );
                let out = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::Equal, out, -1); // u64::MAX
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in boxed {
                    b.ins().call(shim.release, &[arena_ptr, h]);
                }
                // The result is this value's to release.
                stack.push(SlotV::ValRef(out, out));
            }
            // A `std:math` intrinsic: lowered to instructions, no call. The
            // receiver is the math-namespace marker (no registers).
            Op::CallMethod(_, _) if p.math_at.contains_key(&pc) => {
                let op = *p.math_at.get(&pc)?;
                let argc = if matches!(op, MathOp::Min | MathOp::Max) {
                    2
                } else {
                    1
                };
                let mut fargs: Vec<ClValue> = Vec::with_capacity(argc);
                for _ in 0..argc {
                    let (v, t) = scalar(stack.pop()?)?;
                    fargs.push(convert(b, v, t, Ty::F64));
                }
                fargs.reverse(); // [arg0, arg1]
                let SlotV::MathNs = stack.pop()? else {
                    return None;
                };
                let r = match op {
                    MathOp::Sqrt => b.ins().sqrt(fargs[0]),
                    MathOp::Floor => b.ins().floor(fargs[0]),
                    MathOp::Ceil => b.ins().ceil(fargs[0]),
                    MathOp::Trunc => b.ins().trunc(fargs[0]),
                    MathOp::Abs => b.ins().fabs(fargs[0]),
                    // The interpreter folds left: take arg1 iff `(arg1 < arg0)`
                    // for min, iff `!(arg1 < arg0)` for max. `fcmp LessThan` is
                    // false for a NaN operand, so NaN and ±0 come out identical
                    // to the interpreter (which compares with the same `<`).
                    //
                    // Both select on the *same* `a1 < a0` compare — max just
                    // swaps the arms rather than negating it. Negating (`!lt`)
                    // would force the FP-compare result out to a GPR and back
                    // (cset/uxtb/subs), landing three integer ops and two
                    // domain crossings on the loop-carried path; selecting on
                    // `lt` directly lowers to a single `fcmp`+`fcsel`.
                    // One instruction. `fmax`/`fmin` lower to arm64 `FMAX`/`FMIN`,
                    // which propagate NaN — the same answers this pair already
                    // gave through `fcmp`+`select`, and the same JS gives — but
                    // off the loop-carried dependency path by a whole
                    // instruction. `mathk`, whose kernel is
                    // `math.max(acc * 0.0, …)` and is latency-bound on exactly
                    // that chain, goes 15.5 -> 12.4 ms.
                    MathOp::Min | MathOp::Max => {
                        let (a0, a1) = (fargs[0], fargs[1]);
                        if op == MathOp::Min {
                            b.ins().fmin(a0, a1)
                        } else {
                            b.ins().fmax(a0, a1)
                        }
                    }
                };
                stack.push(SlotV::Val(r, Ty::F64));
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

                // Inline a small straight-line arithmetic leaf (`add3`, `step`):
                // its body is expanded here, erasing the call, the frame marshal,
                // and the depth guard. The wins V8's inliner gets for free.
                if !is_method && this.is_none() && inlinable(plans, f, 1) {
                    let r = inline_body(b, plans, f, &args, 1)?;
                    stack.push(r);
                    continue;
                }

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
                    // Three registers, as an object's: data, length, and the
                    // handle that owns it. Taking only the first left the length
                    // behind, so `f(x).length` could not be emitted.
                    Ty::Str => stack.push(SlotV::Str(results[0], results[1], results[2])),
                    Ty::Val => stack.push(SlotV::ValRef(results[0], results[1])),
                    t => stack.push(SlotV::Val(results[0], t)),
                }
            }
            // Allocation: the engine makes the instance (fields folded, containers
            // fresh, GC informed), the arena owns it, and the constructor runs as
            // an ordinary compiled call on the new object.
            // A host constructor (`new URL(s)`): build the args, call `web_new`,
            // capture the handle. No receiver and no depth guard — it is a host
            // call, not a Mersey frame.
            Op::NewNamed(_, _) if p.web_new_at.contains_key(&pc) => {
                let (name, id, kinds) = p.web_new_at.get(&pc)?.clone();
                let (desc_ptr, n, owned) = build_web_args(b, &mut stack, &kinds)?;
                let out = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
                    cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let out_ptr = b.ins().stack_addr(types::I64, out, 0);
                let (nptr, nlen) = str_const(b, name);
                let id_v = b.ins().iconst(types::I64, id as i64);
                let nv = b.ins().iconst(types::I64, n as i64);
                let call = b.ins().call(
                    shim.web_new_v,
                    &[arena_ptr, id_v, nptr, nlen, desc_ptr, nv, out_ptr],
                );
                let failed = b.inst_results(call)[0];
                let threw = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                guard(b, ctx, threw, R_HOST, pc, None);
                for h in owned {
                    release_if_owned(b, shim.release, arena_ptr, h);
                }
                let handle = b.ins().load(types::I64, MemFlags::trusted(), out_ptr, 0);
                stack.push(SlotV::Val(handle, Ty::Web));
            }
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
                let cls_ptr = b
                    .ins()
                    .iconst(types::I64, Rc::as_ptr(&class_rcs[ci as usize]) as i64);
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
                    let status =
                        b.ins()
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
            Op::CastOp(_, _) if p.cast_f64.contains(&pc) => {
                let (v, from) = scalar(stack.pop()?)?;
                let out = convert(b, v, from, Ty::F64);
                stack.push(SlotV::Val(out, Ty::F64));
            }
            // `el as HTMLElement`: a host handle cast to a reference type. The
            // handle is unchanged — re-type the slot to `Ty::Web` and move on.
            Op::CastOp(_, _) if p.cast_web.contains(&pc) => {
                let handle = match stack.pop()? {
                    SlotV::Web(h) => h,
                    SlotV::Val(h, Ty::Web) => h,
                    _ => return None,
                };
                stack.push(SlotV::Val(handle, Ty::Web));
            }
            Op::BinNum(binop, num) => {
                let t = ty_of(num)?;
                let (r, _) = scalar(stack.pop()?)?;
                let (l, _) = scalar(stack.pop()?)?;
                // Integer division can fault (spec §3.6).
                if t.is_int() && matches!(binop, BinOp::Div | BinOp::Rem) {
                    // Constant divisor with |d| ≥ 2: strength-reduce to a
                    // multiply, and skip the guards (a nonzero, non-(−1)
                    // constant can neither fault on zero nor overflow at INT_MIN).
                    //
                    // x86_64 only: its integer divide is 20–40 cycles, so the
                    // magic-multiply sequence is a large win. Apple Silicon (and
                    // ARM64 generally) has a ~2-cycle divider, where the longer
                    // sequence measures *slower* — so leave the hardware divide.
                    if cfg!(target_arch = "x86_64") {
                        if let Some(d) = const_int(b, r) {
                            // 2 ≤ |d| < 2^(w−1): excludes 0, ±1 (handled by the
                            // guarded path) and INT_MIN (outside the magic's domain).
                            let w = if t == Ty::I64 { 64 } else { 32 };
                            if d.abs() >= 2 && (d.unsigned_abs() as u128) < (1u128 << (w - 1)) {
                                let v = emit_const_divrem(b, l, d, t, binop == BinOp::Rem);
                                stack.push(SlotV::Val(v, t));
                                continue;
                            }
                        }
                    }
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
                // As for an object: a built string hands over its arena handle,
                // a borrowed one (a constant, or a field's) is parked first — the
                // caller cannot be given something it has no way to keep.
                SlotV::Str(ptr, len, h) if matches!(p.ret, Ty::Str) => {
                    let no_handle = b.ins().icmp_imm(IntCC::Equal, h, 0);
                    let is_real = b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
                    let promote = b.ins().band(no_handle, is_real);
                    let borrow = b.create_block();
                    let done = b.create_block();
                    b.append_block_param(done, types::I64);
                    b.ins().brif(promote, borrow, &[], done, &[h]);
                    b.switch_to_block(borrow);
                    b.seal_block(borrow);
                    let zero = b.ins().iconst(types::I64, 0);
                    let cl = b.ins().call(shim.box_str, &[arena_ptr, ptr, len, zero]);
                    let boxed = b.inst_results(cl)[0];
                    b.ins().jump(done, &[boxed]);
                    b.switch_to_block(done);
                    b.seal_block(done);
                    let h = b.block_params(done)[0];
                    b.ins().return_(&[ptr, len, h]);
                    reachable = false;
                }
                // An opaque leaves owned too. A borrow (handle 0) names an entry
                // some slot owns, so it takes a reference of its own first — the
                // caller will `take` it out of the arena, which must not steal it
                // from whoever still holds it.
                SlotV::ValRef(v, h) if matches!(p.ret, Ty::Val) => {
                    let no_handle = b.ins().icmp_imm(IntCC::Equal, h, 0);
                    let is_real = b.ins().icmp_imm(IntCC::NotEqual, v, 0);
                    let promote = b.ins().band(no_handle, is_real);
                    let borrow = b.create_block();
                    let done = b.create_block();
                    b.append_block_param(done, types::I64);
                    b.ins().brif(promote, borrow, &[], done, &[h]);
                    b.switch_to_block(borrow);
                    b.seal_block(borrow);
                    let cl = b.ins().call(shim.clone_val, &[arena_ptr, v]);
                    let cloned = b.inst_results(cl)[0];
                    b.ins().jump(done, &[cloned]);
                    b.switch_to_block(done);
                    b.seal_block(done);
                    let h = b.block_params(done)[0];
                    b.ins().return_(&[v, h]);
                    reachable = false;
                }
                SlotV::Null if matches!(p.ret, Ty::Val) => {
                    let z = b.ins().iconst(types::I64, 0);
                    b.ins().return_(&[z, z]);
                    reachable = false;
                }
                SlotV::Null if matches!(p.ret, Ty::Obj(_) | Ty::Str) => {
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

/// One argument on its way to a shim that takes arena handles: a string is parked
/// (or its existing entry reused, or found in the interpreter's memo), a number is
/// boxed. Anything already opaque is passed as it stands. `boxed` collects only
/// what has to be released after the call — which a string never does, its entry
/// being owned by the string itself or by the memo.
fn box_arg(
    b: &mut FunctionBuilder,
    shim: &ShimRefs,
    arena_ptr: ClValue,
    v: SlotV,
    boxed: &mut Vec<ClValue>,
) -> Option<ClValue> {
    Some(match v {
        SlotV::ValRef(h, _) => h,
        // Nothing parked by `box_str` is ours to release: it is either the
        // string's own arena entry handed straight back, or one the interpreter
        // holds in a small bounded memo and frees when it displaces it.
        SlotV::Str(ptr, len, have) => {
            let c = b.ins().call(shim.box_str, &[arena_ptr, ptr, len, have]);
            b.inst_results(c)[0]
        }
        SlotV::Val(v, t) if t.is_num() => {
            let (kind, bits) = if t == Ty::F64 {
                (1i64, b.ins().bitcast(types::I64, MemFlags::new(), v))
            } else if t == Ty::I64 {
                (0i64, v)
            } else {
                (0i64, b.ins().sextend(types::I64, v))
            };
            let k = b.ins().iconst(types::I64, kind);
            let c = b.ins().call(shim.box_num, &[arena_ptr, k, bits]);
            let h = b.inst_results(c)[0];
            boxed.push(h);
            h
        }
        _ => return None,
    })
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
    host_time: cranelift_codegen::ir::FuncRef,
    global_web: cranelift_codegen::ir::FuncRef,
    web_call_num: cranelift_codegen::ir::FuncRef,
    global_val: cranelift_codegen::ir::FuncRef,
    global_str: cranelift_codegen::ir::FuncRef,
    clone_val: cranelift_codegen::ir::FuncRef,
    array_new: cranelift_codegen::ir::FuncRef,
    array_push: cranelift_codegen::ir::FuncRef,
    val_index_get: cranelift_codegen::ir::FuncRef,
    val_index_set: cranelift_codegen::ir::FuncRef,
    native_call: cranelift_codegen::ir::FuncRef,
    random_fill: cranelift_codegen::ir::FuncRef,
    str_eq: cranelift_codegen::ir::FuncRef,
    member_val: cranelift_codegen::ir::FuncRef,
    str_num: cranelift_codegen::ir::FuncRef,
    str_str: cranelift_codegen::ir::FuncRef,
    val_len: cranelift_codegen::ir::FuncRef,
    box_str: cranelift_codegen::ir::FuncRef,
    box_num: cranelift_codegen::ir::FuncRef,
    web_bind_call: cranelift_codegen::ir::FuncRef,
    str_join: cranelift_codegen::ir::FuncRef,
    web_get_num: cranelift_codegen::ir::FuncRef,
    web_get_str_v: cranelift_codegen::ir::FuncRef,
    web_get_str_len_v: cranelift_codegen::ir::FuncRef,
    web_new_v: cranelift_codegen::ir::FuncRef,
    web_call_v: cranelift_codegen::ir::FuncRef,
    web_call_str_v: cranelift_codegen::ir::FuncRef,
    web_set_v: cranelift_codegen::ir::FuncRef,
    /// Where the engine writes an object's address and its fields back to.
    scratch: cranelift_codegen::ir::StackSlot,
}

/// Stop here, and say why. `a`/`b` carry the index and the length, for the one
/// message that needs them.
fn trap(b: &mut FunctionBuilder, ctx: Ctx, why: i64, pc: usize, ab: Option<(ClValue, ClValue)>) {
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
    let is_null = b.ins().icmp_imm(IntCC::Equal, tag, repr::TAG_NULL as i64);
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

/// A `&'static str` as (ptr, len) machine constants — the plan leaks the string,
/// so its address is stable for as long as the compiled code that names it.
fn str_const(b: &mut FunctionBuilder, s: &str) -> (ClValue, ClValue) {
    let ptr = b.ins().iconst(types::I64, s.as_ptr() as i64);
    let len = b.ins().iconst(types::I64, s.len() as i64);
    (ptr, len)
}

/// The C conversions (§3.3), as instructions rather than as a reason to refuse
/// the function. An integer widens or truncates; a float rounds toward zero, and
/// saturates rather than trapping on a value no integer can hold.
/// Pop a web call's arguments and receiver and lay the arguments out as a
/// `WebArgDesc` array on a stack slot — shared by the discarded-result
/// (`web_call_v`) and string-result (`web_call_str_v`) paths. Returns the
/// receiver handle, the descriptor pointer, the argument count, and the handles
/// of any owned string arguments (built templates) the caller must release after
/// the call.
/// Pop `kinds.len()` arguments off the operand stack and lay them out as a
/// `WebArgDesc` array on a fresh stack slot, returning (desc pointer, count,
/// owned string handles to release after the call). Shared by every web call
/// and by `web_new` — the receiver, if any, is popped separately by the caller.
fn build_web_args(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SlotV>,
    kinds: &[ArgKind],
) -> Option<(ClValue, usize, Vec<ClValue>)> {
    let n = kinds.len();
    let mut descs: Vec<(i64, ClValue, ClValue)> = Vec::with_capacity(n);
    let mut owned: Vec<ClValue> = Vec::new();
    for k in kinds.iter().rev() {
        match (*k, stack.pop()?) {
            (ArgKind::Num, v) => {
                let (val, t) = scalar(v)?;
                let f = convert(b, val, t, Ty::F64);
                let bits = b.ins().bitcast(types::I64, MemFlags::new(), f);
                let zero = b.ins().iconst(types::I64, 0);
                descs.push((0, bits, zero));
            }
            (ArgKind::Ref, SlotV::Web(h)) => {
                let zero = b.ins().iconst(types::I64, 0);
                descs.push((1, h, zero));
            }
            (ArgKind::Ref, SlotV::Val(h, Ty::Web)) => {
                let zero = b.ins().iconst(types::I64, 0);
                descs.push((1, h, zero));
            }
            (ArgKind::Str, SlotV::Str(ptr, len, h)) => {
                owned.push(h);
                descs.push((2, ptr, len));
            }
            _ => return None,
        }
    }
    descs.reverse();
    let slot = b.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        (n.max(1) as u32) * 24,
        3,
    ));
    let desc_ptr = b.ins().stack_addr(types::I64, slot, 0);
    for (j, (kind, a, bb)) in descs.iter().enumerate() {
        let off = (j * 24) as i32;
        let kv = b.ins().iconst(types::I64, *kind);
        b.ins().store(MemFlags::trusted(), kv, desc_ptr, off);
        b.ins().store(MemFlags::trusted(), *a, desc_ptr, off + 8);
        b.ins().store(MemFlags::trusted(), *bb, desc_ptr, off + 16);
    }
    Some((desc_ptr, n, owned))
}

fn build_web_desc(
    b: &mut FunctionBuilder,
    stack: &mut Vec<SlotV>,
    kinds: &[ArgKind],
) -> Option<(ClValue, ClValue, usize, Vec<ClValue>)> {
    let (desc_ptr, n, owned) = build_web_args(b, stack, kinds)?;
    let recv = match stack.pop()? {
        SlotV::Web(h) => h,
        SlotV::Val(h, Ty::Web) => h,
        _ => return None,
    };
    Some((recv, desc_ptr, n, owned))
}

/// The inliner's budget: how deep leaf-into-leaf expansion goes, and how many
/// bytecode ops a callee may have. Small — the point is to erase the call
/// overhead of tiny helpers (`add3`, `step`), not to duplicate large bodies.
const INLINE_DEPTH: usize = 3;
const INLINE_MAX_OPS: usize = 48;

/// A callee simple enough to inline: a **straight-line arithmetic leaf** — only
/// slot, const and integer/float `BinNum` ops (no `Div`/`Rem`, which fault and
/// would need a guard) and *recursively inlinable* calls, ending in a `Return`.
/// No branches, no heap, no host: anything else and it stays a real call.
fn inlinable(plans: &[Plan], f: usize, depth: usize) -> bool {
    if depth > INLINE_DEPTH {
        return false;
    }
    let ch = &plans[f].chunk;
    if ch.code.len() > INLINE_MAX_OPS {
        return false;
    }
    let ops_ok = ch.code.iter().all(|op| match op {
        Op::LoadSlot(_)
        | Op::StoreSlot(_)
        | Op::Const(_)
        | Op::LoadName(_)
        | Op::Call(_)
        | Op::Return
        | Op::ReturnNull => true,
        Op::BinNum(bop, _) => !matches!(bop, BinOp::Div | BinOp::Rem),
        _ => false,
    });
    ops_ok
        && plans[f]
            .callee
            .values()
            .all(|&c| inlinable(plans, c, depth + 1))
}

/// Expand an inlinable callee at a call site: mini-evaluate its straight-line
/// body with `args` bound to its parameters and produce the result value. The
/// arithmetic lowers through the very same `lower_bin` the normal path uses, so
/// an inlined call is bit-identical to a real one — just without the call, the
/// frame, or the depth guard. Returns `None` on anything the pre-check would not
/// have accepted (so a `true` from `inlinable` means this succeeds).
fn inline_body(
    b: &mut FunctionBuilder,
    plans: &[Plan],
    f: usize,
    args: &[SlotV],
    depth: usize,
) -> Option<SlotV> {
    if depth > INLINE_DEPTH {
        return None;
    }
    let ch = plans[f].chunk.clone();
    let mut locals: Vec<Option<SlotV>> = vec![None; ch.n_slots as usize];
    for (k, a) in args.iter().enumerate() {
        *locals.get_mut(k)? = Some(*a);
    }
    let mut stack: Vec<SlotV> = Vec::new();
    for op in ch.code.iter() {
        match *op {
            Op::LoadSlot(s) => stack.push((*locals.get(s as usize)?)?),
            Op::StoreSlot(s) => {
                let v = stack.pop()?;
                *locals.get_mut(s as usize)? = Some(v);
            }
            Op::Const(ci) => {
                let s = match &ch.consts[ci as usize] {
                    Value::I32(n) => SlotV::Val(b.ins().iconst(types::I32, *n as i64), Ty::I32),
                    Value::Bool(t) => SlotV::Val(b.ins().iconst(types::I32, *t as i64), Ty::Bool),
                    Value::I64(n) => SlotV::Val(b.ins().iconst(types::I64, *n), Ty::I64),
                    Value::F64(x) => SlotV::Val(b.ins().f64const(*x), Ty::F64),
                    _ => return None,
                };
                stack.push(s);
            }
            Op::BinNum(bop, num) => {
                if matches!(bop, BinOp::Div | BinOp::Rem) {
                    return None;
                }
                let (r, _) = scalar(stack.pop()?)?;
                let (l, _) = scalar(stack.pop()?)?;
                let t = ty_of(num)?;
                let (v, rt) = lower_bin(b, bop, l, r, t);
                stack.push(SlotV::Val(v, rt));
            }
            Op::LoadName(ni) => {
                let c = *plans[f].callee.get(&ni)?;
                stack.push(SlotV::Callee(c));
            }
            Op::Call(n) => {
                let mut cargs: Vec<SlotV> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    cargs.push(stack.pop()?);
                }
                cargs.reverse();
                let SlotV::Callee(cf) = stack.pop()? else {
                    return None;
                };
                let r = inline_body(b, plans, cf, &cargs, depth + 1)?;
                stack.push(r);
            }
            Op::Return => return stack.pop(),
            Op::ReturnNull => {}
            _ => return None,
        }
    }
    None
}

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

/// If `v` is the result of an `iconst`, its value; else `None`. Used to spot a
/// compile-time-constant divisor so `/` and `%` avoid a hardware divide.
fn const_int(b: &FunctionBuilder, v: ClValue) -> Option<i64> {
    use cranelift_codegen::ir::{InstructionData, Opcode, ValueDef};
    if let ValueDef::Result(inst, _) = b.func.dfg.value_def(v) {
        if let InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } = b.func.dfg.insts[inst]
        {
            return Some(imm.bits());
        }
    }
    None
}

/// Granlund–Montgomery magic (Hacker's Delight §10-4) for signed division by a
/// constant `d` in a `w`-bit domain (`w` ∈ {32, 64}). Precondition: |d| ≥ 2.
/// Returns `(M, s)` such that the sequence in [`emit_const_divrem`] reproduces
/// `n / d` bit-for-bit — the interpreter's exact result, no fast-math.
fn signed_magic(d: i64, w: u32) -> (i64, u32) {
    let msb: u128 = 1u128 << (w - 1); // 2^(w-1)
    let dw: u128 = (d as i128 as u128) & ((1u128 << w) - 1); // d's low w bits
    let ad: u128 = if d < 0 {
        d.unsigned_abs() as u128
    } else {
        d as u128
    }; // |d|
    let t: u128 = msb + (dw >> (w - 1)); // 2^(w-1) + sign bit of d
    let anc: u128 = t - 1 - (t % ad); // |nc|
    let (mut p, mut q1, mut r1, mut q2, mut r2) = (
        w - 1,
        msb / anc,
        msb - (msb / anc) * anc,
        msb / ad,
        msb - (msb / ad) * ad,
    );
    loop {
        p += 1;
        q1 <<= 1;
        r1 <<= 1;
        if r1 >= anc {
            q1 += 1;
            r1 -= anc;
        }
        q2 <<= 1;
        r2 <<= 1;
        if r2 >= ad {
            q2 += 1;
            r2 -= ad;
        }
        let delta = ad - r2;
        if q1 >= delta && !(q1 == delta && r1 == 0) {
            break;
        }
    }
    // M = q2 + 1, sign-extended from w bits, negated when d < 0.
    let mag = if w == 32 {
        let mi = (q2 + 1) as u32 as i32;
        (if d < 0 { mi.wrapping_neg() } else { mi }) as i64
    } else {
        let mi = (q2 + 1) as u64 as i64;
        if d < 0 {
            mi.wrapping_neg()
        } else {
            mi
        }
    };
    (mag, p - w)
}

/// Emit `n / d` (rem=false) or `n % d` (rem=true) for a constant `d`, |d| ≥ 2,
/// without a hardware divide. No zero/`INT_MIN` guard is needed: a nonzero
/// constant that is not −1 can neither divide by zero nor overflow.
fn emit_const_divrem(b: &mut FunctionBuilder, n: ClValue, d: i64, t: Ty, rem: bool) -> ClValue {
    let clt = t.cl();
    let w = if t == Ty::I64 { 64 } else { 32 };
    let (m, s) = signed_magic(d, w);
    let mc = b.ins().iconst(clt, m);
    let hi = b.ins().smulhi(mc, n); // high w bits of signed M*n
    let adj = if d > 0 && m < 0 {
        b.ins().iadd(hi, n)
    } else if d < 0 && m > 0 {
        b.ins().isub(hi, n)
    } else {
        hi
    };
    let sh = if s > 0 {
        b.ins().sshr_imm(adj, s as i64)
    } else {
        adj
    };
    let sign = b.ins().ushr_imm(sh, (w - 1) as i64); // 1 iff quotient is negative
    let q = b.ins().iadd(sh, sign); // n / d
    if !rem {
        return q;
    }
    let dd = b.ins().iconst(clt, d);
    let qd = b.ins().imul(q, dd);
    b.ins().isub(n, qd) // n - (n/d)*d
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
        (
            "value layout verified before heap access",
            heap::layout_holds(),
        ),
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
pub const KNOWN_GAPS: &[(&str, &str)] = &[
    (
        "forward-edge CFI (CET/endbr64)",
        "Cranelift exposes no CET setting (checked 0.116, 0.123); x86-64 only",
    ),
    (
        "object-returning functions are compiled",
        "a returned object is (ptr, fields, handle) — 3 return values that fit \
         aarch64's return registers but overflow x86-64's rax:rdx (Cranelift \
         #9510). On x86-64 such functions fall back to the interpreter (correct \
         results, not compiled). Fix: return objects through an out-pointer.",
    ),
];

#[cfg(test)]
mod divmagic_tests {
    use super::signed_magic;

    // Mirror emit_const_divrem's exact instruction sequence in plain Rust, so
    // the magic (M, s) is checked against real division independent of Cranelift.
    fn reduce32(n: i32, d: i32) -> (i32, i32) {
        let (m64, s) = signed_magic(d as i64, 32);
        let m = m64 as i32;
        let hi = (((m as i64) * (n as i64)) >> 32) as i32; // smulhi
        let adj = if d > 0 && m < 0 {
            hi.wrapping_add(n)
        } else if d < 0 && m > 0 {
            hi.wrapping_sub(n)
        } else {
            hi
        };
        let q = adj >> s;
        let q = q.wrapping_add(((q as u32) >> 31) as i32); // + sign bit
        (q, n.wrapping_sub(q.wrapping_mul(d)))
    }

    fn reduce64(n: i64, d: i64) -> (i64, i64) {
        let (m, s) = signed_magic(d, 64);
        let hi = (((m as i128) * (n as i128)) >> 64) as i64; // smulhi
        let adj = if d > 0 && m < 0 {
            hi.wrapping_add(n)
        } else if d < 0 && m > 0 {
            hi.wrapping_sub(n)
        } else {
            hi
        };
        let q = adj >> s;
        let q = q.wrapping_add(((q as u64) >> 63) as i64);
        (q, n.wrapping_sub(q.wrapping_mul(d)))
    }

    #[test]
    fn magic_matches_hardware_i32() {
        let ds = [2, 3, 4, 7, 8, 10, 100, 1000, 65536, -3, -7, -100, i32::MAX];
        for &d in &ds {
            let mut x: i32 = 0x1234_5678;
            for _ in 0..5000 {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                assert_eq!(
                    reduce32(x, d),
                    (x.wrapping_div(d), x.wrapping_rem(d)),
                    "n={x} d={d}"
                );
            }
            for n in [i32::MIN + 1, -1000, -1, 0, 1, 1000, i32::MAX] {
                assert_eq!(
                    reduce32(n, d),
                    (n.wrapping_div(d), n.wrapping_rem(d)),
                    "n={n} d={d}"
                );
            }
        }
    }

    #[test]
    fn magic_matches_hardware_i64() {
        let ds = [2i64, 3, 100, 1_000_000, -3, -1000, i32::MAX as i64 + 1];
        for &d in &ds {
            let mut x: i64 = 0x1234_5678_9abc_def0u64 as i64;
            for _ in 0..5000 {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                assert_eq!(
                    reduce64(x, d),
                    (x.wrapping_div(d), x.wrapping_rem(d)),
                    "n={x} d={d}"
                );
            }
            for n in [i64::MIN + 1, -1, 0, 1, i64::MAX] {
                assert_eq!(
                    reduce64(n, d),
                    (n.wrapping_div(d), n.wrapping_rem(d)),
                    "n={n} d={d}"
                );
            }
        }
    }
}
