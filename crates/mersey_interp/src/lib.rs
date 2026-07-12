//! MVP tree-walking interpreter. Executes a bound module directly from the
//! AST. This is the Phase-"MVP" execution engine; the typed-bytecode VM and
//! JIT (ROADMAP Phases 2/4) replace it without changing observable behavior
//! — the runtime conformance suite is the contract.
//!
//! Semantics honored from the spec: UTF-32 strings with O(1) code-point
//! indexing (§3.4), C-style numeric promotion with defined wrapping (§3.3,
//! §3.6: division by zero and `INT_MIN / -1` throw `RangeError`, shift
//! counts masked), checked vs `wrapping` casts, sealed class shapes (§4.1:
//! assigning an undeclared field throws), class-chain method dispatch with
//! `super`, and module-level declaration hoisting (§4.5).
//!
//! Deliberately out of MVP scope (clean `TypeError` at runtime, tracked in
//! ROADMAP): `bigint`/`bigdec` arithmetic, `async`/`await`, dynamic
//! `import()`, multi-module graphs.
//!
//! The AST is borrowed with `&'static` lifetime; drivers leak one parsed
//! module per program (bounded, lives for the process/page lifetime).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mersey_front::ast::*;

pub mod bignum;
pub mod gc;
use gc::GcCell;
pub mod regex;
pub mod vm;
pub mod webjson;
use bignum::{BigDec, BigInt, RoundingMode};
use webjson::Json;

// ---- host interface ---------------------------------------------------------

/// Everything the interpreter can ask of the outside world. The CLI wires
/// this to stdout; the browser build wires it to `console`/DOM via the
/// loader (docs/architecture/browser-integration.md, Stage A).
pub trait Host {
    fn print(&mut self, s: &str);
    fn dom_set_text(&mut self, id: &str, text: &str);
    fn dom_get_text(&mut self, id: &str) -> Option<String>;
    /// Register callback `cb` (an index the driver passes back to
    /// `Interp::invoke_callback`) for click events on element `id`.
    /// Register `cb` as a listener for `event` on element `id`. The host owns
    /// the event loop; the engine only ever hands it a callback id.
    fn dom_add_listener(&mut self, id: &str, event: &str, cb: u32);

    /// Create an element; returns its handle id.
    fn dom_create(&mut self, _tag: &str) -> String {
        String::new()
    }
    fn dom_append(&mut self, _parent: &str, _child: &str) {}
    fn dom_remove(&mut self, _id: &str) {}
    fn dom_get_value(&mut self, _id: &str) -> String {
        String::new()
    }
    fn dom_set_value(&mut self, _id: &str, _value: &str) {}

    // ---- capability-gated I/O (spec §5.3): deny by default -------------
    fn read_text(&mut self, _path: &str) -> Result<String, String> {
        Err("no `read` capability (run with --allow-read)".into())
    }
    fn env_var(&mut self, _name: &str) -> Option<String> {
        None
    }
    fn caps(&self) -> Vec<String> {
        Vec::new()
    }
    fn drop_cap(&mut self, _cap: &str) {}

    // ---- universal web bridge (spec §5.4: import-gated) ---------------
    /// Resolve an ambient global to a handle; -1 = unavailable.
    fn web_global(&mut self, _name: &str) -> i64 {
        -1
    }
    /// Read `target[prop]`; returns a tagged-JSON WebValue, or an
    /// `{"err":"…"}` object. Default: not available.
    fn web_get(&mut self, _target: i64, _prop: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set(&mut self, _target: i64, _prop: &str, _value_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Call `target[method](args)`; method "" calls the target itself.
    fn web_call(&mut self, _target: i64, _method: &str, _args_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_new(&mut self, _ctor: &str, _args_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }

    // ---- fast paths (avoid JSON + per-call string decoding) -------------
    /// Intern a member name; the id is stable for the host's lifetime.
    fn web_intern(&mut self, _name: &str) -> u32 {
        u32::MAX
    }
    fn web_get_id(&mut self, _target: i64, _name_id: u32) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set_str(&mut self, _target: i64, _name_id: u32, _value: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set_num(&mut self, _target: i64, _name_id: u32, _value: f64) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Call with a single string argument (`createElement("span")`, …).
    fn web_call_str(&mut self, _target: i64, _name_id: u32, _arg: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Snapshot a host iterable (NodeList, HTMLCollection, Set, …) as an
    /// array, so `for (const n of nodeList)` works.
    fn web_iterate(&mut self, _target: i64) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Drop a host handle (and any callbacks it retained).
    fn web_release(&mut self, _target: i64) {}

    /// Bulk-copy a host typed array / ArrayBuffer into engine memory.
    fn web_bytes_read(&mut self, _target: i64) -> Option<Vec<u8>> {
        None
    }
    /// Bulk-copy engine bytes back into a fresh host Uint8ClampedArray-ish
    /// object; returns its handle (or -1).
    fn web_bytes_write(&mut self, _bytes: &[u8]) -> i64 {
        -1
    }
    /// `object instanceof constructor` on the host side.
    fn web_instanceof(&mut self, _target: i64, _ctor: i64) -> bool {
        false
    }
    /// Wall-clock (`epoch = true`) or monotonic milliseconds. Time is not
    /// capability-gated: it leaks no data the program didn't already have.
    fn time_ms(&mut self, _epoch: bool) -> f64 {
        0.0
    }
}

// ---- values -------------------------------------------------------------------

type Str = Rc<Vec<char>>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    I32(i32),
    I64(i64),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Char(char),
    Str(Str),
    BigIntV(Rc<BigInt>),
    BigDecV(Rc<BigDec>),
    Array(Rc<GcCell<Vec<Value>>>),
    /// Insertion-ordered map; key equality is `values_equal` (O(n) MVP).
    MapV(Rc<GcCell<Vec<(Value, Value)>>>),
    SetV(Rc<GcCell<Vec<Value>>>),
    /// Insertion-ordered fields: a record's field order is part of its
    /// observable behaviour (it survives `JSON.stringify` across the bridge).
    Record(Rc<GcCell<Vec<(String, Value)>>>),
    Closure(Rc<Closure>),
    Class(Rc<ClassDef>),
    Instance(Rc<GcCell<Instance>>),
    /// `console`, `document`, enum objects: named bags of values.
    Namespace(Rc<Namespace>),
    /// A DOM element handle (Stage A: identified by element id).
    Dom(Rc<String>),
    /// Opaque handle to a host (JS) object, reached via the universal
    /// bridge. Handle 0 is the global object (window).
    JsRef(i64),
    /// Packed byte buffer with O(1) element access — the engine-side home
    /// for pixel/audio/binary data (no per-element bridge hops).
    Bytes(Rc<RefCell<Vec<u8>>>),
    /// A compiled regular expression.
    RegexV(Rc<regex::Regex>),
    /// A generator: a coroutine that produces values (`Iter<T>`).
    IterV(Rc<GcCell<GenState>>),
    /// A Mersey promise (§ async/await).
    PromiseV(Rc<GcCell<PromiseState>>),
    /// A callable that settles a promise; handed to host `.then(…)` so JS
    /// promises can resume Mersey coroutines.
    Resolver(Rc<GcCell<PromiseState>>, bool),
    /// Internal reaction used by `Promise.all` (slot index, is_reject).
    AllSlot(u32, bool),
    /// Executor handed to `new Promise(…)` on the host side: receives the
    /// host's (resolve, reject) and wires them to a Mersey promise, so a
    /// Mersey promise can cross the bridge as a *real* JS promise.
    PromiseExec(Rc<GcCell<PromiseState>>),
    Native(&'static str),
}

/// One input slot of a pending `Promise.all`.
struct AllCell {
    results: Rc<GcCell<Vec<Value>>>,
    remaining: Rc<RefCell<usize>>,
    out: Rc<GcCell<PromiseState>>,
    idx: usize,
}

#[derive(Clone, PartialEq)]
pub enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

pub struct PromiseState {
    pub status: PromiseStatus,
    pub value: Value,
    /// Coroutines awaiting this promise.
    waiters: Vec<Coro>,
    /// `then`/`catch` reactions: (on_fulfilled, on_rejected, downstream).
    reactions: Vec<(Option<Value>, Option<Value>, Rc<GcCell<PromiseState>>)>,
}

impl PromiseState {
    pub(crate) fn waiters(&self) -> &[Coro] {
        &self.waiters
    }
    #[allow(clippy::type_complexity)]
    pub(crate) fn reactions(&self) -> &[(Option<Value>, Option<Value>, Rc<GcCell<PromiseState>>)] {
        &self.reactions
    }

    /// Sweep: drop every edge out of an unreachable promise. Nothing can
    /// settle it or observe it, so its waiters and reactions are dead too.
    pub(crate) fn clear_edges(&mut self) {
        let mut sink = Vec::new();
        self.value = Value::Null;
        self.take_edges(&mut sink);
    }

    /// Move every value this promise holds into `out` — its own result, and
    /// whatever its waiting coroutines and reactions were holding.
    pub(crate) fn take_edges(&mut self, out: &mut Vec<Value>) {
        for coro in std::mem::take(&mut self.waiters) {
            out.extend(coro.stack);
        }
        for (ok, err, _) in std::mem::take(&mut self.reactions) {
            out.extend(ok);
            out.extend(err);
        }
    }

    fn pending() -> Rc<GcCell<PromiseState>> {
        let p = Rc::new(GcCell::new(PromiseState {
            status: PromiseStatus::Pending,
            value: Value::Null,
            waiters: Vec::new(),
            reactions: Vec::new(),
        }));
        gc::track_promise(&p);
        p
    }
}

/// A suspended generator.
pub struct GenState {
    coro: Option<Coro>,
    done: bool,
    /// An *async* generator: one coroutine that both yields and awaits. The VM
    /// already reports all three outcomes (done, yielded, awaiting), so this
    /// needs no second mechanism — only somewhere to put the promise that the
    /// current `next()` will settle when the body finally reaches a `yield`.
    is_async: bool,
    /// The promise handed out by the `next()` now in flight.
    pending: Option<Rc<GcCell<PromiseState>>>,
}

impl GenState {
    /// The suspended coroutine, if this generator has not finished. Its saved
    /// operand stack and scopes are GC roots for as long as it can be resumed.
    pub(crate) fn saved(&self) -> Option<Coro> {
        self.coro.clone()
    }

    /// The promise the in-flight `next()` will settle, if this is an async
    /// generator that is mid-await.
    pub(crate) fn pending_next(&self) -> Option<Rc<GcCell<PromiseState>>> {
        self.pending.clone()
    }

    /// Sweep: an unreachable generator can never be resumed, so drop the
    /// coroutine it was holding (which is where its cycle runs through).
    pub(crate) fn discard(&mut self) {
        let mut sink = Vec::new();
        self.take_coro(&mut sink);
    }

    /// Move the suspended coroutine's values into `out`.
    pub(crate) fn take_coro(&mut self, out: &mut Vec<Value>) {
        if let Some(coro) = self.coro.take() {
            out.extend(coro.stack);
        }
        self.pending = None;
        self.done = true;
    }
}

/// How a module's top level finished.
enum ModuleFlow {
    Done,
    /// The module's top-level `await` is still waiting on something only the
    /// host can settle. Everything that imports it waits too.
    Awaiting(Rc<GcCell<PromiseState>>),
}

/// A module graph paused on a top-level `await`.
struct PendingGraph {
    /// The suspended module's completion promise.
    promise: Rc<GcCell<PromiseState>>,
    spec: String,
    module: &'static Module,
    /// The suspended module's own scope, so its exports can be collected once
    /// it finishes.
    env: Env,
    /// Modules that have not run yet — the ones that import it, and their
    /// importers.
    rest: Vec<(String, &'static Module)>,
}

/// One entry of the diagnostic call stack.
pub struct Frame_ {
    pub name: String,
    pub module: String,
    pub pos: mersey_front::diag::Pos,
}

/// A suspended async function: the VM's whole state is data, so `await`
/// captures it and resumes later (no CPS transform, no threads).
#[derive(Clone)]
pub struct Coro {
    /// The async generator this coroutine belongs to, if any. An `await` inside
    /// it suspends through the ordinary promise machinery; when the microtask
    /// queue resumes it, this is how the engine knows a `yield` must settle the
    /// generator's pending `next()` rather than the coroutine's own result.
    pub(crate) gen: Option<Rc<GcCell<GenState>>>,
    pub chunk: Rc<vm::Chunk>,
    pub pc: usize,
    pub stack: Vec<Value>,
    pub scopes: Vec<Env>,
    pub handlers: Vec<(usize, usize, usize)>,
    pub cls: Option<Rc<ClassDef>>,
    /// The promise this coroutine's completion settles.
    pub result: Rc<GcCell<PromiseState>>,
}

/// Work the engine owes itself before returning to the host.
enum Task {
    Resume(Coro, Value, bool),
    React(
        Option<Value>,
        Option<Value>,
        Rc<GcCell<PromiseState>>,
        Value,
        bool,
    ),
}

pub struct Namespace {
    pub name: String,
    pub entries: HashMap<String, Value>,
}

pub struct Closure {
    data: Rc<FnData>,
    pub(crate) env: Env,
    pub(crate) this: Option<Value>,
    /// Class that lexically contains the function (for `super`).
    pub(crate) cls: Option<Rc<ClassDef>>,
}

struct FnData {
    #[allow(dead_code)] // future stack traces
    name: String,
    is_async: bool,
    params: &'static [Param],
    body: FnBody,
    /// Lazily compiled bytecode: None = not tried, Some(None) = this body
    /// uses a construct the compiler doesn't cover (AST fallback),
    /// Some(Some(chunk)) = compiled.
    chunk: RefCell<Option<Option<Rc<vm::Chunk>>>>,
}

impl FnData {
    fn new(name: String, is_async: bool, params: &'static [Param], body: FnBody) -> FnData {
        FnData {
            name,
            is_async,
            params,
            body,
            chunk: RefCell::new(None),
        }
    }
}

enum FnBody {
    Block(&'static [Stmt]),
    Expr(&'static Expr),
}

pub struct ClassDef {
    /// Process-unique, never reused. Inline caches key on this rather than on
    /// the `Rc` address, which a later class could otherwise reuse after a
    /// free and silently make a stale cache hit (§4.1 layouts differ).
    pub(crate) id: u64,
    name: String,
    pub(crate) parent: Option<Rc<ClassDef>>,
    /// Instance fields in initialization order (base-class fields first).
    /// Sealed shapes (§4.1) mean this layout is fixed at class-definition
    /// time, so a field is a **constant offset** — the whole point of
    /// removing prototypes.
    fields: Vec<(String, Option<&'static Expr>)>,
    /// name → slot, computed once when the class is defined.
    field_slots: HashMap<String, u32>,
    methods: HashMap<String, Rc<FnData>>,
    getters: HashMap<String, Rc<FnData>>,
    setters: HashMap<String, Rc<FnData>>,
    ctor: Option<Rc<FnData>>,
    pub(crate) statics: GcCell<HashMap<String, Value>>,
    static_methods: HashMap<String, Rc<FnData>>,
    /// Built-in error classes construct without an AST ctor.
    is_builtin_error: bool,
    /// Host interface this class extends, if any (`extends HTMLElement`).
    host_iface: Option<String>,
    pub(crate) env: Option<Env>,
}

pub struct Instance {
    pub(crate) class: Rc<ClassDef>,
    /// Flat slots, indexed by the class's fixed layout — a constant-offset
    /// load, not a hash lookup.
    pub(crate) slots: Vec<Value>,
    /// Host object backing this instance (`class X extends HTMLElement`):
    /// members not declared in Mersey resolve against it, and the instance
    /// crosses the bridge AS that object.
    host: Option<i64>,
}

// ---- environments ----------------------------------------------------------------

type Env = Rc<GcCell<Scope>>;

pub(crate) struct Scope {
    pub(crate) vars: HashMap<String, Value>,
    pub(crate) parent: Option<Env>,
}

fn child_env(parent: &Env) -> Env {
    let e = Rc::new(GcCell::new(Scope {
        vars: HashMap::new(),
        parent: Some(parent.clone()),
    }));
    gc::track_env(&e);
    e
}

fn env_get(env: &Env, name: &str) -> Option<Value> {
    let scope = env.borrow();
    if let Some(v) = scope.vars.get(name) {
        return Some(v.clone());
    }
    scope.parent.as_ref().and_then(|p| env_get(p, name))
}

fn env_set(env: &Env, name: &str, value: Value) -> bool {
    let mut scope = env.borrow_mut();
    if let Some(slot) = scope.vars.get_mut(name) {
        *slot = value;
        return true;
    }
    match &scope.parent {
        Some(p) => env_set(p, name, value),
        None => false,
    }
}

fn env_define(env: &Env, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// ---- control flow / errors ---------------------------------------------------------

enum Sig {
    Normal,
    Return(Value),
    Break(Option<String>),
    Continue(Option<String>),
}

enum LoopCtl {
    BreakLoop,
    NextIter,
    Out(Sig),
}

fn loop_ctl(sig: Sig, label: Option<&str>) -> LoopCtl {
    match sig {
        Sig::Normal | Sig::Continue(None) => LoopCtl::NextIter,
        Sig::Break(None) => LoopCtl::BreakLoop,
        Sig::Continue(Some(l)) if Some(l.as_str()) == label => LoopCtl::NextIter,
        Sig::Break(Some(l)) if Some(l.as_str()) == label => LoopCtl::BreakLoop,
        other => LoopCtl::Out(other),
    }
}

/// A runtime error is always a thrown value (built-in errors are instances
/// of the built-in `Error` classes, so `catch (e: RangeError)` works).
pub struct Thrown(pub Value);

type VResult = Result<Value, Thrown>;
type SResult = Result<Sig, Thrown>;

// ---- interpreter ------------------------------------------------------------------

pub struct Interp {
    host: Box<dyn Host>,
    /// Callback slots the host still holds (a JS listener, a promise
    /// reaction). Cleared slots are reused, so a page that churns through
    /// listeners doesn't grow the table forever.
    free_callbacks: Vec<u32>,
    /// Shared prelude (built-in classes); every module scope descends from it.
    root: Env,
    globals: Env,
    callbacks: Vec<Value>,
    error_classes: HashMap<&'static str, Rc<ClassDef>>,
    /// Class whose method is currently executing (innermost last), for `super`.
    class_stack: Vec<Rc<ClassDef>>,
    /// Call stack for diagnostics: (function name, module, position of the
    /// instruction currently executing in that frame).
    frames: Vec<Frame_>,
    /// Mersey call depth. See `MAX_CALL_DEPTH`.
    depth: usize,
    /// A graph paused on a module's top-level `await`.
    pending_graph: Option<PendingGraph>,
    /// Modules that are in the graph but have not been run: the targets of a
    /// dynamic `import(…)`. They were loaded, checked and locked with the rest
    /// (§4.5 — the graph is closed); they simply do not execute until someone
    /// imports them.
    lazy_modules: HashMap<String, &'static Module>,
    /// Stack address at the last host boundary — the base the budget measures
    /// growth from. See `STACK_BUDGET`.
    stack_base: usize,
    /// Execute compiled bytecode where available (Tier 0); AST fallback
    /// otherwise. Off = pure tree-walking (differential-test oracle).
    pub use_vm: bool,
    /// Microtask queue (promise reactions + coroutine resumptions), drained
    /// before control returns to the host — the engine owns no event loop
    /// (embedding-api.md rule 1); the host owns timers and I/O.
    tasks: std::collections::VecDeque<Task>,
    all_cells: Vec<AllCell>,
    /// Member-name interning: a name crosses the ABI once, then it is an id.
    interned: HashMap<String, u32>,
    /// Evaluated modules: specifier → its exported bindings.
    modules: HashMap<String, HashMap<String, Value>>,
    /// The module currently executing (for relative import resolution).
    current_module: String,
    /// Mersey classes declared in the module being defined but not yet
    /// created, so `extends` can tell a late Mersey base from a host one.
    pending_class_names: std::collections::HashSet<String>,
    /// A `gc.collect()` request from Mersey: honoured at the next safe point.
    gc_pending: bool,
    /// Tier 1: optional JIT backend (native builds register Cranelift).
    pub jit: Option<JitHook>,
    jit_cache: HashMap<usize, Option<JitFn>>,
    call_counts: HashMap<usize, u32>,
}

/// An argument to a JIT kernel (kernels are homogeneous: all int or all float).
#[derive(Clone, Copy)]
pub enum JitArg {
    I32(i32),
    F64(f64),
}

/// What a JIT kernel returned. `Bail` means the kernel hit a condition the
/// spec says must throw (`x / 0`, `INT_MIN / -1`) — the interpreter re-runs
/// the call so the error carries a proper message and stack trace. This is a
/// trap at the *edge*, not a deopt in the middle: compiled code never
/// resumes.
pub enum JitResult {
    I32(i32),
    F64(f64),
    Null,
    Bail,
}

/// A JIT-compiled kernel.
pub type JitFn = Rc<dyn Fn(&[JitArg]) -> JitResult>;
/// Backend entry: compile a chunk whose parameters are the given simple
/// names; None = outside the JIT-able subset.
pub type JitHook = fn(&vm::Chunk, &[String]) -> Option<JitFn>;

/// Calls before a function is considered hot (Tier 1 threshold).
const JIT_THRESHOLD: u32 = 64;

pub fn new_interp(host: Box<dyn Host>) -> Interp {
    let root = Rc::new(GcCell::new(Scope {
        vars: HashMap::new(),
        parent: None,
    }));
    let globals = child_env(&root);
    let mut error_classes = HashMap::new();
    let base = Rc::new(builtin_error_class("Error", None));
    for name in ["RangeError", "TypeError"] {
        error_classes.insert(name, Rc::new(builtin_error_class(name, Some(base.clone()))));
    }
    error_classes.insert("Error", base);
    for (name, cls) in &error_classes {
        env_define(&root, name, Value::Class(cls.clone()));
    }
    Interp {
        host,
        free_callbacks: Vec::new(),
        root,
        globals,
        callbacks: Vec::new(),
        error_classes,
        class_stack: Vec::new(),
        frames: Vec::new(),
        depth: 0,
        pending_graph: None,
        lazy_modules: HashMap::new(),
        stack_base: stack_here(),
        use_vm: true,
        tasks: std::collections::VecDeque::new(),
        all_cells: Vec::new(),
        interned: HashMap::new(),
        modules: HashMap::new(),
        current_module: String::new(),
        pending_class_names: std::collections::HashSet::new(),
        gc_pending: false,
        jit: None,
        jit_cache: HashMap::new(),
        call_counts: HashMap::new(),
    }
}

thread_local! {
    static NEXT_CLASS_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

fn fresh_class_id() -> u64 {
    NEXT_CLASS_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

impl ClassDef {
    /// The constant offset of `name` in this class's instances, if it is a
    /// declared field.
    pub(crate) fn slot_of(&self, name: &str) -> Option<u32> {
        self.field_slots.get(name).copied()
    }
}

fn builtin_error_class(name: &'static str, parent: Option<Rc<ClassDef>>) -> ClassDef {
    let fields = vec![("message".to_string(), None), ("stack".to_string(), None)];
    let field_slots = fields
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.clone(), i as u32))
        .collect();
    ClassDef {
        id: fresh_class_id(),
        name: name.to_string(),
        parent,
        field_slots,
        fields,
        methods: HashMap::new(),
        getters: HashMap::new(),
        setters: HashMap::new(),
        ctor: None,
        statics: GcCell::new(HashMap::new()),
        static_methods: HashMap::new(),
        is_builtin_error: true,
        host_iface: None,
        env: None,
    }
}

impl Interp {
    /// Public throw for the VM module.
    pub(crate) fn throw_public(&self, class: &'static str, msg: impl Into<String>) -> Thrown {
        self.throw(class, msg)
    }

    fn throw(&self, class: &'static str, msg: impl Into<String>) -> Thrown {
        let cls = self.error_classes[class].clone();
        let stack = self.stack_trace();
        let mut slots = vec![Value::Null; cls.fields.len()];
        slots[0] = Value::Str(Rc::new(msg.into().chars().collect())); // message
        if slots.len() > 1 {
            slots[1] = Value::Str(Rc::new(stack.chars().collect())); // stack
        }
        Thrown(Value::Instance(Rc::new(GcCell::new(Instance {
            class: cls,
            slots,
            host: None,
        }))))
    }

    /// `at fn (module:line:col)` per frame, innermost first.
    ///
    /// Deep traces are truncated: a runaway recursion has thousands of
    /// identical frames, and a multi-megabyte error message is its own denial
    /// of service — the frames that say something are the ones at each end.
    pub fn stack_trace(&self) -> String {
        const HEAD: usize = 12;
        const TAIL: usize = 4;
        let n = self.frames.len();
        let mut out = String::new();
        let frame_line = |f: &Frame_| {
            let loc = if f.pos.line > 0 {
                format!("{}:{}:{}", f.module, f.pos.line, f.pos.col)
            } else {
                f.module.clone()
            };
            format!("\n    at {} ({loc})", f.name)
        };
        for (i, f) in self.frames.iter().rev().enumerate() {
            if n > HEAD + TAIL + 1 && i == HEAD {
                let hidden = n - HEAD - TAIL;
                out.push_str(&format!("\n    ... {hidden} more frames"));
            }
            if n > HEAD + TAIL + 1 && i >= HEAD && i < n - TAIL {
                continue;
            }
            out.push_str(&frame_line(f));
        }
        out
    }

    /// Update the position of the innermost frame (called by the VM loop).
    pub(crate) fn set_site(&mut self, pos: mersey_front::diag::Pos) {
        if let Some(f) = self.frames.last_mut() {
            f.pos = pos;
        }
    }

    pub(crate) fn push_frame(&mut self, name: &str, module: &str) {
        self.frames.push(Frame_ {
            name: name.to_string(),
            module: module.to_string(),
            pos: mersey_front::diag::Pos { line: 0, col: 0 },
        });
    }

    pub(crate) fn pop_frame(&mut self) {
        self.frames.pop();
    }

    fn type_error<T>(&self, msg: impl Into<String>) -> Result<T, Thrown> {
        Err(self.throw("TypeError", msg))
    }

    /// Render a thrown value for host error reporting.
    pub fn describe_thrown(&self, t: &Thrown) -> String {
        match &t.0 {
            Value::Instance(i) => {
                let i = i.borrow();
                let get = |name: &str| {
                    i.class
                        .field_slots
                        .get(name)
                        .and_then(|s| i.slots.get(*s as usize))
                        .map(to_display)
                        .unwrap_or_default()
                };
                format!("{}: {}{}", i.class.name, get("message"), get("stack"))
            }
            other => format!("uncaught: {}", to_display(other)),
        }
    }

    // ---- module execution ------------------------------------------------------

    /// Execute a module graph (dependency-first). Each module gets its own
    /// scope; imports link to the exporting module's evaluated bindings.
    pub fn run_graph(&mut self, modules: Vec<(String, &'static Module)>) -> Result<(), Thrown> {
        // The host may call in at any stack depth: measure growth from here.
        self.stack_base = stack_here();
        self.run_modules(modules)?;
        self.maybe_collect();
        Ok(())
    }

    /// Run modules in dependency order, stopping if one suspends on a
    /// top-level `await` — its importers cannot run until it has finished.
    fn run_modules(&mut self, modules: Vec<(String, &'static Module)>) -> Result<(), Thrown> {
        let mut queue = modules.into_iter();
        while let Some((spec, module)) = queue.next() {
            let env = child_env(&self.root);
            let saved_globals = std::mem::replace(&mut self.globals, env.clone());
            let saved_spec = std::mem::replace(&mut self.current_module, spec.clone());
            let result = self.run_module_inner(module);
            let exports = match &result {
                Ok(ModuleFlow::Done) => collect_exports(module, &env),
                _ => HashMap::new(),
            };
            self.globals = saved_globals;
            self.current_module = saved_spec;
            match result? {
                ModuleFlow::Done => {
                    self.modules.insert(spec, exports);
                }
                ModuleFlow::Awaiting(promise) => {
                    self.pending_graph = Some(PendingGraph {
                        promise,
                        spec,
                        module,
                        env,
                        rest: queue.collect(),
                    });
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Is a paused graph now able to continue?
    fn graph_can_resume(&self) -> bool {
        self.pending_graph
            .as_ref()
            .is_some_and(|p| p.promise.borrow().status != PromiseStatus::Pending)
    }

    /// The awaited thing settled: finish that module and run the ones waiting
    /// on it.
    fn resume_graph(&mut self) -> Result<(), Thrown> {
        let Some(p) = self.pending_graph.take() else {
            return Ok(());
        };
        let (status, value) = {
            let st = p.promise.borrow();
            (st.status.clone(), st.value.clone())
        };
        if status == PromiseStatus::Rejected {
            // A module that throws takes its importers with it.
            return Err(Thrown(value));
        }
        let saved_spec = std::mem::replace(&mut self.current_module, p.spec.clone());
        let exports = collect_exports(p.module, &p.env);
        self.current_module = saved_spec;
        self.modules.insert(p.spec, exports);
        self.run_modules(p.rest)
    }

    /// Did the graph stop on a top-level `await` that nothing has settled?
    pub fn graph_is_waiting(&self) -> bool {
        self.pending_graph.is_some()
    }

    /// Register a module that is in the graph but does not run at startup —
    /// the target of a dynamic `import(…)`.
    pub fn register_lazy(&mut self, spec: String, module: &'static Module) {
        self.lazy_modules.insert(spec, module);
    }

    /// `import("./x.mersey")` — a promise of that module's exports.
    ///
    /// The module was already loaded, checked and locked with the rest of the
    /// graph, so this defers *evaluation*, not loading: running code has no
    /// authority to reach for code that was not named up front (§5.4). The
    /// first import runs the module; later ones get the same exports.
    pub(crate) fn dynamic_import(&mut self, spec: &str) -> VResult {
        let target = mersey_front::graph::resolve_module(&self.current_module, spec);
        if !self.modules.contains_key(&target) {
            let Some(module) = self.lazy_modules.get(&target).copied() else {
                return Err(self.throw("Error", format!("`{spec}` is not in the module graph")));
            };
            let env = child_env(&self.root);
            let saved_globals = std::mem::replace(&mut self.globals, env.clone());
            let saved_spec = std::mem::replace(&mut self.current_module, target.clone());
            let result = self.run_module_inner(module);
            let exports = match &result {
                Ok(ModuleFlow::Done) => collect_exports(module, &env),
                _ => HashMap::new(),
            };
            self.globals = saved_globals;
            self.current_module = saved_spec;
            match result? {
                ModuleFlow::Done => {
                    self.modules.insert(target.clone(), exports);
                }
                ModuleFlow::Awaiting(_) => {
                    // The imported module's own top level is awaiting. Nothing
                    // here can wait for it without blocking the whole engine.
                    return Err(self.throw(
                        "Error",
                        format!(
                            "`{spec}` suspends on a top-level `await`; import it statically \
                             so the graph can wait for it"
                        ),
                    ));
                }
            }
        }
        let exports = self.modules.get(&target).cloned().unwrap_or_default();
        let mut fields: Vec<(String, Value)> = exports.into_iter().collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let rec = Rc::new(GcCell::new(fields));
        gc::track_record(&rec);
        let promise = PromiseState::pending();
        self.settle(&promise, Value::Record(rec), false);
        Ok(Value::PromiseV(promise))
    }

    pub fn run_module(&mut self, module: &'static Module) -> Result<(), Thrown> {
        // The host may call in at any stack depth: measure growth from here.
        self.stack_base = stack_here();
        self.run_module_inner(module)?;
        Ok(())
    }

    fn run_module_inner(&mut self, module: &'static Module) -> Result<ModuleFlow, Thrown> {
        let mut decls: Vec<&'static Decl> = Vec::new();
        for item in &module.items {
            match item {
                Item::Import(im) => self.bind_import(im)?,
                Item::Decl(d) => decls.push(d),
                Item::Export(ex) => match &ex.kind {
                    ExportKind::Decl(d) => decls.push(d),
                    // Exported variables execute with the other statements
                    // (second walk below); named re-exports are inert here.
                    ExportKind::Var(_) | ExportKind::Named { .. } => {}
                },
                Item::Stmt(_) => {}
            }
        }

        // Hoist declarations (order-independent, §4.5). Classes may extend
        // classes declared later, so define in dependency order.
        for d in &decls {
            if let Decl::Function(f) = d {
                let data = Rc::new(FnData::new(
                    f.name.text.clone(),
                    f.is_async,
                    &f.params,
                    FnBody::Block(&f.body),
                ));
                let c = Closure {
                    data,
                    env: self.globals.clone(),
                    this: None,
                    cls: None,
                };
                env_define(&self.globals, &f.name.text, Value::Closure(Rc::new(c)));
            }
        }
        let mut pending: Vec<&'static ClassDecl> = decls
            .iter()
            .filter_map(|d| match d {
                Decl::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        self.pending_class_names = pending.iter().map(|c| c.name.text.clone()).collect();
        while !pending.is_empty() {
            let mut still = Vec::new();
            let mut progressed = false;
            for c in pending {
                if self.try_define_class(c)? {
                    progressed = true;
                    self.pending_class_names.remove(&c.name.text);
                } else {
                    still.push(c);
                }
            }
            pending = still;
            if !pending.is_empty() && !progressed {
                let name = &pending[0].name.text;
                return Err(self.throw(
                    "TypeError",
                    format!("cannot resolve base class of `{name}`"),
                ));
            }
        }
        for d in &decls {
            if let Decl::Enum(e) = d {
                self.define_enum(e)?;
            }
        }

        // Execute remaining top-level statements in order (including
        // exported variable statements) — compiled when possible.
        let spec = self.current_module.clone();
        let compiled = vm::compile_module_stmts_in(module, &spec);
        {
            if let Some(chunk) = compiled.clone() {
                let globals = self.globals.clone();
                // Top-level `await`: the module *is* the async function. It runs
                // as a coroutine, and the modules that import it wait for it to
                // settle (§4.5) — exactly what a caller of an async function
                // does.
                //
                // This happens on the bytecode VM whether or not the tree-walker
                // is selected, for the same reason an async *function* does:
                // `await` suspends by capturing VM state, which the AST walker
                // has none of. The two tiers therefore agree on async semantics
                // by construction rather than by keeping two implementations in
                // step.
                if vm::chunk_awaits(&chunk) {
                    let result = PromiseState::pending();
                    let coro = Coro {
                        gen: None,
                        chunk,
                        pc: 0,
                        stack: Vec::new(),
                        scopes: vec![globals],
                        handlers: Vec::new(),
                        cls: None,
                        result: result.clone(),
                    };
                    self.push_frame("<module>", &spec);
                    let out = self.drive(coro, None);
                    self.pop_frame();
                    out?;
                    self.drain_tasks()?;
                    return match result.borrow().status {
                        PromiseStatus::Fulfilled => Ok(ModuleFlow::Done),
                        PromiseStatus::Rejected => Err(Thrown(result.borrow().value.clone())),
                        // Still waiting on something only the host can settle
                        // (a fetch, a timer). The graph continues when it does.
                        PromiseStatus::Pending => Ok(ModuleFlow::Awaiting(result.clone())),
                    };
                }
            }
        }
        if self.use_vm {
            if let Some(chunk) = compiled {
                let globals = self.globals.clone();
                self.push_frame("<module>", &spec);
                let out = vm::run_chunk(self, &chunk, globals);
                self.pop_frame();
                out?;
                self.drain_tasks()?;
                return Ok(ModuleFlow::Done);
            }
        }
        for item in &module.items {
            match item {
                Item::Stmt(s) => {
                    self.exec_stmt(s, &self.globals.clone())?;
                }
                Item::Export(ExportDecl {
                    kind: ExportKind::Var(v),
                    ..
                }) => {
                    self.exec_var(v, &self.globals.clone())?;
                }
                _ => {}
            }
        }
        self.drain_tasks()?;
        Ok(ModuleFlow::Done)
    }

    fn bind_import(&mut self, im: &'static ImportDecl) -> Result<(), Thrown> {
        // `import * as m from "…"`: bind the module (or built-in namespace)
        // under one name. Relative specifiers are handled below, where the
        // module's exports are known.
        let namespace_alias: Option<&'static Name> = match &im.clause {
            Some(ImportClause::Namespace(n)) => Some(n),
            _ => None,
        };
        let names: Vec<&Name> = match &im.clause {
            None => return Ok(()),
            Some(ImportClause::Namespace(_)) => Vec::new(),
            Some(ImportClause::Named(specs)) => specs
                .iter()
                .map(|s| s.alias.as_ref().unwrap_or(&s.name))
                .collect(),
        };
        match im.from.as_str() {
            "std:console" => {
                let mut entries = HashMap::new();
                entries.insert("log".to_string(), Value::Native("console.log"));
                let console = Value::Namespace(Rc::new(Namespace {
                    name: "console".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, console.clone());
                }
                Ok(())
            }
            "std:regex" | "std:parse" => {
                let (ns_name, natives): (&str, &[&str]) = if im.from == "std:regex" {
                    ("regex", &["compile"])
                } else {
                    ("parse", &["int32", "int64", "float64", "bigint", "bigdec"])
                };
                let mut entries = HashMap::new();
                for n in natives {
                    let id: &'static str = Box::leak(format!("{ns_name}.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: ns_name.to_string(),
                    entries,
                }));
                for n in names.iter().chain(namespace_alias.iter()) {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:gc" => {
                let mut entries = HashMap::new();
                for n in ["collect", "stats"] {
                    let id: &'static str = Box::leak(format!("gc.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "gc".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:time" => {
                let mut entries = HashMap::new();
                for n in ["now", "monotonic", "parts", "fromParts"] {
                    let id: &'static str = Box::leak(format!("time.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "time".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:bytes" => {
                let mut entries = HashMap::new();
                for n in ["alloc", "fromHost", "toHost", "fill"] {
                    let id: &'static str = Box::leak(format!("bytes.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "bytes".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:async" => {
                let mut entries = HashMap::new();
                for n in ["resolve", "reject", "all"] {
                    let id: &'static str = Box::leak(format!("promise.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "Promise".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:math" | "std:format" | "std:fs" | "std:env" | "std:caps" => {
                let (ns_name, natives, consts): (&str, &[&str], &[(&str, Value)]) =
                    match im.from.as_str() {
                        "std:math" => (
                            "math",
                            &["abs", "min", "max", "floor", "ceil", "sqrt", "pow"],
                            &[
                                ("PI", Value::F64(std::f64::consts::PI)),
                                ("E", Value::F64(std::f64::consts::E)),
                            ],
                        ),
                        "std:format" => ("format", &["pad", "fixed"], &[]),
                        "std:fs" => ("fs", &["readText"], &[]),
                        "std:env" => ("env", &["get"], &[]),
                        _ => ("caps", &["has", "list", "drop"], &[]),
                    };
                let mut entries = HashMap::new();
                for n in natives {
                    // Native ids are `<ns>.<method>`, leaked once per import.
                    let id: &'static str = Box::leak(format!("{ns_name}.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(id));
                }
                for (n, v) in consts {
                    entries.insert(n.to_string(), v.clone());
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: ns_name.to_string(),
                    entries,
                }));
                for n in names.iter().chain(namespace_alias.iter()) {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "browser:dom" => {
                for n in names {
                    // Engine-provided helpers (not IDL): explicit handle
                    // release for long-lived pages.
                    if n.text == "release" {
                        env_define(&self.globals, "release", Value::Native("web.release"));
                        continue;
                    }
                    // Bind a Mersey instance of a host-backed class to an
                    // existing host object (the browser builds custom elements).
                    if n.text == "attach" {
                        env_define(&self.globals, "attach", Value::Native("web.attach"));
                        continue;
                    }
                    // Fast path: the hand-written DOM surface (kept because
                    // the Stage A demos and goldens pin it).
                    if n.text == "document" && self.host.web_global("document") < 0 {
                        let mut entries = HashMap::new();
                        entries.insert(
                            "getElementById".to_string(),
                            Value::Native("dom.getElementById"),
                        );
                        entries.insert(
                            "createElement".to_string(),
                            Value::Native("dom.createElement"),
                        );
                        let document = Value::Namespace(Rc::new(Namespace {
                            name: "document".to_string(),
                            entries,
                        }));
                        env_define(&self.globals, &n.text, document);
                        continue;
                    }
                    // General path: any ambient web global, via the bridge.
                    let handle = self.host.web_global(&n.text);
                    if handle < 0 {
                        return self.type_error(format!(
                            "`{}` is not available in this host (no web bridge)",
                            n.text
                        ));
                    }
                    env_define(&self.globals, &n.text, Value::JsRef(handle));
                }
                Ok(())
            }
            other if crate::graph_is_module(other) => {
                let target = mersey_front::graph::resolve_module(&self.current_module, other);
                let Some(exports) = self.modules.get(&target).cloned() else {
                    return self.type_error(format!(
                        "module `{other}` was not loaded (resolved to `{target}`)"
                    ));
                };
                match &im.clause {
                    Some(ImportClause::Named(specs)) => {
                        for s in specs {
                            let local = s.alias.as_ref().unwrap_or(&s.name);
                            match exports.get(&s.name.text) {
                                Some(v) => env_define(&self.globals, &local.text, v.clone()),
                                None => {
                                    return self.type_error(format!(
                                        "`{}` is not exported by `{other}`",
                                        s.name.text
                                    ))
                                }
                            }
                        }
                    }
                    Some(ImportClause::Namespace(n)) => {
                        let ns = Value::Namespace(Rc::new(Namespace {
                            name: n.text.clone(),
                            entries: exports,
                        }));
                        env_define(&self.globals, &n.text, ns);
                    }
                    None => {}
                }
                Ok(())
            }
            other => self.type_error(format!(
                "module `{other}` is not available (built-ins: std:console, std:math, \
                 std:format, std:fs, std:env, std:caps, std:async, browser:dom)"
            )),
        }
    }

    fn try_define_class(&mut self, c: &'static ClassDecl) -> Result<bool, Thrown> {
        let mut host_iface: Option<String> = None;
        let parent = match &c.extends {
            None => None,
            Some(Type::Named { name, .. }) => {
                let head = name.split('.').next().unwrap_or(name).to_string();
                match env_get(&self.globals, &head) {
                    Some(Value::Class(p)) => Some(p),
                    // A Mersey base class declared later in this module.
                    _ if self.pending_class_names.contains(&head) => return Ok(false),
                    // Otherwise it is a host interface (`extends HTMLElement`):
                    // instances are backed by host objects.
                    _ => {
                        host_iface = Some(head);
                        None
                    }
                }
            }
            Some(_) => return self.type_error("invalid extends clause"),
        };
        // A Mersey base class may itself be host-backed: inherit that.
        if host_iface.is_none() {
            if let Some(p) = &parent {
                host_iface = p.host_iface.clone();
            }
        }

        let mut fields: Vec<(String, Option<&'static Expr>)> = Vec::new();
        if let Some(p) = &parent {
            fields.extend(p.fields.iter().map(|(n, e)| (n.clone(), *e)));
        }
        let mut methods = HashMap::new();
        let mut getters = HashMap::new();
        let mut setters = HashMap::new();
        let mut static_methods = HashMap::new();
        let mut ctor = None;
        let statics: GcCell<HashMap<String, Value>> = GcCell::new(HashMap::new());

        for m in &c.members {
            match m {
                ClassMember::Field {
                    mods, name, init, ..
                } => {
                    if mods.is_static {
                        let v = match init {
                            Some(e) => self.eval(e, &self.globals.clone())?,
                            None => Value::Null,
                        };
                        statics.borrow_mut().insert(name.clone(), v);
                    } else {
                        fields.push((name.clone(), init.as_ref()));
                    }
                }
                ClassMember::Method {
                    mods,
                    is_async,
                    name,
                    params,
                    body,
                    ..
                } => {
                    if let Some(body) = body {
                        let data = Rc::new(FnData::new(
                            name.clone(),
                            *is_async,
                            params,
                            FnBody::Block(body),
                        ));
                        if mods.is_static {
                            static_methods.insert(name.clone(), data);
                        } else {
                            methods.insert(name.clone(), data);
                        }
                    }
                }
                ClassMember::Getter { name, body, .. } => {
                    getters.insert(
                        name.clone(),
                        Rc::new(FnData::new(name.clone(), false, &[], FnBody::Block(body))),
                    );
                }
                ClassMember::Setter {
                    name, param, body, ..
                } => {
                    setters.insert(
                        name.clone(),
                        Rc::new(FnData::new(
                            name.clone(),
                            false,
                            std::slice::from_ref(param),
                            FnBody::Block(body),
                        )),
                    );
                }
                ClassMember::Ctor { params, body, .. } => {
                    ctor = Some(Rc::new(FnData::new(
                        format!("{}.constructor", c.name.text),
                        false,
                        params,
                        FnBody::Block(body),
                    )));
                }
            }
        }

        let field_slots: HashMap<String, u32> = fields
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as u32))
            .collect();
        let def = Rc::new(ClassDef {
            id: fresh_class_id(),
            name: c.name.text.clone(),
            parent,
            field_slots,
            fields,
            methods,
            getters,
            setters,
            ctor,
            statics,
            static_methods,
            is_builtin_error: false,
            host_iface,
            env: Some(self.globals.clone()),
        });
        gc::track_class(&def);
        env_define(&self.globals, &c.name.text, Value::Class(def));
        Ok(true)
    }

    fn define_enum(&mut self, e: &'static EnumDecl) -> Result<(), Thrown> {
        let mut entries = HashMap::new();
        let mut next: i64 = 0;
        for (name, init) in &e.members {
            let v = match init {
                Some(expr) => {
                    let v = self.eval(expr, &self.globals.clone())?;
                    as_i64(&v).ok_or_else(|| {
                        self.throw("TypeError", "enum member value must be an integer")
                    })?
                }
                None => next,
            };
            next = v + 1;
            entries.insert(name.text.clone(), Value::I64(v));
        }
        let ns = Value::Namespace(Rc::new(Namespace {
            name: e.name.text.clone(),
            entries,
        }));
        env_define(&self.globals, &e.name.text, ns);
        Ok(())
    }

    /// Driver entry point for host event callbacks (Stage A DOM events).
    pub fn invoke_callback(&mut self, id: u32) -> Result<(), Thrown> {
        // The host may call in at any stack depth: measure growth from here.
        self.stack_base = stack_here();
        let cb = match self.callbacks.get(id as usize) {
            Some(v) => v.clone(),
            None => return self.type_error(format!("unknown callback #{id}")),
        };
        self.call_value(&cb, Vec::new())?;
        self.drain_microtasks()?;
        // A finished callback is a host boundary: no VM frame is live, so the
        // roots really are the roots and it is safe to collect.
        self.maybe_collect();
        Ok(())
    }

    // ---- statements -----------------------------------------------------------

    fn exec_block(&mut self, stmts: &'static [Stmt], env: &Env) -> SResult {
        let scope = child_env(env);
        for s in stmts {
            match self.exec_stmt(s, &scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    fn exec_stmt(&mut self, s: &'static Stmt, env: &Env) -> SResult {
        self.exec_stmt_l(s, env, None)
    }

    /// `label` is the label attached to this statement, if it is a loop —
    /// `break label`/`continue label` signals matching it are consumed here.
    fn exec_stmt_l(&mut self, s: &'static Stmt, env: &Env, label: Option<&str>) -> SResult {
        match s {
            Stmt::Block(b) => self.exec_block(b, env),
            Stmt::Var(v) => {
                self.exec_var(v, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Empty => Ok(Sig::Normal),
            Stmt::If { cond, then, els } => {
                if self.truthy(cond, env)? {
                    self.exec_stmt(then, env)
                } else if let Some(e) = els {
                    self.exec_stmt(e, env)
                } else {
                    Ok(Sig::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.truthy(cond, env)? {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    if !self.truthy(cond, env)? {
                        break;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                let outer = child_env(env);
                let mut per_iteration: Vec<String> = Vec::new();
                match init {
                    Some(ForInit::Var(v)) => {
                        self.exec_var(v, &outer)?;
                        // `for (let i = 0; …)` gives each iteration its own
                        // `i`, so a closure made in the body captures the value
                        // it saw rather than the one the loop finished with —
                        // the reason `let` exists in a loop head at all.
                        // Only when something can actually capture it:
                        // otherwise this is an ordinary counted loop and stays
                        // one, with no scope allocated per iteration.
                        if v.kind == VarKind::Let && vm::loop_captures(cond, step, body) {
                            for b in &v.bindings {
                                pattern_names_of(&b.target, &mut per_iteration);
                            }
                        }
                    }
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.eval(e, &outer)?;
                        }
                    }
                    None => {}
                }
                let fresh = |from: &Env, names: &[String]| -> Env {
                    let it = child_env(from);
                    for name in names {
                        let v = env_get(from, name).unwrap_or(Value::Null);
                        env_define(&it, name, v);
                    }
                    it
                };
                let mut scope = if per_iteration.is_empty() {
                    outer.clone()
                } else {
                    fresh(&outer, &per_iteration)
                };
                loop {
                    if let Some(c) = cond {
                        if !self.truthy(c, &scope)? {
                            break;
                        }
                    }
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    // The update runs in the *next* iteration's scope: if it
                    // ran in this one, the closure just created in the body
                    // would see the incremented value — exactly the bug that
                    // per-iteration bindings exist to prevent.
                    if !per_iteration.is_empty() {
                        scope = fresh(&scope, &per_iteration);
                    }
                    for e in step {
                        self.eval(e, &scope)?;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::ForOf {
                target, iter, body, ..
            } => {
                let iterable = self.eval(iter, env)?;
                let items: Vec<Value> = match &iterable {
                    Value::Array(a) => a.borrow().clone(),
                    Value::Str(s) => s.iter().map(|c| Value::Char(*c)).collect(),
                    Value::JsRef(h) => {
                        let h = *h;
                        self.web_iterate(h)?
                    }
                    Value::IterV(g) => {
                        let g = g.clone();
                        let mut out = Vec::new();
                        loop {
                            match self.gen_next(g.clone())? {
                                Value::Null => break,
                                v => out.push(v),
                            }
                        }
                        out
                    }
                    _ => {
                        return self.type_error("`for of` needs an array, string, or host iterable")
                    }
                };
                for item in items {
                    let scope = child_env(env);
                    self.bind_pattern(target, item, &scope)?;
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Switch { scrutinee, clauses } => {
                let v = self.eval(scrutinee, env)?;
                let scope = child_env(env);
                let mut matched = clauses.len();
                for (i, c) in clauses.iter().enumerate() {
                    if let Some(t) = &c.test {
                        let tv = self.eval(t, &scope)?;
                        if self.values_equal(&v, &tv)? {
                            matched = i;
                            break;
                        }
                    }
                }
                if matched == clauses.len() {
                    if let Some(i) = clauses.iter().position(|c| c.test.is_none()) {
                        matched = i;
                    }
                }
                'clauses: for c in clauses.iter().skip(matched) {
                    for s in &c.body {
                        match self.exec_stmt(s, &scope)? {
                            Sig::Normal => {}
                            Sig::Break(None) => break 'clauses,
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Break { label, .. } => Ok(Sig::Break(label.as_ref().map(|l| l.text.clone()))),
            Stmt::Continue { label, .. } => {
                Ok(Sig::Continue(label.as_ref().map(|l| l.text.clone())))
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                Ok(Sig::Return(v))
            }
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                Err(Thrown(v))
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                let result = self.exec_block(block, env);
                let result = match result {
                    Err(thrown) => {
                        let mut handled = None;
                        for c in catches {
                            if self.catch_matches(&c.ty, &thrown.0) {
                                let scope = child_env(env);
                                env_define(&scope, &c.name.text, thrown.0.clone());
                                handled = Some(self.exec_block_in(&c.block, &scope));
                                break;
                            }
                        }
                        handled.unwrap_or(Err(thrown))
                    }
                    ok => ok,
                };
                if let Some(f) = finally {
                    match self.exec_block(f, env)? {
                        Sig::Normal => {}
                        other => return Ok(other), // finally overrides
                    }
                }
                result
            }
            Stmt::Labeled { label: l, body } => {
                // The loop consumes matching break/continue signals itself.
                self.exec_stmt_l(body, env, Some(&l.text))
            }
        }
    }

    fn exec_block_in(&mut self, stmts: &'static [Stmt], scope: &Env) -> SResult {
        for s in stmts {
            match self.exec_stmt(s, scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    fn catch_matches(&self, ty: &Type, thrown: &Value) -> bool {
        let want = match ty {
            Type::Named { name, .. } => name.as_str(),
            _ => return false,
        };
        if want == "Error" {
            return true;
        }
        if let Value::Instance(i) = thrown {
            let mut cls = Some(i.borrow().class.clone());
            while let Some(c) = cls {
                if c.name == want {
                    return true;
                }
                cls = c.parent.clone();
            }
        }
        false
    }

    fn exec_var(&mut self, v: &'static VarStmt, env: &Env) -> Result<(), Thrown> {
        for b in &v.bindings {
            let value = match &b.init {
                Some(e) => self.eval(e, env)?,
                None => Value::Null,
            };
            self.bind_pattern(&b.target, value, env)?;
        }
        Ok(())
    }

    fn bind_pattern(&mut self, p: &'static Pattern, value: Value, env: &Env) -> Result<(), Thrown> {
        match p {
            Pattern::Name(n) => {
                env_define(env, &n.text, value);
                Ok(())
            }
            Pattern::Array { elems, rest } => {
                let items: Vec<Value> = match &value {
                    Value::Array(a) => a.borrow().clone(),
                    Value::Str(s) => s.iter().map(|c| Value::Char(*c)).collect(),
                    _ => return self.type_error("cannot destructure a non-array"),
                };
                for (i, e) in elems.iter().enumerate() {
                    let mut v = items.get(i).cloned().unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &e.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    self.bind_pattern(&e.target, v, env)?;
                }
                if let Some(r) = rest {
                    let tail: Vec<Value> = items.iter().skip(elems.len()).cloned().collect();
                    self.bind_pattern(r, new_array(tail), env)?;
                }
                Ok(())
            }
            Pattern::Record(fields) => {
                for f in fields {
                    let mut v = self
                        .get_member(&value, &f.name.text)?
                        .unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &f.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    match &f.target {
                        Some(t) => self.bind_pattern(t, v, env)?,
                        None => env_define(env, &f.name.text, v),
                    }
                }
                Ok(())
            }
        }
    }

    // ---- calls --------------------------------------------------------------------

    fn call_closure(&mut self, c: &Closure, args: Vec<Value>) -> VResult {
        // Both tiers recurse on the Rust stack for a Mersey call, so unbounded
        // Mersey recursion would overflow it — and a stack overflow is a
        // process abort, not an exception a program can handle. In a renderer
        // that is a crash on hostile input (§5.2), so the depth is a budget the
        // engine enforces: past it, an ordinary catchable error.
        if self.depth >= MAX_CALL_DEPTH || self.stack_base.abs_diff(stack_here()) > STACK_BUDGET {
            return Err(self.throw("RangeError", "maximum call depth exceeded"));
        }
        self.depth += 1;
        let out = self.call_closure_inner(c, args);
        self.depth -= 1;
        out
    }

    fn call_closure_inner(&mut self, c: &Closure, args: Vec<Value>) -> VResult {
        let scope = child_env(&c.env);
        self.bind_params(c.data.params, args, &scope)?;
        if let Some(this) = &c.this {
            env_define(&scope, "this", this.clone());
        }
        // A generator (its body contains `yield`) returns an iterator: the
        // body doesn't run until the first `next()`. Like async functions,
        // generators must run on the VM — only it can suspend.
        if !c.data.is_async {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            if let Some(chunk) = compiled {
                if vm::chunk_yields(&chunk) {
                    let coro = Coro {
                        gen: None,
                        chunk,
                        pc: 0,
                        stack: Vec::new(),
                        scopes: vec![scope],
                        handlers: Vec::new(),
                        cls: c.cls.clone(),
                        result: PromiseState::pending(),
                    };
                    let g = Rc::new(GcCell::new(GenState {
                        coro: Some(coro),
                        done: false,
                        is_async: false,
                        pending: None,
                    }));
                    gc::track_gen(&g);
                    return Ok(Value::IterV(g));
                }
            }
        }
        // Async functions always run on the bytecode VM: `await` suspends by
        // capturing VM state, which the AST walker cannot do. (Both tiers
        // therefore agree on async semantics by construction.)
        if c.data.is_async {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            let Some(chunk) = compiled else {
                return self.type_error(
                    "this async function uses a construct the compiler cannot suspend",
                );
            };
            // An `async` function that yields is an async generator: one
            // coroutine that both awaits and yields. Its `next()` hands back a
            // promise, which settles when the body reaches the next `yield`.
            if vm::chunk_yields(&chunk) {
                let coro = Coro {
                    gen: None,
                    chunk,
                    pc: 0,
                    stack: Vec::new(),
                    scopes: vec![scope],
                    handlers: Vec::new(),
                    cls: c.cls.clone(),
                    result: PromiseState::pending(),
                };
                let g = Rc::new(GcCell::new(GenState {
                    coro: Some(coro),
                    done: false,
                    is_async: true,
                    pending: None,
                }));
                gc::track_gen(&g);
                return Ok(Value::IterV(g));
            }
            return self.start_coro(c, chunk, scope);
        }
        if self.use_vm {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            if let Some(chunk) = compiled {
                // Tier 1: hot, simple-int kernels run native (Phase 4).
                if let Some(hook) = self.jit {
                    if c.this.is_none() && c.cls.is_none() {
                        if let Some(v) = self.try_jit(hook, &chunk, c.data.params, &scope)? {
                            return Ok(v);
                        }
                    }
                }
                self.push_frame(&c.data.name, &chunk.module);
                let out = {
                    let frame = Frame::enter(self, c);
                    vm::run_chunk(frame.i, &chunk, scope)
                };
                self.pop_frame();
                return out;
            }
        }
        match &c.data.body {
            FnBody::Expr(e) => {
                let frame = Frame::enter(self, c);
                frame.i.eval(e, &scope)
            }
            FnBody::Block(stmts) => {
                let frame = Frame::enter(self, c);
                match frame.i.exec_block_in(stmts, &scope)? {
                    Sig::Return(v) => Ok(v),
                    _ => Ok(Value::Null),
                }
            }
        }
    }

    /// Attempt a Tier 1 native call: count the call site, compile once
    /// hot, and dispatch when every argument is an int32. The arguments are
    /// re-read from the freshly bound scope so default-value semantics
    /// stayed with `bind_params`.
    fn try_jit(
        &mut self,
        hook: JitHook,
        chunk: &Rc<vm::Chunk>,
        params: &'static [Param],
        scope: &Env,
    ) -> Result<Option<Value>, Thrown> {
        let key = Rc::as_ptr(chunk) as usize;
        let count = self.call_counts.entry(key).or_insert(0);
        *count += 1;
        if *count < JIT_THRESHOLD {
            return Ok(None);
        }
        let names: Option<Vec<String>> = params
            .iter()
            .map(|p| match (&p.target, p.rest, &p.default) {
                (Pattern::Name(n), false, None) => Some(n.text.clone()),
                _ => None,
            })
            .collect();
        let Some(names) = names else { return Ok(None) };
        let compiled = self
            .jit_cache
            .entry(key)
            .or_insert_with(|| hook(chunk, &names))
            .clone();
        let Some(f) = compiled else { return Ok(None) };
        // Entry guard: every argument must match the kernel's numeric world.
        let mut args = Vec::with_capacity(names.len());
        for n in &names {
            match env_get(scope, n) {
                Some(Value::I32(v)) => args.push(JitArg::I32(v)),
                Some(Value::F64(v)) => args.push(JitArg::F64(v)),
                _ => return Ok(None), // guard failed: interpret instead
            }
        }
        Ok(match f(&args) {
            JitResult::I32(v) => Some(Value::I32(v)),
            JitResult::F64(v) => Some(Value::F64(v)),
            JitResult::Null => Some(Value::Null),
            // The kernel hit a trapping condition: re-run interpreted so the
            // error is raised properly (with its position and stack).
            JitResult::Bail => None,
        })
    }

    fn bind_params(
        &mut self,
        params: &'static [Param],
        mut args: Vec<Value>,
        scope: &Env,
    ) -> Result<(), Thrown> {
        let mut rest_param: Option<&'static Param> = None;
        let positional: Vec<&'static Param> = params
            .iter()
            .filter(|p| {
                if p.rest {
                    rest_param = Some(p);
                    false
                } else {
                    true
                }
            })
            .collect();
        let n = positional.len().min(args.len());
        let rest_args: Vec<Value> = args.split_off(n);
        for (i, p) in positional.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or(Value::Null);
            if matches!(v, Value::Null) {
                if let Some(d) = &p.default {
                    v = self.eval(d, scope)?;
                }
            }
            self.bind_pattern(&p.target, v, scope)?;
        }
        if let Some(r) = rest_param {
            self.bind_pattern(&r.target, new_array(rest_args), scope)?;
        }
        Ok(())
    }

    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> VResult {
        match callee {
            Value::Closure(c) => self.call_closure(c, args),
            Value::PromiseExec(p) => {
                let p = p.clone();
                let mut it = args.into_iter();
                let (resolve, reject) = (it.next(), it.next());
                self.promise_then(&p, resolve, reject);
                Ok(Value::Null)
            }
            Value::AllSlot(slot, is_reject) => {
                let (slot, is_reject) = (*slot as usize, *is_reject);
                let v = args.into_iter().next().unwrap_or(Value::Null);
                let (results, remaining, out, idx) = {
                    let c = &self.all_cells[slot];
                    (c.results.clone(), c.remaining.clone(), c.out.clone(), c.idx)
                };
                if is_reject {
                    self.settle(&out, v, true); // first rejection wins
                } else {
                    results.borrow_mut()[idx] = v;
                    let left = {
                        let mut r = remaining.borrow_mut();
                        *r -= 1;
                        *r
                    };
                    if left == 0 {
                        let all = new_array(results.borrow().clone());
                        self.settle(&out, all, false);
                    }
                }
                Ok(Value::Null)
            }
            // Settling callbacks handed to host promises.
            Value::Resolver(p, rejected) => {
                let (p, rejected) = (p.clone(), *rejected);
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, rejected);
                Ok(Value::Null)
            }
            Value::Native(name) => self.call_native(name, None, args),
            // A handle to a JS function (e.g. imported `fetch`): call it.
            Value::JsRef(h) => {
                let h = *h;
                self.web_call(h, "", args)
            }
            _ => self.type_error("value is not callable"),
        }
    }

    fn call_native(&mut self, name: &str, recv: Option<&Value>, args: Vec<Value>) -> VResult {
        match name {
            "console.log" => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.host.print(&line);
                Ok(Value::Null)
            }
            "dom.getElementById" => {
                let id = self.want_string(args.first())?;
                Ok(Value::Dom(Rc::new(id)))
            }
            "dom.createElement" => {
                let tag = self.want_string(args.first())?;
                let id = self.host.dom_create(&tag);
                Ok(Value::Dom(Rc::new(id)))
            }
            "dom.appendChild" => {
                let Some(Value::Dom(parent)) = recv else {
                    return self.type_error("appendChild needs an element");
                };
                let Some(Value::Dom(child)) = args.first() else {
                    return self.type_error("appendChild takes an element");
                };
                let (p, c) = (parent.to_string(), child.to_string());
                self.host.dom_append(&p, &c);
                Ok(Value::Null)
            }
            "dom.remove" => {
                let Some(Value::Dom(id)) = recv else {
                    return self.type_error("remove needs an element");
                };
                let id = id.to_string();
                self.host.dom_remove(&id);
                Ok(Value::Null)
            }
            "dom.addEventListener" => {
                let Some(Value::Dom(id)) = recv else {
                    return self.type_error("addEventListener needs an element");
                };
                let event = self.want_string(args.first())?;
                let cb = args.get(1).cloned().unwrap_or(Value::Null);
                let cb_id = self.callbacks.len() as u32;
                self.callbacks.push(cb);
                // Any event: the engine does not have a list of which ones
                // exist. The host owns the event loop, so it is the host that
                // knows — and in a browser that is the DOM itself.
                self.host.dom_add_listener(id, &event, cb_id);
                Ok(Value::Null)
            }
            "math.abs" => Ok(match args.first() {
                Some(Value::I32(n)) => Value::I32(n.wrapping_abs()),
                Some(Value::I64(n)) => Value::I64(n.wrapping_abs()),
                Some(Value::F32(f)) => Value::F32(f.abs()),
                Some(Value::F64(f)) => Value::F64(f.abs()),
                v => Value::F64(v.and_then(as_num).unwrap_or(f64::NAN).abs()),
            }),
            "math.min" | "math.max" => {
                let mut best: Option<Value> = None;
                for a in args {
                    best = Some(match best {
                        None => a,
                        Some(b) => {
                            let take_a =
                                match self.numeric_binop(BinOp::Lt, a.clone(), b.clone())? {
                                    Value::Bool(lt) => lt == (name == "math.min"),
                                    _ => false,
                                };
                            if take_a {
                                a
                            } else {
                                b
                            }
                        }
                    });
                }
                Ok(best.unwrap_or(Value::Null))
            }
            "math.floor" | "math.ceil" | "math.sqrt" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                Ok(Value::F64(match name {
                    "math.floor" => x.floor(),
                    "math.ceil" => x.ceil(),
                    _ => x.sqrt(),
                }))
            }
            "math.pow" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let y = args.get(1).and_then(as_num).unwrap_or(f64::NAN);
                Ok(Value::F64(x.powf(y)))
            }
            "format.pad" => {
                let text = to_display(args.first().unwrap_or(&Value::Null));
                let width = args.get(1).and_then(as_i64).unwrap_or(0).max(0) as usize;
                let n = text.chars().count();
                let padded = if n >= width {
                    text
                } else {
                    format!("{}{text}", " ".repeat(width - n))
                };
                Ok(Value::Str(Rc::new(padded.chars().collect())))
            }
            "format.fixed" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let d = args.get(1).and_then(as_i64).unwrap_or(0).clamp(0, 17) as usize;
                Ok(Value::Str(Rc::new(format!("{x:.d$}").chars().collect())))
            }
            "fs.readText" => {
                let path = self.want_string(args.first())?;
                match self.host.read_text(&path) {
                    Ok(text) => Ok(Value::Str(Rc::new(text.chars().collect()))),
                    Err(msg) => Err(self.throw("Error", msg)),
                }
            }
            "env.get" => {
                let key = self.want_string(args.first())?;
                Ok(match self.host.env_var(&key) {
                    Some(v) => Value::Str(Rc::new(v.chars().collect())),
                    None => Value::Null,
                })
            }
            "caps.has" => {
                let cap = self.want_string(args.first())?;
                Ok(Value::Bool(self.host.caps().contains(&cap)))
            }
            "caps.list" => {
                let caps: Vec<Value> = self
                    .host
                    .caps()
                    .into_iter()
                    .map(|c| Value::Str(Rc::new(c.chars().collect())))
                    .collect();
                Ok(new_array(caps))
            }
            "promise.resolve" => {
                let p = PromiseState::pending();
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, false);
                Ok(Value::PromiseV(p))
            }
            "promise.reject" => {
                let p = PromiseState::pending();
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, true);
                Ok(Value::PromiseV(p))
            }
            "promise.all" => {
                let items: Vec<Value> = match args.first() {
                    Some(Value::Array(a)) => a.borrow().clone(),
                    _ => return self.type_error("Promise.all needs an array"),
                };
                let out = PromiseState::pending();
                let results = Rc::new(GcCell::new(vec![Value::Null; items.len()]));
                let remaining = Rc::new(RefCell::new(items.len()));
                if items.is_empty() {
                    let all = new_array(Vec::new());
                    self.settle(&out, all, false);
                    return Ok(Value::PromiseV(out));
                }
                for (idx, item) in items.into_iter().enumerate() {
                    let p = self.as_promise(item)?;
                    let cell = AllCell {
                        results: results.clone(),
                        remaining: remaining.clone(),
                        out: out.clone(),
                        idx,
                    };
                    self.all_cells.push(cell);
                    let slot = (self.all_cells.len() - 1) as u32;
                    let on_ok = Value::AllSlot(slot, false);
                    let on_err = Value::AllSlot(slot, true);
                    self.promise_then(&p, Some(on_ok), Some(on_err));
                }
                Ok(Value::PromiseV(out))
            }
            // A collection cannot run mid-expression (live VM frames are not
            // roots), so this *requests* one for the next safe point.
            "gc.collect" => {
                self.gc_pending = true;
                Ok(Value::Null)
            }
            "gc.stats" => {
                // Reports only — sweeping here would be unsound (live VM
                // frames are not roots mid-expression).
                let stats = gc::stats_only();
                Ok(new_record(vec![(
                    "live".to_string(),
                    Value::I32(stats.tracked as i32),
                )]))
            }
            "regex.compile" => {
                let pattern = self.want_string(args.first())?;
                let flags = match args.get(1) {
                    Some(Value::Str(s)) => s.iter().collect::<String>(),
                    _ => String::new(),
                };
                match regex::Regex::new(&pattern, &flags) {
                    Ok(re) => Ok(Value::RegexV(Rc::new(re))),
                    Err(msg) => Err(self.throw("Error", format!("bad regex: {msg}"))),
                }
            }
            "parse.int32" | "parse.int64" | "parse.float64" | "parse.bigint" | "parse.bigdec" => {
                let text = self.want_string(args.first())?;
                let t = text.trim();
                // Parsing returns null on failure — no exceptions for input
                // you expected to be dubious (§1.3: no sentinel values).
                Ok(match name {
                    "parse.int32" => {
                        let radix = args.get(1).and_then(as_i64).unwrap_or(10).clamp(2, 36) as u32;
                        match i32::from_str_radix(t, radix) {
                            Ok(v) => Value::I32(v),
                            Err(_) => Value::Null,
                        }
                    }
                    "parse.int64" => {
                        let radix = args.get(1).and_then(as_i64).unwrap_or(10).clamp(2, 36) as u32;
                        match i64::from_str_radix(t, radix) {
                            Ok(v) => Value::I64(v),
                            Err(_) => Value::Null,
                        }
                    }
                    "parse.float64" => match t.parse::<f64>() {
                        Ok(v) => Value::F64(v),
                        Err(_) => Value::Null,
                    },
                    "parse.bigint" => {
                        let (neg, digits) = match t.strip_prefix('-') {
                            Some(rest) => (true, rest),
                            None => (false, t.strip_prefix('+').unwrap_or(t)),
                        };
                        match BigInt::parse(digits, 10) {
                            Some(b) if !digits.is_empty() => {
                                Value::BigIntV(Rc::new(if neg { b.negate() } else { b }))
                            }
                            _ => Value::Null,
                        }
                    }
                    _ => match BigDec::parse(t) {
                        Some(d) => Value::BigDecV(Rc::new(d)),
                        None => Value::Null,
                    },
                })
            }
            // Civil calendar from a millisecond timestamp (Howard Hinnant's
            // days-from-civil algorithm, proleptic Gregorian).
            "time.parts" => {
                let ms = args.first().and_then(as_num).unwrap_or(0.0);
                let secs = (ms / 1000.0).floor() as i64;
                let ms_part = (ms - (secs as f64) * 1000.0).round() as i64;
                let days = secs.div_euclid(86_400);
                let tod = secs.rem_euclid(86_400);
                let (y, m, d) = civil_from_days(days);
                let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
                Ok(new_record(vec![
                    ("year".into(), Value::I32(y as i32)),
                    ("month".into(), Value::I32(m as i32)),
                    ("day".into(), Value::I32(d as i32)),
                    ("hour".into(), Value::I32((tod / 3600) as i32)),
                    ("minute".into(), Value::I32(((tod % 3600) / 60) as i32)),
                    ("second".into(), Value::I32((tod % 60) as i32)),
                    ("millis".into(), Value::I32(ms_part as i32)),
                    ("weekday".into(), Value::I32(weekday as i32)),
                ]))
            }
            "time.fromParts" => {
                let Some(Value::Record(r)) = args.first() else {
                    return self.type_error("time.fromParts needs a record");
                };
                let f = r.borrow();
                let get =
                    |k: &str, dflt: i64| rec_get(&f, k).and_then(|v| as_i64(&v)).unwrap_or(dflt);
                let days = days_from_civil(get("year", 1970), get("month", 1), get("day", 1));
                let secs = days * 86_400
                    + get("hour", 0) * 3600
                    + get("minute", 0) * 60
                    + get("second", 0);
                Ok(Value::F64((secs as f64) * 1000.0 + get("millis", 0) as f64))
            }
            "time.now" | "time.monotonic" => Ok(Value::F64(self.host.time_ms(name == "time.now"))),
            "bytes.alloc" => {
                let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
                Ok(Value::Bytes(Rc::new(RefCell::new(vec![0u8; n]))))
            }
            "bytes.fromHost" => {
                let Some(Value::JsRef(h)) = args.first() else {
                    return self.type_error("bytes.fromHost needs a host typed array");
                };
                let h = *h;
                match self.host.web_bytes_read(h) {
                    Some(v) => Ok(Value::Bytes(Rc::new(RefCell::new(v)))),
                    None => self.type_error("value is not a typed array / ArrayBuffer"),
                }
            }
            "bytes.toHost" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("bytes.toHost needs a Bytes buffer");
                };
                let data = b.borrow().clone();
                let handle = self.host.web_bytes_write(&data);
                if handle < 0 {
                    self.type_error("host cannot accept byte buffers")
                } else {
                    Ok(Value::JsRef(handle))
                }
            }
            "bytes.fill" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("bytes.fill needs a Bytes buffer");
                };
                let v = (args.get(1).and_then(as_i64).unwrap_or(0) & 0xFF) as u8;
                b.borrow_mut().iter_mut().for_each(|x| *x = v);
                Ok(Value::Null)
            }
            "web.attach" => {
                let (Some(inst_v), Some(host)) = (args.first(), args.get(1)) else {
                    return self.type_error("attach(instance, hostObject) needs both");
                };
                let Value::Instance(inst) = inst_v else {
                    return self.type_error("attach: the first value must be a class instance");
                };
                let h = match host {
                    Value::JsRef(h) => *h,
                    Value::Instance(i) => match i.borrow().host {
                        Some(h) => h,
                        None => {
                            return self.type_error("attach: the second value is not a host object")
                        }
                    },
                    _ => return self.type_error("attach: the second value is not a host object"),
                };
                inst.borrow_mut().host = Some(h);
                Ok(inst_v.clone())
            }
            "web.release" => {
                if let Some(v) = args.first() {
                    self.web_release_value(v);
                }
                Ok(Value::Null)
            }
            "caps.drop" => {
                let cap = self.want_string(args.first())?;
                self.host.drop_cap(&cap);
                Ok(Value::Null)
            }
            _ => self.type_error(format!("unknown native `{name}`")),
        }
    }

    fn want_string(&self, v: Option<&Value>) -> Result<String, Thrown> {
        match v {
            Some(Value::Str(s)) => Ok(s.iter().collect()),
            _ => Err(self.throw("TypeError", "expected a string argument")),
        }
    }

    fn instantiate(&mut self, cls: &Rc<ClassDef>, args: Vec<Value>) -> VResult {
        if cls.is_builtin_error {
            let mut slots = vec![Value::Null; cls.fields.len()];
            slots[0] = args.into_iter().next().unwrap_or(Value::Null);
            if slots.len() > 1 {
                slots[1] = Value::Str(Rc::new(self.stack_trace().chars().collect()));
            }
            let inst = Rc::new(GcCell::new(Instance {
                class: cls.clone(),
                slots,
                host: None,
            }));
            gc::track_instance(&inst);
            return Ok(Value::Instance(inst));
        }
        let inst = Rc::new(GcCell::new(Instance {
            class: cls.clone(),
            slots: vec![Value::Null; cls.fields.len()],
            host: None,
        }));
        gc::track_instance(&inst);
        let this = Value::Instance(inst.clone());
        let env = cls.env.clone().unwrap_or_else(|| self.globals.clone());

        // Field initializers, base-first, with `this` in scope.
        for (slot, (_, init)) in cls.fields.clone().iter().enumerate() {
            let v = match init {
                Some(e) => {
                    let scope = child_env(&env);
                    env_define(&scope, "this", this.clone());
                    self.eval(e, &scope)?
                }
                None => Value::Null,
            };
            inst.borrow_mut().slots[slot] = v;
        }

        // Nearest constructor up the chain; implicit pass-through otherwise.
        let mut search = Some(cls.clone());
        while let Some(c) = search {
            if let Some(ctor) = &c.ctor {
                let closure = Closure {
                    data: ctor.clone(),
                    env,
                    this: Some(this.clone()),
                    cls: Some(c.clone()),
                };
                self.call_closure(&closure, args)?;
                break;
            }
            search = c.parent.clone();
        }
        Ok(this)
    }

    // ---- member access -----------------------------------------------------------

    fn get_member(&mut self, obj: &Value, name: &str) -> Result<Option<Value>, Thrown> {
        match obj {
            Value::Str(s) => Ok(match name {
                "length" => Some(Value::I32(s.len() as i32)),
                _ => None,
            }),
            Value::Array(a) => Ok(match name {
                "length" => Some(Value::I32(a.borrow().len() as i32)),
                _ => None,
            }),
            Value::JsRef(h) => {
                let h = *h;
                self.web_get(h, name).map(Some)
            }
            Value::Bytes(b) => Ok(match name {
                "length" => Some(Value::I32(b.borrow().len() as i32)),
                _ => None,
            }),
            Value::MapV(m) => Ok(match name {
                "size" => Some(Value::I32(m.borrow().len() as i32)),
                _ => None,
            }),
            Value::SetV(m) => Ok(match name {
                "size" => Some(Value::I32(m.borrow().len() as i32)),
                _ => None,
            }),
            Value::Record(r) => Ok(rec_get(&r.borrow(), name)),
            Value::Namespace(ns) => Ok(ns.entries.get(name).cloned()),
            Value::Dom(id) => match name {
                "textContent" => Ok(Some(Value::Str(Rc::new(
                    self.host
                        .dom_get_text(id)
                        .unwrap_or_default()
                        .chars()
                        .collect(),
                )))),
                "value" => {
                    let id = id.to_string();
                    Ok(Some(Value::Str(Rc::new(
                        self.host.dom_get_value(&id).chars().collect(),
                    ))))
                }
                _ => Ok(None),
            },
            Value::Class(c) => {
                if let Some(v) = c.statics.borrow().get(name) {
                    return Ok(Some(v.clone()));
                }
                if let Some(m) = c.static_methods.get(name) {
                    let env = c.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m.clone(),
                        env,
                        this: None,
                        cls: Some(c.clone()),
                    }))));
                }
                Ok(None)
            }
            Value::Instance(inst) => {
                {
                    // Constant-offset load: sealed shapes make the slot
                    // known from the class alone (§4.1).
                    let i = inst.borrow();
                    if let Some(slot) = i.class.field_slots.get(name) {
                        return Ok(i.slots.get(*slot as usize).cloned());
                    }
                }
                let class = inst.borrow().class.clone();
                if let Some((getter, defining)) = find_in_chain(&class, |c| {
                    c.getters.get(name).map(|g| (g.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: getter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    return self.call_closure(&closure, Vec::new()).map(Some);
                }
                if let Some((m, defining)) = find_in_chain(&class, |c| {
                    c.methods.get(name).map(|m| (m.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    }))));
                }
                // Host-backed class: read it off the host object.
                let host = inst.borrow().host;
                if let Some(h) = host {
                    return self.web_get(h, name).map(Some);
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn set_member(&mut self, obj: &Value, name: &str, value: Value) -> Result<(), Thrown> {
        match obj {
            Value::JsRef(h) => {
                let h = *h;
                self.web_set(h, name, value)
            }
            Value::Record(r) => {
                rec_set(&mut r.borrow_mut(), name, value);
                Ok(())
            }
            Value::Dom(id) => match name {
                "textContent" => {
                    let id = id.to_string();
                    self.host.dom_set_text(&id, &to_display(&value));
                    Ok(())
                }
                "value" => {
                    let id = id.to_string();
                    self.host.dom_set_value(&id, &to_display(&value));
                    Ok(())
                }
                _ => self.type_error(format!("DOM elements have no settable `{name}`")),
            },
            Value::Class(c) => {
                if c.statics.borrow().contains_key(name) {
                    c.statics.borrow_mut().insert(name.to_string(), value);
                    Ok(())
                } else {
                    self.type_error(format!("no static field `{name}` on class `{}`", c.name))
                }
            }
            Value::Instance(inst) => {
                let class = inst.borrow().class.clone();
                if let Some((setter, defining)) = find_in_chain(&class, |c| {
                    c.setters.get(name).map(|s| (s.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: setter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    self.call_closure(&closure, vec![value])?;
                    return Ok(());
                }
                // Sealed shapes (§4.1): the field must be declared, and its
                // slot is a constant.
                let slot = class.field_slots.get(name).copied();
                if let Some(slot) = slot {
                    inst.borrow_mut().slots[slot as usize] = value;
                    return Ok(());
                }
                // Host-backed class: write through to the host object
                // (`this.textContent = …` on a class extending HTMLElement).
                let host = inst.borrow().host;
                if let Some(h) = host {
                    return self.web_set(h, name, value);
                }
                self.type_error(format!(
                    "class `{}` has no field `{name}` (shapes are sealed)",
                    class.name
                ))
            }
            _ => self.type_error("cannot assign to a member of this value"),
        }
    }

    // ---- expressions -----------------------------------------------------------------

    fn truthy(&mut self, e: &'static Expr, env: &Env) -> Result<bool, Thrown> {
        let v = self.eval(e, env)?;
        self.value_truthy(&v)
    }

    /// Conditions accept `bool` or numeric (`!= 0`), per §3.3 — nothing else.
    fn value_truthy(&self, v: &Value) -> Result<bool, Thrown> {
        Ok(match v {
            Value::Bool(b) => *b,
            Value::I32(n) => *n != 0,
            Value::I64(n) => *n != 0,
            Value::U32(n) => *n != 0,
            Value::U64(n) => *n != 0,
            Value::F32(f) => *f != 0.0,
            Value::F64(f) => *f != 0.0,
            _ => {
                return Err(self.throw(
                    "TypeError",
                    "condition must be bool or numeric (§3.3); write the comparison",
                ))
            }
        })
    }

    fn eval(&mut self, e: &'static Expr, env: &Env) -> VResult {
        match e {
            Expr::Ident(n) => env_get(env, &n.text)
                .ok_or_else(|| self.throw("TypeError", format!("`{}` is not defined", n.text))),
            Expr::This(_) => env_get(env, "this")
                .ok_or_else(|| self.throw("TypeError", "`this` is not available here")),
            Expr::Lit { kind, text, .. } => self.eval_literal(*kind, text),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TplPart::Text(t) => out.push_str(&unescape(t)),
                        TplPart::Expr(e) => {
                            let v = self.eval(e, env)?;
                            out.push_str(&to_display(&v));
                        }
                    }
                }
                Ok(Value::Str(Rc::new(out.chars().collect())))
            }
            Expr::Array(elems) => {
                let mut items = Vec::new();
                for el in elems {
                    let v = self.eval(&el.expr, env)?;
                    if el.spread {
                        match v {
                            Value::Array(a) => items.extend(a.borrow().iter().cloned()),
                            _ => return self.type_error("can only spread arrays"),
                        }
                    } else {
                        items.push(v);
                    }
                }
                Ok(new_array(items))
            }
            Expr::Record(fields) => {
                let mut out: Vec<(String, Value)> = Vec::new();
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            let v = match value {
                                Some(e) => self.eval(e, env)?,
                                None => env_get(env, &name.text).ok_or_else(|| {
                                    self.throw(
                                        "TypeError",
                                        format!("`{}` is not defined", name.text),
                                    )
                                })?,
                            };
                            rec_set(&mut out, &name.text, v);
                        }
                        RecordField::Spread(e) => {
                            let v = self.eval(e, env)?;
                            match v {
                                Value::Record(r) => {
                                    for (k, val) in r.borrow().iter() {
                                        rec_set(&mut out, k, val.clone());
                                    }
                                }
                                _ => return self.type_error("can only spread records"),
                            }
                        }
                    }
                }
                Ok(new_record(out))
            }
            Expr::Paren(e) => self.eval(e, env),
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
                // Arrows capture `this` lexically.
                let this = env_get(env, "this");
                Ok(Value::Closure(Rc::new(Closure {
                    data,
                    env: env.clone(),
                    this,
                    cls: None,
                })))
            }
            Expr::Unary { op, expr, .. } => {
                if *op == UnaryOp::Await {
                    return self.type_error("`await` is not in the MVP");
                }
                // `-2147483648` is one literal, not a negation of one.
                if let (
                    UnaryOp::Neg,
                    Expr::Lit {
                        kind: LitKind::Int,
                        text,
                        ..
                    },
                ) = (op, &**expr)
                {
                    return negated_int_literal(text)
                        .map_err(|(class, msg)| self.throw(class, msg));
                }
                let v = self.eval(expr, env)?;
                self.eval_unary(*op, v)
            }
            Expr::Update { prefix, inc, expr } => {
                let old = self.eval(expr, env)?;
                let one = Value::I32(1);
                let new = self.numeric_binop(
                    if *inc { BinOp::Add } else { BinOp::Sub },
                    old.clone(),
                    one,
                )?;
                self.assign_to(expr, new.clone(), env)?;
                Ok(if *prefix { new } else { old })
            }
            Expr::Binary { op, l, r } => match op {
                BinOp::And => {
                    let lv = self.eval(l, env)?;
                    if !self.value_truthy(&lv)? {
                        return Ok(Value::Bool(false));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Or => {
                    let lv = self.eval(l, env)?;
                    if self.value_truthy(&lv)? {
                        return Ok(Value::Bool(true));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Coalesce => {
                    let lv = self.eval(l, env)?;
                    if matches!(lv, Value::Null) {
                        self.eval(r, env)
                    } else {
                        Ok(lv)
                    }
                }
                BinOp::Instanceof => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    self.instance_of(&lv, &rv)
                }
                BinOp::Eq | BinOp::Ne => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    let eq = self.values_equal(&lv, &rv)?;
                    Ok(Value::Bool(if *op == BinOp::Eq { eq } else { !eq }))
                }
                _ => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    self.numeric_binop(*op, lv, rv)
                }
            },
            Expr::Assign { op, target, value } => {
                let rhs = self.eval(value, env)?;
                let new = if *op == "=" {
                    rhs
                } else {
                    let old = self.eval(target, env)?;
                    match *op {
                        "&&=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep {
                                rhs
                            } else {
                                old
                            }
                        }
                        "||=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep {
                                old
                            } else {
                                rhs
                            }
                        }
                        "??=" => {
                            if matches!(old, Value::Null) {
                                rhs
                            } else {
                                old
                            }
                        }
                        _ => {
                            let bin = match *op {
                                "+=" => BinOp::Add,
                                "-=" => BinOp::Sub,
                                "*=" => BinOp::Mul,
                                "/=" => BinOp::Div,
                                "%=" => BinOp::Rem,
                                "**=" => BinOp::Pow,
                                "<<=" => BinOp::Shl,
                                ">>=" => BinOp::Shr,
                                "&=" => BinOp::BitAnd,
                                "|=" => BinOp::BitOr,
                                "^=" => BinOp::BitXor,
                                _ => return self.type_error("unknown assignment operator"),
                            };
                            self.numeric_binop(bin, old, rhs)?
                        }
                    }
                };
                self.assign_to(target, new.clone(), env)?;
                Ok(new)
            }
            Expr::Cond { cond, then, els } => {
                let c = self.eval(cond, env)?;
                if self.value_truthy(&c)? {
                    self.eval(then, env)
                } else {
                    self.eval(els, env)
                }
            }
            Expr::Cast { expr, wrapping, ty } => {
                let v = self.eval(expr, env)?;
                self.eval_cast(v, *wrapping, ty)
            }
            Expr::Call {
                callee,
                args,
                optional,
                ..
            } => {
                // Receiver/callee evaluates before the arguments; a null
                // receiver under `?.` skips argument evaluation entirely.
                if let Expr::Member {
                    obj,
                    name,
                    optional: mopt,
                } = callee.as_ref()
                {
                    let recv = self.eval(obj, env)?;
                    if (*mopt || *optional) && matches!(recv, Value::Null) {
                        return Ok(Value::Null);
                    }
                    let argv = self.eval_args(args, env)?;
                    return self.call_member(&recv, name, argv);
                }
                if let Expr::SuperMember { name, .. } = callee.as_ref() {
                    let argv = self.eval_args(args, env)?;
                    return self.call_super_method(name, argv, env);
                }
                let f = self.eval(callee, env)?;
                if *optional && matches!(f, Value::Null) {
                    return Ok(Value::Null);
                }
                let argv = self.eval_args(args, env)?;
                self.call_value(&f, argv)
            }
            Expr::New { ty, args } => {
                let Type::Named { name, .. } = ty else {
                    return self.type_error("`new` needs a class");
                };
                let argv = self.eval_args(args, env)?;
                self.new_named(name, argv, env)
            }
            Expr::Member {
                obj,
                name,
                optional,
            } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                match self.get_member(&o, name)? {
                    Some(v) => Ok(v),
                    None => self.type_error(format!("no member `{name}` on {}", kind_of(&o))),
                }
            }
            Expr::Index {
                obj,
                index,
                optional,
            } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                let i = self.eval(index, env)?;
                self.index_get(&o, &i)
            }
            Expr::SuperMember { name, .. } => {
                // Non-call super member: resolve to a bound closure.
                self.super_lookup(name, env)
            }
            Expr::SuperCall { args, .. } => {
                let argv = self.eval_args(args, env)?;
                self.super_call(argv, env)
            }
            Expr::ImportCall(inner) => {
                let spec = match &**inner {
                    Expr::Lit {
                        kind: LitKind::Str,
                        text,
                        ..
                    } => mersey_front::ast::string_value(text),
                    // The checker rejects a non-literal specifier (§4.5).
                    _ => return self.type_error("`import(…)` needs a literal specifier"),
                };
                self.dynamic_import(&spec)
            }
            // Generators run on the VM (only it can suspend); reaching here
            // means the AST tier was asked to run one.
            Expr::Yield { .. } => self.type_error("`yield` requires the bytecode VM"),
        }
    }

    fn eval_args(&mut self, args: &'static [ArrayElem], env: &Env) -> Result<Vec<Value>, Thrown> {
        let mut out = Vec::new();
        for a in args {
            let v = self.eval(&a.expr, env)?;
            if a.spread {
                match v {
                    Value::Array(arr) => out.extend(arr.borrow().iter().cloned()),
                    _ => return self.type_error("can only spread arrays"),
                }
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn call_member(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> VResult {
        match recv {
            Value::IterV(g) => match name {
                "next" => self.gen_next(g.clone()),
                "toArray" => {
                    let mut out = Vec::new();
                    loop {
                        match self.gen_next(g.clone())? {
                            Value::Null => break,
                            v => out.push(v),
                        }
                    }
                    Ok(new_array(out))
                }
                _ => self.type_error(format!("no method `{name}` on Iter")),
            },
            Value::PromiseV(p) => {
                let p = p.clone();
                let mut it = args.into_iter();
                match name {
                    "then" => {
                        let ok = it.next();
                        let err = it.next();
                        Ok(self.promise_then(&p, ok, err))
                    }
                    "catch" => {
                        let err = it.next();
                        Ok(self.promise_then(&p, None, err))
                    }
                    _ => self.type_error(format!("no method `{name}` on Promise")),
                }
            }
            Value::JsRef(h) => {
                let h = *h;
                self.web_call(h, name, args)
            }
            Value::Array(a) => {
                let a = a.clone();
                let items = || a.borrow().clone();
                match name {
                    "push" => {
                        for v in args {
                            a.borrow_mut().push(v);
                        }
                        Ok(Value::Null)
                    }
                    "pop" => Ok(a.borrow_mut().pop().unwrap_or(Value::Null)),
                    "clear" => {
                        a.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "keys" => {
                        let n = a.borrow().len();
                        Ok(new_array((0..n).map(|i| Value::I32(i as i32)).collect()))
                    }
                    "join" => {
                        let sep = match args.first() {
                            Some(Value::Str(s)) => s.iter().collect::<String>(),
                            _ => String::new(),
                        };
                        let parts: Vec<String> = a.borrow().iter().map(to_display).collect();
                        Ok(Value::Str(Rc::new(parts.join(&sep).chars().collect())))
                    }
                    "map" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut out = Vec::new();
                        for item in items() {
                            out.push(self.call_value(&f, vec![item])?);
                        }
                        Ok(new_array(out))
                    }
                    "filter" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut out = Vec::new();
                        for item in items() {
                            let keep = self.call_value(&f, vec![item.clone()])?;
                            if self.value_truthy(&keep)? {
                                out.push(item);
                            }
                        }
                        Ok(new_array(out))
                    }
                    "reduce" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Null);
                        for item in items() {
                            acc = self.call_value(&f, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    "forEach" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        for item in items() {
                            self.call_value(&f, vec![item])?;
                        }
                        Ok(Value::Null)
                    }
                    "find" | "findIndex" | "some" | "every" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let want_all = name == "every";
                        for (i, item) in items().into_iter().enumerate() {
                            let hit = self.call_value(&f, vec![item.clone()])?;
                            let hit = self.value_truthy(&hit)?;
                            if hit && !want_all {
                                return Ok(match name {
                                    "find" => item,
                                    "findIndex" => Value::I32(i as i32),
                                    _ => Value::Bool(true),
                                });
                            }
                            if !hit && want_all {
                                return Ok(Value::Bool(false));
                            }
                        }
                        Ok(match name {
                            "find" => Value::Null,
                            "findIndex" => Value::I32(-1),
                            "some" => Value::Bool(false),
                            _ => Value::Bool(true),
                        })
                    }
                    "indexOf" | "contains" => {
                        let want = args.first().cloned().unwrap_or(Value::Null);
                        for (i, item) in items().into_iter().enumerate() {
                            if self.values_equal(&item, &want)? {
                                return Ok(if name == "contains" {
                                    Value::Bool(true)
                                } else {
                                    Value::I32(i as i32)
                                });
                            }
                        }
                        Ok(if name == "contains" {
                            Value::Bool(false)
                        } else {
                            Value::I32(-1)
                        })
                    }
                    "slice" => {
                        let src = items();
                        let len = src.len() as i64;
                        let norm = |v: i64| v.clamp(0, len) as usize;
                        let start = norm(args.first().and_then(as_i64).unwrap_or(0));
                        let end = norm(args.get(1).and_then(as_i64).unwrap_or(len));
                        let out = if start < end {
                            src[start..end].to_vec()
                        } else {
                            Vec::new()
                        };
                        Ok(new_array(out))
                    }
                    "concat" => {
                        let mut out = items();
                        if let Some(Value::Array(b)) = args.first() {
                            out.extend(b.borrow().iter().cloned());
                        }
                        Ok(new_array(out))
                    }
                    "reverseInPlace" => {
                        a.borrow_mut().reverse();
                        Ok(Value::Null)
                    }
                    "toReversed" => {
                        let mut out = items();
                        out.reverse();
                        Ok(new_array(out))
                    }
                    // Comparator-driven sort: merge sort so the comparator is
                    // called a predictable number of times and the sort is
                    // stable (a comparator can throw, so it must be fallible).
                    "sortInPlace" | "toSorted" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let sorted = self.merge_sort(items(), &f)?;
                        if name == "sortInPlace" {
                            *a.borrow_mut() = sorted;
                            Ok(Value::Null)
                        } else {
                            Ok(new_array(sorted))
                        }
                    }
                    "toString" => Ok(Value::Str(Rc::new(to_display(recv).chars().collect()))),
                    _ => self.type_error(format!("arrays have no method `{name}`")),
                }
            }
            Value::MapV(m) => {
                let m = m.clone();
                match name {
                    "set" => {
                        let (k, v) = (
                            args.first().cloned().unwrap_or(Value::Null),
                            args.get(1).cloned().unwrap_or(Value::Null),
                        );
                        let idx = self.map_find(&m, &k)?;
                        match idx {
                            Some(i) => m.borrow_mut()[i].1 = v,
                            None => m.borrow_mut().push((k, v)),
                        }
                        Ok(Value::Null)
                    }
                    "get" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        Ok(match self.map_find(&m, &k)? {
                            Some(i) => m.borrow()[i].1.clone(),
                            None => Value::Null,
                        })
                    }
                    "has" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        Ok(Value::Bool(self.map_find(&m, &k)?.is_some()))
                    }
                    "remove" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        Ok(match self.map_find(&m, &k)? {
                            Some(i) => {
                                m.borrow_mut().remove(i);
                                Value::Bool(true)
                            }
                            None => Value::Bool(false),
                        })
                    }
                    "keys" => Ok(new_array(
                        m.borrow().iter().map(|(k, _)| k.clone()).collect(),
                    )),
                    "values" => Ok(new_array(
                        m.borrow().iter().map(|(_, v)| v.clone()).collect(),
                    )),
                    "entries" => {
                        let pairs: Vec<Value> = m
                            .borrow()
                            .iter()
                            .map(|(k, v)| new_array(vec![k.clone(), v.clone()]))
                            .collect();
                        Ok(new_array(pairs))
                    }
                    "clear" => {
                        m.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "toString" => Ok(Value::Str(Rc::new(to_display(recv).chars().collect()))),
                    _ => self.type_error(format!("no method `{name}` on Map")),
                }
            }
            Value::SetV(m) => {
                let m = m.clone();
                match name {
                    "add" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        if self.set_find(&m, &v)?.is_none() {
                            m.borrow_mut().push(v);
                        }
                        Ok(Value::Null)
                    }
                    "has" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        Ok(Value::Bool(self.set_find(&m, &v)?.is_some()))
                    }
                    "remove" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        Ok(match self.set_find(&m, &v)? {
                            Some(i) => {
                                m.borrow_mut().remove(i);
                                Value::Bool(true)
                            }
                            None => Value::Bool(false),
                        })
                    }
                    "values" => Ok(new_array(m.borrow().clone())),
                    "clear" => {
                        m.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "toString" => Ok(Value::Str(Rc::new(to_display(recv).chars().collect()))),
                    _ => self.type_error(format!("no method `{name}` on Set")),
                }
            }
            Value::RegexV(re) => {
                let re = re.clone();
                let Some(Value::Str(subject)) = args.first() else {
                    return self.type_error(format!("regex `{name}` needs a string"));
                };
                let chars: Vec<char> = subject.as_ref().clone();
                let slice =
                    |a: usize, b: usize| -> Value { Value::Str(Rc::new(chars[a..b].to_vec())) };
                let make_match = |m: &regex::Match| -> Value {
                    let groups: Vec<Value> = m
                        .groups
                        .iter()
                        .map(|g| match g {
                            Some((a, b)) => slice(*a, *b),
                            None => Value::Null,
                        })
                        .collect();
                    new_record(vec![
                        ("text".into(), slice(m.start, m.end)),
                        ("start".into(), Value::I32(m.start as i32)),
                        ("end".into(), Value::I32(m.end as i32)),
                        ("groups".into(), new_array(groups)),
                    ])
                };
                match name {
                    "test" => Ok(Value::Bool(re.is_match(&chars))),
                    "find" => Ok(match re.find_at(&chars, 0) {
                        Some(m) => make_match(&m),
                        None => Value::Null,
                    }),
                    "findAll" => {
                        let mut out = Vec::new();
                        let mut at = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) => {
                                    at = if m.end > m.start { m.end } else { m.start + 1 };
                                    out.push(make_match(&m));
                                }
                                None => break,
                            }
                        }
                        Ok(new_array(out))
                    }
                    "replaceAll" => {
                        let with = match args.get(1) {
                            Some(Value::Str(w)) => w.iter().collect::<String>(),
                            Some(other) => to_display(other),
                            None => String::new(),
                        };
                        let mut out: Vec<char> = Vec::new();
                        let mut at = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) => {
                                    out.extend_from_slice(&chars[at..m.start]);
                                    out.extend(with.chars());
                                    at = if m.end > m.start {
                                        m.end
                                    } else {
                                        if m.start < chars.len() {
                                            out.push(chars[m.start]);
                                        }
                                        m.start + 1
                                    };
                                }
                                None => break,
                            }
                        }
                        if at < chars.len() {
                            out.extend_from_slice(&chars[at..]);
                        }
                        Ok(Value::Str(Rc::new(out)))
                    }
                    "split" => {
                        let mut parts = Vec::new();
                        let mut at = 0;
                        let mut last = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) if m.end > m.start => {
                                    parts.push(slice(last, m.start));
                                    last = m.end;
                                    at = m.end;
                                }
                                _ => break,
                            }
                        }
                        parts.push(slice(last, chars.len()));
                        Ok(new_array(parts))
                    }
                    _ => self.type_error(format!("no method `{name}` on Regex")),
                }
            }
            Value::Str(s) => {
                let text: String = s.iter().collect();
                let arg0 = || -> String {
                    match args.first() {
                        Some(Value::Str(a)) => a.iter().collect(),
                        Some(other) => to_display(other),
                        None => String::new(),
                    }
                };
                match name {
                    "toString" => Ok(Value::Str(s.clone())),
                    "indexOf" => {
                        let needle = arg0();
                        // Code-point index, not byte index (§3.4).
                        Ok(Value::I32(match text.find(&needle) {
                            Some(b) => text[..b].chars().count() as i32,
                            None => -1,
                        }))
                    }
                    "contains" => Ok(Value::Bool(text.contains(&arg0()))),
                    "startsWith" => Ok(Value::Bool(text.starts_with(&arg0()))),
                    "endsWith" => Ok(Value::Bool(text.ends_with(&arg0()))),
                    "toUpperCase" => Ok(Value::Str(Rc::new(text.to_uppercase().chars().collect()))),
                    "toLowerCase" => Ok(Value::Str(Rc::new(text.to_lowercase().chars().collect()))),
                    "trim" => Ok(Value::Str(Rc::new(text.trim().chars().collect()))),
                    "slice" => {
                        let len = s.len() as i64;
                        let norm = |v: i64| v.clamp(0, len) as usize;
                        let start = norm(args.first().and_then(as_i64).unwrap_or(0));
                        let end = norm(args.get(1).and_then(as_i64).unwrap_or(len));
                        let out: Vec<char> = if start < end {
                            s[start..end].to_vec()
                        } else {
                            Vec::new()
                        };
                        Ok(Value::Str(Rc::new(out)))
                    }
                    "replace" | "replaceAll" => {
                        let needle = arg0();
                        let with = match args.get(1) {
                            Some(Value::Str(a)) => a.iter().collect::<String>(),
                            Some(other) => to_display(other),
                            None => String::new(),
                        };
                        let out = if name == "replace" {
                            text.replacen(&needle as &str, &with, 1)
                        } else {
                            text.replace(&needle as &str, &with)
                        };
                        Ok(Value::Str(Rc::new(out.chars().collect())))
                    }
                    "repeat" => {
                        let n = args
                            .first()
                            .and_then(as_i64)
                            .unwrap_or(0)
                            .clamp(0, 1_000_000);
                        Ok(Value::Str(Rc::new(
                            text.repeat(n as usize).chars().collect(),
                        )))
                    }
                    "padStart" | "padEnd" => {
                        let width = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
                        let pad = match args.get(1) {
                            Some(Value::Str(a)) if !a.is_empty() => a.iter().collect::<String>(),
                            _ => " ".to_string(),
                        };
                        let mut out: Vec<char> = s.as_ref().clone();
                        let pad_chars: Vec<char> = pad.chars().collect();
                        let mut k = 0;
                        while out.len() < width {
                            let c = pad_chars[k % pad_chars.len()];
                            if name == "padStart" {
                                out.insert(k, c);
                            } else {
                                out.push(c);
                            }
                            k += 1;
                        }
                        Ok(Value::Str(Rc::new(out)))
                    }
                    "split" => {
                        let sep = arg0();
                        let parts: Vec<Value> = if sep.is_empty() {
                            text.chars().map(|c| Value::Str(Rc::new(vec![c]))).collect()
                        } else {
                            text.split(&sep as &str)
                                .map(|p| Value::Str(Rc::new(p.chars().collect())))
                                .collect()
                        };
                        Ok(new_array(parts))
                    }
                    _ => self.type_error(format!("no method `{name}` on string")),
                }
            }
            // bigdec.divide(other, { scale: 2, mode: "HALF_EVEN" }) — §3.7
            Value::BigDecV(a) if name == "divide" => {
                let Some(Value::BigDecV(b)) = args.first() else {
                    return self.type_error("divide(divisor, context) needs a bigdec divisor");
                };
                let ctx = args.get(1);
                let (scale, mode) = match ctx {
                    Some(Value::Record(fields)) => {
                        let f = fields.borrow();
                        let scale = rec_get(&f, "scale")
                            .and_then(|v| as_i64(&v))
                            .unwrap_or(0)
                            .clamp(0, 1000) as u32;
                        let mode_name = match rec_get(&f, "mode") {
                            Some(Value::Str(s)) => s.iter().collect::<String>(),
                            _ => "HALF_EVEN".to_string(),
                        };
                        let Some(mode) = RoundingMode::parse(&mode_name) else {
                            return self.type_error(format!("unknown rounding mode `{mode_name}`"));
                        };
                        (scale, mode)
                    }
                    _ => return self.type_error("divide needs a rounding context"),
                };
                match a.divide(b, scale, mode) {
                    Some(q) => Ok(Value::BigDecV(Rc::new(q))),
                    None => Err(self.throw("RangeError", "division by zero")),
                }
            }
            Value::Char(_)
            | Value::I32(_)
            | Value::I64(_)
            | Value::U32(_)
            | Value::U64(_)
            | Value::F32(_)
            | Value::F64(_)
            | Value::Bool(_)
                if name == "toString" =>
            {
                Ok(Value::Str(Rc::new(to_display(recv).chars().collect())))
            }
            Value::Dom(_) if name == "addEventListener" => {
                self.call_native("dom.addEventListener", Some(recv), args)
            }
            Value::Dom(_) if name == "appendChild" => {
                self.call_native("dom.appendChild", Some(recv), args)
            }
            Value::Dom(_) if name == "remove" => self.call_native("dom.remove", Some(recv), args),
            Value::Namespace(ns) => match ns.entries.get(name) {
                Some(Value::Native(n)) => {
                    let n = *n;
                    self.call_native(n, Some(recv), args)
                }
                Some(v @ Value::Closure(_)) => {
                    let v = v.clone();
                    self.call_value(&v, args)
                }
                _ => self.type_error(format!("no member `{name}` on `{}`", ns.name)),
            },
            // Host-backed instances: a method not declared in Mersey is the
            // host's (`this.addEventListener(…)`).
            Value::Instance(inst) => {
                let declared_in_mersey = {
                    let i = inst.borrow();
                    i.class.field_slots.contains_key(name)
                        || find_in_chain(&i.class, |c| c.methods.get(name).map(|_| ())).is_some()
                        || find_in_chain(&i.class, |c| c.getters.get(name).map(|_| ())).is_some()
                };
                let host = inst.borrow().host;
                if !declared_in_mersey {
                    if let Some(h) = host {
                        return self.web_call(h, name, args);
                    }
                }
                let member = self.get_member(recv, name)?;
                match member {
                    Some(f) => self.call_value(&f, args),
                    None => self.type_error(format!("no method `{name}` on {}", kind_of(recv))),
                }
            }
            _ => {
                let member = self.get_member(recv, name)?;
                match member {
                    Some(f) => self.call_value(&f, args),
                    None => self.type_error(format!("no method `{name}` on {}", kind_of(recv))),
                }
            }
        }
    }

    fn index_get(&mut self, o: &Value, i: &Value) -> VResult {
        if let Value::Bytes(b) = o {
            let ix = as_i64(i).unwrap_or(-1);
            let bytes = b.borrow();
            return if ix < 0 || ix as usize >= bytes.len() {
                Err(self.throw(
                    "RangeError",
                    format!("index {ix} out of bounds (length {})", bytes.len()),
                ))
            } else {
                Ok(Value::I32(bytes[ix as usize] as i32))
            };
        }
        // Host objects: `list[0]`, `obj["key"]` → bridge property read.
        if let Value::JsRef(h) = o {
            let (h, prop) = (*h, to_display(i));
            return self.web_get(h, &prop);
        }
        match (o, as_i64(i)) {
            (Value::Array(a), Some(ix)) => {
                let a = a.borrow();
                if ix < 0 || ix as usize >= a.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", a.len()),
                    ))
                } else {
                    Ok(a[ix as usize].clone())
                }
            }
            (Value::Str(s), Some(ix)) => {
                if ix < 0 || ix as usize >= s.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", s.len()),
                    ))
                } else {
                    Ok(Value::Char(s[ix as usize]))
                }
            }
            _ => self.type_error("only arrays and strings are indexable"),
        }
    }

    fn index_set(&mut self, o: &Value, i: &Value, value: Value) -> Result<(), Thrown> {
        if let Value::Bytes(b) = o {
            let ix = as_i64(i).unwrap_or(-1);
            let v = as_i64(&value).unwrap_or(0);
            let mut bytes = b.borrow_mut();
            return if ix < 0 || ix as usize >= bytes.len() {
                Err(self.throw(
                    "RangeError",
                    format!("index {ix} out of bounds (length {})", bytes.len()),
                ))
            } else {
                // Wrapping, like a Uint8 store (§3.6).
                bytes[ix as usize] = (v & 0xFF) as u8;
                Ok(())
            };
        }
        if let Value::JsRef(h) = o {
            let (h, prop) = (*h, to_display(i));
            return self.web_set(h, &prop, value);
        }
        match (o, as_i64(i)) {
            (Value::Array(a), Some(ix)) => {
                let mut a = a.borrow_mut();
                if ix < 0 || ix as usize >= a.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", a.len()),
                    ))
                } else {
                    a[ix as usize] = value;
                    Ok(())
                }
            }
            _ => self.type_error("only array elements can be assigned by index"),
        }
    }

    fn instance_of(&mut self, l: &Value, r: &Value) -> VResult {
        match r {
            // `x instanceof SomeMerseyClass`
            Value::Class(want) => {
                let mut ok = false;
                if let Value::Instance(i) = l {
                    let mut cls = Some(i.borrow().class.clone());
                    while let Some(c) = cls {
                        if Rc::ptr_eq(&c, want) {
                            ok = true;
                            break;
                        }
                        cls = c.parent.clone();
                    }
                }
                Ok(Value::Bool(ok))
            }
            // `x instanceof HTMLElement` — a host interface object. The left
            // side may be a host object, or a host-backed Mersey instance.
            Value::JsRef(ctor) => {
                let target = match l {
                    Value::JsRef(h) => Some(*h),
                    Value::Instance(i) => i.borrow().host,
                    _ => None,
                };
                match target {
                    Some(h) => Ok(Value::Bool(self.host.web_instanceof(h, *ctor))),
                    None => Ok(Value::Bool(false)),
                }
            }
            _ => self.type_error("right side of instanceof must be a class or host interface"),
        }
    }

    // ---- promises, microtasks, coroutines -------------------------------

    /// Settle a promise and queue its reactions/waiters as microtasks.
    fn settle(&mut self, p: &Rc<GcCell<PromiseState>>, value: Value, rejected: bool) {
        {
            let st = p.borrow();
            if st.status != PromiseStatus::Pending {
                return; // already settled: first settle wins
            }
        }
        // Resolving with a promise adopts its state.
        if !rejected {
            if let Value::PromiseV(inner) = &value {
                let inner = inner.clone();
                let outer = p.clone();
                let st = inner.borrow().status.clone();
                match st {
                    PromiseStatus::Pending => {
                        inner.borrow_mut().reactions.push((None, None, outer));
                        return;
                    }
                    PromiseStatus::Fulfilled => {
                        let v = inner.borrow().value.clone();
                        return self.settle(p, v, false);
                    }
                    PromiseStatus::Rejected => {
                        let v = inner.borrow().value.clone();
                        return self.settle(p, v, true);
                    }
                }
            }
        }
        let (waiters, reactions) = {
            let mut st = p.borrow_mut();
            st.status = if rejected {
                PromiseStatus::Rejected
            } else {
                PromiseStatus::Fulfilled
            };
            st.value = value.clone();
            (
                std::mem::take(&mut st.waiters),
                std::mem::take(&mut st.reactions),
            )
        };
        for coro in waiters {
            self.tasks
                .push_back(Task::Resume(coro, value.clone(), rejected));
        }
        for (on_ok, on_err, downstream) in reactions {
            self.tasks.push_back(Task::React(
                on_ok,
                on_err,
                downstream,
                value.clone(),
                rejected,
            ));
        }
    }

    /// Register `then`-style reactions, returning the chained promise.
    fn promise_then(
        &mut self,
        p: &Rc<GcCell<PromiseState>>,
        on_ok: Option<Value>,
        on_err: Option<Value>,
    ) -> Value {
        let downstream = PromiseState::pending();
        let st = p.borrow().status.clone();
        match st {
            PromiseStatus::Pending => {
                p.borrow_mut()
                    .reactions
                    .push((on_ok, on_err, downstream.clone()));
            }
            PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                let rejected = st == PromiseStatus::Rejected;
                let value = p.borrow().value.clone();
                self.tasks.push_back(Task::React(
                    on_ok,
                    on_err,
                    downstream.clone(),
                    value,
                    rejected,
                ));
            }
        }
        Value::PromiseV(downstream)
    }

    /// Convert any awaitable into a Mersey promise: a host (JS) promise is
    /// adopted by handing it Resolver callbacks through the bridge.
    fn as_promise(&mut self, v: Value) -> Result<Rc<GcCell<PromiseState>>, Thrown> {
        match v {
            Value::PromiseV(p) => Ok(p),
            Value::JsRef(h) => {
                let p = PromiseState::pending();
                let ok = Value::Resolver(p.clone(), false);
                let err = Value::Resolver(p.clone(), true);
                // A JS thenable settles our promise through the bridge.
                self.web_call(h, "then", vec![ok, err])?;
                Ok(p)
            }
            other => {
                // Awaiting a plain value: already-resolved promise.
                let p = PromiseState::pending();
                self.settle(&p, other, false);
                Ok(p)
            }
        }
    }

    /// The engine's live set at a safe point (no VM frames on the Rust
    /// stack), for the cycle collector.
    fn gc_roots(&self) -> gc::Roots {
        let mut roots = gc::Roots {
            envs: vec![self.root.clone(), self.globals.clone()],
            classes: self.class_stack.clone(),
            ..Default::default()
        };
        for cls in self.error_classes.values() {
            roots.classes.push(cls.clone());
        }
        for exports in self.modules.values() {
            roots.values.extend(exports.values().cloned());
        }
        roots.values.extend(self.callbacks.iter().cloned());
        for task in &self.tasks {
            match task {
                Task::Resume(coro, v, _) => {
                    roots.values.push(v.clone());
                    roots.coros.push(coro.result.clone());
                    for e in &coro.scopes {
                        roots.envs.push(e.clone());
                    }
                    roots.values.extend(coro.stack.iter().cloned());
                }
                Task::React(ok, err, down, v, _) => {
                    roots.values.extend(ok.iter().cloned());
                    roots.values.extend(err.iter().cloned());
                    roots.values.push(v.clone());
                    roots.coros.push(down.clone());
                }
            }
        }
        for cell in &self.all_cells {
            roots.coros.push(cell.out.clone());
            roots.values.extend(cell.results.borrow().iter().cloned());
        }
        // A graph paused on a top-level `await` is live: its module's scope
        // holds everything the module has built so far.
        if let Some(p) = &self.pending_graph {
            roots.coros.push(p.promise.clone());
            roots.envs.push(p.env.clone());
        }
        roots
    }

    /// Collect cycles. Only safe at a host boundary — see gc.rs.
    pub fn collect_garbage(&mut self) -> gc::GcStats {
        let roots = self.gc_roots();
        self.gc_pending = false;
        // An explicit request means "reclaim what you can", including
        // old-generation cycles, so it gets the full trace.
        gc::collect_major(&roots)
    }

    /// The routine collection: generational, so the pause is bounded by how
    /// much has been allocated since last time rather than by the heap.
    fn collect_young(&mut self) -> gc::GcStats {
        let roots = self.gc_roots();
        gc::collect(&roots)
    }

    /// Collect if requested or if enough has been allocated. Called only at
    /// host boundaries.
    fn maybe_collect(&mut self) {
        if self.gc_pending {
            // Explicit `gc.collect()`: full trace.
            self.collect_garbage();
        } else if gc::should_collect() {
            self.collect_young();
        }
    }

    /// Run microtasks to completion. Called before control returns to the
    /// host, so a turn always leaves the queue empty.
    pub fn drain_microtasks(&mut self) -> Result<(), Thrown> {
        loop {
            self.drain_tasks()?;
            // A module that suspended on a top-level `await` may now be able to
            // finish — and everything importing it is still waiting. Running
            // those modules can queue more microtasks, so this loops.
            if !self.graph_can_resume() {
                return Ok(());
            }
            self.resume_graph()?;
        }
    }

    /// The microtask queue itself.
    fn drain_tasks(&mut self) -> Result<(), Thrown> {
        // Bounded to catch runaway promise loops in hostile input.
        const MAX: u32 = 1_000_000;
        let mut n = 0;
        while let Some(task) = self.tasks.pop_front() {
            n += 1;
            if n > MAX {
                return self.type_error("microtask queue did not settle");
            }
            match task {
                Task::Resume(coro, value, rejected) => {
                    self.resume(coro, value, rejected)?;
                }
                Task::React(on_ok, on_err, downstream, value, rejected) => {
                    let handler = if rejected { on_err } else { on_ok };
                    match handler {
                        Some(f) => match self.call_value(&f, vec![value]) {
                            Ok(out) => self.settle(&downstream, out, false),
                            Err(t) => self.settle(&downstream, t.0, true),
                        },
                        // No handler: pass the settlement through.
                        None => self.settle(&downstream, value, rejected),
                    }
                }
            }
        }
        Ok(())
    }

    /// Start an async function: run its chunk until it completes or awaits.
    fn start_coro(&mut self, c: &Closure, chunk: Rc<vm::Chunk>, scope: Env) -> VResult {
        let result = PromiseState::pending();
        let coro = Coro {
            gen: None,
            chunk,
            pc: 0,
            stack: Vec::new(),
            scopes: vec![scope],
            handlers: Vec::new(),
            cls: c.cls.clone(),
            result: result.clone(),
        };
        self.drive(coro, None)?;
        Ok(Value::PromiseV(result))
    }

    fn resume(&mut self, coro: Coro, value: Value, rejected: bool) -> Result<(), Thrown> {
        self.drive(coro, Some((value, rejected)))
    }

    /// Drive a coroutine until it finishes or suspends on an await.
    fn drive(&mut self, mut coro: Coro, resumed: Option<(Value, bool)>) -> Result<(), Thrown> {
        // A coroutine belonging to an async generator settles that generator's
        // pending `next()` when it yields — not its own result promise.
        if let Some(g) = coro.gen.clone() {
            return self.drive_gen(g, coro, resumed);
        }
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, resumed);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        match outcome {
            Ok(vm::Flow::Done(v)) => {
                let result = coro.result.clone();
                self.settle(&result, v, false);
                Ok(())
            }
            Ok(vm::Flow::Yield(_)) => {
                let result = coro.result.clone();
                let t = self.throw("TypeError", "`yield` inside an async function");
                self.settle(&result, t.0, true);
                Ok(())
            }
            Ok(vm::Flow::Await(awaited)) => {
                let p = self.as_promise(awaited)?;
                let status = p.borrow().status.clone();
                match status {
                    PromiseStatus::Pending => {
                        p.borrow_mut().waiters.push(coro);
                    }
                    PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                        let v = p.borrow().value.clone();
                        let rejected = status == PromiseStatus::Rejected;
                        self.tasks.push_back(Task::Resume(coro, v, rejected));
                    }
                }
                Ok(())
            }
            Err(t) => {
                // An uncaught throw rejects the async function's promise.
                let result = coro.result.clone();
                self.settle(&result, t.0, true);
                Ok(())
            }
        }
    }

    // ---- universal web bridge -------------------------------------------

    /// Mersey value → tagged JSON. Objects become `{"__ref__":n}`,
    /// closures are registered and become `{"__cb__":id}`.
    fn to_web(&mut self, v: &Value) -> Json {
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::I32(n) => Json::Num(*n as f64),
            Value::I64(n) => Json::Num(*n as f64),
            Value::U32(n) => Json::Num(*n as f64),
            Value::U64(n) => Json::Num(*n as f64),
            Value::F32(f) => Json::Num(*f as f64),
            Value::F64(f) => Json::Num(*f),
            Value::Char(c) => Json::Str(c.to_string()),
            Value::Str(s) => Json::Str(s.iter().collect()),
            Value::BigIntV(b) => Json::Str(b.to_decimal()),
            Value::BigDecV(d) => Json::Str(d.to_decimal()),
            Value::JsRef(h) => Json::Obj(vec![("__ref__".into(), Json::Num(*h as f64))]),
            // A host-backed instance IS its host object on the wire.
            Value::Instance(i) if i.borrow().host.is_some() => {
                let h = i.borrow().host.expect("checked");
                Json::Obj(vec![("__ref__".into(), Json::Num(h as f64))])
            }
            Value::Dom(id) => Json::Obj(vec![("__dom__".into(), Json::Str(id.to_string()))]),
            Value::Array(a) => {
                let items: Vec<Value> = a.borrow().clone();
                Json::Arr(items.iter().map(|x| self.to_web(x)).collect())
            }
            Value::Record(r) => {
                // Field order is preserved across the bridge.
                let entries: Vec<(String, Value)> = r.borrow().clone();
                Json::Obj(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k, self.to_web(&v)))
                        .collect(),
                )
            }
            Value::Closure(_)
            | Value::Native(_)
            | Value::Resolver(..)
            | Value::AllSlot(..)
            | Value::PromiseExec(..) => {
                let id = self.alloc_callback(v.clone());
                Json::Obj(vec![("__cb__".into(), Json::Num(id as f64))])
            }
            // A Mersey promise crosses as a real host promise: construct one
            // whose executor forwards settlement from ours.
            Value::PromiseV(p) => {
                let exec = Value::PromiseExec(p.clone());
                match self.web_new("Promise", vec![exec]) {
                    Ok(Value::JsRef(h)) => Json::Obj(vec![("__ref__".into(), Json::Num(h as f64))]),
                    _ => Json::Null,
                }
            }
            other => Json::Str(to_display(other)),
        }
    }

    /// Tagged JSON → Mersey value.
    fn from_web(&self, j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() <= i32::MAX as f64 {
                    Value::I32(*n as i32)
                } else {
                    Value::F64(*n)
                }
            }
            Json::Str(s) => Value::Str(Rc::new(s.chars().collect())),
            Json::Arr(items) => new_array(items.iter().map(|i| self.from_web(i)).collect()),
            Json::Obj(fields) => {
                if let Some(Json::Num(h)) = j.get("__ref__") {
                    return Value::JsRef(*h as i64);
                }
                let entries: Vec<(String, Value)> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.from_web(v)))
                    .collect();
                new_record(entries)
            }
        }
    }

    /// Decode a bridge reply (`{"ok":…}` / `{"err":"…"}`).
    fn web_reply(&self, reply: &str) -> VResult {
        let Some(j) = webjson::parse(reply) else {
            return Err(self.throw("Error", format!("bad bridge reply: {reply}")));
        };
        if let Some(Json::Str(msg)) = j.get("err") {
            return Err(self.throw("Error", msg.clone()));
        }
        match j.get("ok") {
            Some(v) => Ok(self.from_web(v)),
            None => Ok(Value::Null),
        }
    }

    /// Take a callback slot, reusing a freed one when possible.
    fn alloc_callback(&mut self, v: Value) -> u32 {
        match self.free_callbacks.pop() {
            Some(id) => {
                self.callbacks[id as usize] = v;
                id
            }
            None => {
                self.callbacks.push(v);
                (self.callbacks.len() - 1) as u32
            }
        }
    }

    /// Release a callback the host will never invoke again (a removed
    /// listener, a settled promise reaction).
    pub fn release_callback(&mut self, id: u32) {
        if let Some(slot) = self.callbacks.get_mut(id as usize) {
            if !matches!(slot, Value::Null) {
                *slot = Value::Null;
                self.free_callbacks.push(id);
            }
        }
    }

    fn args_json(&mut self, args: &[Value]) -> String {
        let arr = Json::Arr(args.iter().map(|a| self.to_web(a)).collect());
        let mut s = String::new();
        webjson::write(&mut s, &arr);
        s
    }

    /// Intern a member name once; afterwards only the id crosses the ABI.
    fn intern(&mut self, name: &str) -> Option<u32> {
        if let Some(id) = self.interned.get(name) {
            return if *id == u32::MAX { None } else { Some(*id) };
        }
        let id = self.host.web_intern(name);
        self.interned.insert(name.to_string(), id);
        if id == u32::MAX {
            None
        } else {
            Some(id)
        }
    }

    fn web_get(&mut self, target: i64, prop: &str) -> VResult {
        let reply = match self.intern(prop) {
            Some(id) => self.host.web_get_id(target, id),
            None => self.host.web_get(target, prop),
        };
        self.web_reply(&reply)
    }

    fn web_set(&mut self, target: i64, prop: &str, v: Value) -> Result<(), Thrown> {
        // Fast paths: a scalar value needs no JSON at all.
        if let Some(id) = self.intern(prop) {
            let reply = match &v {
                Value::Str(s) => {
                    let text: String = s.iter().collect();
                    Some(self.host.web_set_str(target, id, &text))
                }
                Value::I32(n) => Some(self.host.web_set_num(target, id, *n as f64)),
                Value::F64(f) => Some(self.host.web_set_num(target, id, *f)),
                Value::I64(n) => Some(self.host.web_set_num(target, id, *n as f64)),
                _ => None,
            };
            if let Some(reply) = reply {
                return self.web_reply(&reply).map(|_| ());
            }
        }
        let j = self.to_web(&v);
        let mut s = String::new();
        webjson::write(&mut s, &j);
        let reply = self.host.web_set(target, prop, &s);
        self.web_reply(&reply).map(|_| ())
    }

    fn web_call(&mut self, target: i64, method: &str, args: Vec<Value>) -> VResult {
        // Fast path: one string argument (getElementById, createElement, …).
        // Not for `method == ""`, which calls the handle itself (`fetch(url)`).
        if args.len() == 1 && !method.is_empty() {
            if let Value::Str(s) = &args[0] {
                if let Some(id) = self.intern(method) {
                    let text: String = s.iter().collect();
                    let reply = self.host.web_call_str(target, id, &text);
                    return self.web_reply(&reply);
                }
            }
        }
        let a = self.args_json(&args);
        let reply = self.host.web_call(target, method, &a);
        self.web_reply(&reply)
    }

    /// Release a host handle explicitly (`release(el)`): long-lived pages
    /// that churn through DOM objects can hand them back. Handles are not
    /// GC-tracked yet — this is the documented escape hatch.
    pub(crate) fn web_release_value(&mut self, v: &Value) {
        if let Value::JsRef(h) = v {
            if *h != 0 {
                self.host.web_release(*h);
            }
        }
    }

    /// Elements of a host iterable, as Mersey values.
    pub(crate) fn web_iterate(&mut self, target: i64) -> Result<Vec<Value>, Thrown> {
        let reply = self.host.web_iterate(target);
        match self.web_reply(&reply)? {
            Value::Array(a) => Ok(a.borrow().clone()),
            other => Err(self.throw(
                "TypeError",
                format!("`{}` is not iterable", kind_of(&other)),
            )),
        }
    }

    fn web_new(&mut self, ctor: &str, args: Vec<Value>) -> VResult {
        let a = self.args_json(&args);
        let reply = self.host.web_new(ctor, &a);
        self.web_reply(&reply)
    }

    /// Fire a callback with host-supplied arguments (event objects etc.).
    pub fn invoke_callback_json(&mut self, id: u32, args_json: &str) -> Result<(), Thrown> {
        // The host may call in at any stack depth: measure growth from here.
        self.stack_base = stack_here();
        let args = match webjson::parse(args_json) {
            Some(Json::Arr(items)) => items.iter().map(|i| self.from_web(i)).collect(),
            _ => Vec::new(),
        };
        let cb = match self.callbacks.get(id as usize) {
            Some(v) => v.clone(),
            None => return self.type_error(format!("unknown callback #{id}")),
        };
        self.call_value(&cb, args)?;
        self.drain_microtasks()?;
        self.maybe_collect();
        Ok(())
    }

    /// Stable merge sort driven by a Mersey comparator (which may throw).
    fn merge_sort(&mut self, items: Vec<Value>, cmp: &Value) -> Result<Vec<Value>, Thrown> {
        if items.len() <= 1 {
            return Ok(items);
        }
        let mid = items.len() / 2;
        let right = self.merge_sort(items[mid..].to_vec(), cmp)?;
        let left = self.merge_sort(items[..mid].to_vec(), cmp)?;
        let mut out = Vec::with_capacity(left.len() + right.len());
        let (mut i, mut j) = (0, 0);
        while i < left.len() && j < right.len() {
            let ord = self.call_value(cmp, vec![left[i].clone(), right[j].clone()])?;
            let ord = as_i64(&ord).unwrap_or(0);
            if ord <= 0 {
                out.push(left[i].clone());
                i += 1;
            } else {
                out.push(right[j].clone());
                j += 1;
            }
        }
        out.extend_from_slice(&left[i..]);
        out.extend_from_slice(&right[j..]);
        Ok(out)
    }

    fn map_find(
        &self,
        m: &Rc<GcCell<Vec<(Value, Value)>>>,
        k: &Value,
    ) -> Result<Option<usize>, Thrown> {
        let items = m.borrow();
        for (i, (key, _)) in items.iter().enumerate() {
            if self.values_equal(key, k)? {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn set_find(&self, m: &Rc<GcCell<Vec<Value>>>, v: &Value) -> Result<Option<usize>, Thrown> {
        let items = m.borrow();
        for (i, item) in items.iter().enumerate() {
            if self.values_equal(item, v)? {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn current_class(&self) -> Result<Rc<ClassDef>, Thrown> {
        self.class_stack
            .last()
            .cloned()
            .ok_or_else(|| self.throw("TypeError", "`super` outside a class"))
    }

    fn super_lookup(&mut self, name: &str, env: &Env) -> VResult {
        let this =
            env_get(env, "this").ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
        let cls = self.current_class()?;
        let parent = cls
            .parent
            .clone()
            .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
        if let Some((m, defining)) = find_in_chain(&parent, |c| {
            c.methods.get(name).map(|m| (m.clone(), c.clone()))
        }) {
            let env2 = defining.env.clone().unwrap_or_else(|| self.globals.clone());
            return Ok(Value::Closure(Rc::new(Closure {
                data: m,
                env: env2,
                this: Some(this),
                cls: Some(defining),
            })));
        }
        self.type_error(format!("no method `{name}` on the base class"))
    }

    fn new_named(&mut self, head: &str, argv: Vec<Value>, env: &Env) -> VResult {
        // `new geo.Point(…)` — resolve through a namespace import.
        if let Some((ns, member)) = head.split_once('.') {
            if let Some(Value::Namespace(entries)) = env_get(env, ns) {
                return match entries.entries.get(member) {
                    Some(Value::Class(cls)) => {
                        let cls = cls.clone();
                        self.instantiate(&cls, argv)
                    }
                    _ => self.type_error(format!("`{head}` is not a class")),
                };
            }
        }
        if head == "Map" && env_get(env, "Map").is_none() {
            return Ok(new_map(Vec::new()));
        }
        if head == "Set" && env_get(env, "Set").is_none() {
            return Ok(new_set(Vec::new()));
        }
        let bare = head.split('.').next().unwrap_or(head);
        match env_get(env, bare) {
            Some(Value::Class(cls)) => self.instantiate(&cls, argv),
            // `new WebSocket(url)`, `new Uint8Array(n)`, …: any host
            // constructor reachable through the bridge.
            _ => self.web_new(bare, argv),
        }
    }

    /// Drain a generator into a vector (used by `for … of` in the VM).
    pub(crate) fn drain_iter(&mut self, v: &Value) -> Result<Vec<Value>, Thrown> {
        let Value::IterV(g) = v else {
            return self.type_error("not an iterator");
        };
        let g = g.clone();
        let mut out = Vec::new();
        loop {
            match self.gen_next(g.clone())? {
                Value::Null => break,
                item => out.push(item),
            }
        }
        Ok(out)
    }

    /// Resume a generator to its next `yield` (or to completion).
    /// `next()` on an async generator: a promise that settles at the next
    /// `yield` (with the value), at the end (with `null`), or with whatever the
    /// body threw.
    fn gen_next_async(&mut self, g: Rc<GcCell<GenState>>) -> VResult {
        let promise = PromiseState::pending();
        if g.borrow().done {
            self.settle(&promise, Value::Null, false);
            return Ok(Value::PromiseV(promise));
        }
        let Some(mut coro) = g.borrow_mut().coro.take() else {
            g.borrow_mut().done = true;
            self.settle(&promise, Value::Null, false);
            return Ok(Value::PromiseV(promise));
        };
        g.borrow_mut().pending = Some(promise.clone());
        coro.gen = Some(g.clone());
        self.drive_gen(g, coro, None)?;
        Ok(Value::PromiseV(promise))
    }

    /// Drive an async generator's coroutine until it yields, finishes, or
    /// suspends on an `await`.
    fn drive_gen(
        &mut self,
        g: Rc<GcCell<GenState>>,
        mut coro: Coro,
        resumed: Option<(Value, bool)>,
    ) -> Result<(), Thrown> {
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, resumed);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        let pending = g.borrow_mut().pending.take();
        match outcome {
            Ok(vm::Flow::Yield(v)) => {
                // Suspended at a `yield`: keep the coroutine for the next call
                // and hand the value to whoever is awaiting `next()`.
                g.borrow_mut().coro = Some(coro);
                if let Some(p) = pending {
                    self.settle(&p, v, false);
                }
                Ok(())
            }
            Ok(vm::Flow::Done(_)) => {
                g.borrow_mut().discard();
                if let Some(p) = pending {
                    self.settle(&p, Value::Null, false); // exhausted
                }
                Ok(())
            }
            Ok(vm::Flow::Await(awaited)) => {
                // The body awaited something. This `next()` has not settled yet:
                // put its promise back, and resume when the awaited thing does.
                g.borrow_mut().pending = pending;
                let p = self.as_promise(awaited)?;
                let status = p.borrow().status.clone();
                match status {
                    PromiseStatus::Pending => {
                        p.borrow_mut().waiters.push(coro);
                    }
                    PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                        let v = p.borrow().value.clone();
                        let rejected = status == PromiseStatus::Rejected;
                        self.tasks.push_back(Task::Resume(coro, v, rejected));
                    }
                }
                Ok(())
            }
            Err(t) => {
                g.borrow_mut().discard();
                if let Some(p) = pending {
                    self.settle(&p, t.0, true);
                }
                Ok(())
            }
        }
    }

    fn gen_next(&mut self, g: Rc<GcCell<GenState>>) -> VResult {
        if g.borrow().is_async {
            return self.gen_next_async(g);
        }
        if g.borrow().done {
            return Ok(Value::Null);
        }
        let Some(mut coro) = g.borrow_mut().coro.take() else {
            g.borrow_mut().done = true;
            return Ok(Value::Null);
        };
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, None);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        match outcome {
            Ok(vm::Flow::Yield(v)) => {
                // Suspended: keep the coroutine for the next call.
                g.borrow_mut().coro = Some(coro);
                Ok(v)
            }
            Ok(vm::Flow::Done(_)) => {
                g.borrow_mut().done = true;
                Ok(Value::Null) // exhausted
            }
            Ok(vm::Flow::Await(_)) => {
                g.borrow_mut().done = true;
                self.type_error("`await` inside a generator is not supported")
            }
            Err(t) => {
                g.borrow_mut().done = true;
                Err(t)
            }
        }
    }

    fn super_call(&mut self, argv: Vec<Value>, env: &Env) -> VResult {
        let this =
            env_get(env, "this").ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
        let cls = self.current_class()?;
        let parent = cls
            .parent
            .clone()
            .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
        let mut search = Some(parent);
        while let Some(c) = search {
            // The builtin errors are constructed by the engine, not by a
            // Mersey constructor — so `super(msg)` in `class X extends Error`
            // would otherwise walk past them and quietly drop the message.
            if c.is_builtin_error {
                if let Value::Instance(inst) = &this {
                    let msg = argv.into_iter().next().unwrap_or(Value::Null);
                    let stack = Value::Str(Rc::new(self.stack_trace().chars().collect()));
                    let mut i = inst.borrow_mut();
                    if let Some(slot) = i.class.slot_of("message") {
                        i.slots[slot as usize] = msg;
                    }
                    if let Some(slot) = i.class.slot_of("stack") {
                        i.slots[slot as usize] = stack;
                    }
                }
                return Ok(Value::Null);
            }
            if let Some(ctor) = &c.ctor {
                let env2 = c.env.clone().unwrap_or_else(|| self.globals.clone());
                let closure = Closure {
                    data: ctor.clone(),
                    env: env2,
                    this: Some(this.clone()),
                    cls: Some(c.clone()),
                };
                return self.call_closure(&closure, argv);
            }
            search = c.parent.clone();
        }
        Ok(Value::Null) // no ctor anywhere up the chain: nothing to do
    }

    fn call_super_method(&mut self, name: &str, args: Vec<Value>, env: &Env) -> VResult {
        let f = self.super_lookup(name, env)?;
        self.call_value(&f, args)
    }

    fn assign_to(&mut self, target: &'static Expr, value: Value, env: &Env) -> Result<(), Thrown> {
        match target {
            Expr::Ident(n) => {
                if env_set(env, &n.text, value) {
                    Ok(())
                } else {
                    self.type_error(format!("`{}` is not defined", n.text))
                }
            }
            Expr::Member { obj, name, .. } => {
                let o = self.eval(obj, env)?;
                self.set_member(&o, name, value)
            }
            Expr::Index { obj, index, .. } => {
                let o = self.eval(obj, env)?;
                let i = self.eval(index, env)?;
                self.index_set(&o, &i, value)
            }
            _ => self.type_error("invalid assignment target"),
        }
    }

    // ---- literals, numerics, casts ------------------------------------------------

    fn eval_literal(&self, kind: LitKind, text: &str) -> VResult {
        parse_literal(kind, text).map_err(|(class, msg)| self.throw(class, msg))
    }

    fn eval_unary(&mut self, op: UnaryOp, v: Value) -> VResult {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!self.value_truthy(&v)?)),
            UnaryOp::Plus => match v {
                Value::I32(_)
                | Value::I64(_)
                | Value::U32(_)
                | Value::U64(_)
                | Value::F32(_)
                | Value::F64(_) => Ok(v),
                _ => self.type_error("unary `+` needs a number"),
            },
            UnaryOp::Neg => match v {
                Value::BigIntV(b) => Ok(Value::BigIntV(Rc::new(b.negate()))),
                Value::BigDecV(d) => Ok(Value::BigDecV(Rc::new(BigDec {
                    coef: d.coef.negate(),
                    scale: d.scale,
                }))),
                Value::I32(n) => Ok(Value::I32(n.wrapping_neg())),
                Value::I64(n) => Ok(Value::I64(n.wrapping_neg())),
                Value::U32(n) => Ok(Value::U32(n.wrapping_neg())),
                Value::U64(n) => Ok(Value::U64(n.wrapping_neg())),
                Value::F32(f) => Ok(Value::F32(-f)),
                Value::F64(f) => Ok(Value::F64(-f)),
                _ => self.type_error("unary `-` needs a number"),
            },
            UnaryOp::BitNot => match v {
                Value::I32(n) => Ok(Value::I32(!n)),
                Value::I64(n) => Ok(Value::I64(!n)),
                Value::U32(n) => Ok(Value::U32(!n)),
                Value::U64(n) => Ok(Value::U64(!n)),
                _ => self.type_error("`~` needs an integer"),
            },
            UnaryOp::Await => self.type_error("`await` is not in the MVP"),
        }
    }

    fn values_equal(&self, a: &Value, b: &Value) -> Result<bool, Thrown> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Char(x), Value::Char(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::BigIntV(x), Value::BigIntV(y)) => x.cmp(y) == std::cmp::Ordering::Equal,
            (Value::BigDecV(x), Value::BigDecV(y)) => x.cmp(y) == std::cmp::Ordering::Equal,
            // Host objects compare by identity: the bridge's handle table
            // dedups by object, so equal handles are the same object.
            (Value::JsRef(x), Value::JsRef(y)) => x == y,
            (Value::Bytes(x), Value::Bytes(y)) => Rc::ptr_eq(x, y),
            (Value::MapV(x), Value::MapV(y)) => Rc::ptr_eq(x, y),
            (Value::SetV(x), Value::SetV(y)) => Rc::ptr_eq(x, y),
            (Value::Array(x), Value::Array(y)) => Rc::ptr_eq(x, y),
            (Value::Record(x), Value::Record(y)) => Rc::ptr_eq(x, y),
            (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(x, y),
            (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
            _ => {
                if let (Some(_), Some(_)) = (as_num(a), as_num(b)) {
                    let (x, y) = promote_pair(a, b)
                        .ok_or_else(|| self.throw("TypeError", "cannot compare these values"))?;
                    return Ok(num_eq(&x, &y));
                }
                return Err(self.throw(
                    "TypeError",
                    format!(
                        "`==` between {} and {} (no coercion, §3.3)",
                        kind_of(a),
                        kind_of(b)
                    ),
                ));
            }
        })
    }

    fn numeric_binop(&mut self, op: BinOp, l: Value, r: Value) -> VResult {
        // String / char concatenation and comparisons first.
        match (&l, &r, op) {
            (Value::Str(a), Value::Str(b), BinOp::Add) => {
                let mut s: Vec<char> = a.as_ref().clone();
                s.extend(b.iter());
                return Ok(Value::Str(Rc::new(s)));
            }
            (Value::Str(a), Value::Str(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            (Value::Char(a), Value::Char(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            _ => {}
        }
        match (&l, &r) {
            (Value::BigIntV(a), Value::BigIntV(b)) => return self.bigint_op(op, a, b),
            (Value::BigDecV(a), Value::BigDecV(b)) => return self.bigdec_op(op, a, b),
            _ => {}
        }
        let (a, b) = promote_pair(&l, &r).ok_or_else(|| {
            self.throw(
                "TypeError",
                format!(
                    "`{}` needs numeric operands, got {} and {}",
                    op.as_str(),
                    kind_of(&l),
                    kind_of(&r)
                ),
            )
        })?;
        self.promoted_binop(op, a, b)
    }

    fn promoted_binop(&mut self, op: BinOp, a: Value, b: Value) -> VResult {
        use BinOp::*;
        macro_rules! int_ops {
            ($x:expr, $y:expr, $wrap:ident, $t:ty, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x.wrapping_add(y)),
                    Sub => $mk(x.wrapping_sub(y)),
                    Mul => $mk(x.wrapping_mul(y)),
                    Div => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_div(y) {
                            Some(q) => $mk(q),
                            None => {
                                return Err(self.throw("RangeError", "integer overflow in division"))
                            }
                        }
                    }
                    Rem => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_rem(y) {
                            Some(q) => $mk(q),
                            None => $mk(0 as $t),
                        }
                    }
                    Pow => {
                        let mut acc: $t = 1 as $t;
                        let mut i = 0i64;
                        let n = y as i64;
                        if n < 0 {
                            return Err(self.throw("RangeError", "negative integer exponent"));
                        }
                        while i < n {
                            acc = acc.wrapping_mul(x);
                            i += 1;
                        }
                        $mk(acc)
                    }
                    Shl => $mk(x.wrapping_shl(y as u32)),
                    Shr => $mk(x.wrapping_shr(y as u32)),
                    BitAnd => $mk(x & y),
                    BitOr => $mk(x | y),
                    BitXor => $mk(x ^ y),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("bad operator"),
                }
            }};
        }
        macro_rules! float_ops {
            ($x:expr, $y:expr, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x + y),
                    Sub => $mk(x - y),
                    Mul => $mk(x * y),
                    Div => $mk(x / y),
                    Rem => $mk(x % y),
                    Pow => $mk(x.powf(y)),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("floats do not support this operator"),
                }
            }};
        }
        Ok(match (a, b) {
            (Value::I32(x), Value::I32(y)) => int_ops!(x, y, wrapping, i32, Value::I32),
            (Value::U32(x), Value::U32(y)) => int_ops!(x, y, wrapping, u32, Value::U32),
            (Value::I64(x), Value::I64(y)) => int_ops!(x, y, wrapping, i64, Value::I64),
            (Value::U64(x), Value::U64(y)) => int_ops!(x, y, wrapping, u64, Value::U64),
            (Value::F32(x), Value::F32(y)) => float_ops!(x, y, Value::F32),
            (Value::F64(x), Value::F64(y)) => float_ops!(x, y, Value::F64),
            _ => return self.type_error("operands did not promote to a common type"),
        })
    }

    fn bigint_op(&mut self, op: BinOp, a: &BigInt, b: &BigInt) -> VResult {
        use std::cmp::Ordering as O;
        use BinOp::*;
        Ok(match op {
            Add => Value::BigIntV(Rc::new(a.add(b))),
            Sub => Value::BigIntV(Rc::new(a.sub(b))),
            Mul => Value::BigIntV(Rc::new(a.mul(b))),
            Div | Rem => {
                let (q, r) = a
                    .divmod(b)
                    .ok_or_else(|| self.throw("RangeError", "division by zero"))?;
                Value::BigIntV(Rc::new(if op == Div { q } else { r }))
            }
            Lt => Value::Bool(a.cmp(b) == O::Less),
            Gt => Value::Bool(a.cmp(b) == O::Greater),
            Le => Value::Bool(a.cmp(b) != O::Greater),
            Ge => Value::Bool(a.cmp(b) != O::Less),
            _ => return self.type_error("operator not defined for bigint"),
        })
    }

    fn bigdec_op(&mut self, op: BinOp, a: &BigDec, b: &BigDec) -> VResult {
        use std::cmp::Ordering as O;
        use BinOp::*;
        Ok(match op {
            Add => Value::BigDecV(Rc::new(a.add(b))),
            Sub => Value::BigDecV(Rc::new(a.sub(b))),
            Mul => Value::BigDecV(Rc::new(a.mul(b))),
            Div => match a.div_exact(b) {
                Some(q) => Value::BigDecV(Rc::new(q)),
                None => {
                    return Err(self.throw(
                        "RangeError",
                        "inexact bigdec division needs a rounding context (§3.7)",
                    ))
                }
            },
            Lt => Value::Bool(a.cmp(b) == O::Less),
            Gt => Value::Bool(a.cmp(b) == O::Greater),
            Le => Value::Bool(a.cmp(b) != O::Greater),
            Ge => Value::Bool(a.cmp(b) != O::Less),
            _ => return self.type_error("operator not defined for bigdec"),
        })
    }

    fn eval_cast(&mut self, v: Value, wrapping: bool, ty: &Type) -> VResult {
        let Type::Named { name, .. } = ty else {
            return Ok(v); // casts to complex types: checker's concern
        };
        let out_of_range = || {
            self.throw(
                "RangeError",
                format!("value does not fit `{name}` (use `as wrapping`)"),
            )
        };
        let as_f = match as_num(&v) {
            Some(f) => f,
            None => return Ok(v), // non-numeric cast: reference cast, pass through
        };
        let as_i = as_i64(&v);
        macro_rules! to_int {
            ($t:ty, $mk:expr) => {{
                if wrapping {
                    match as_i {
                        Some(i) => $mk(i as $t),
                        None => $mk(as_f as $t), // saturating from float, defined
                    }
                } else {
                    match as_i {
                        Some(i) => match <$t>::try_from(i) {
                            Ok(x) => $mk(x),
                            Err(_) => return Err(out_of_range()),
                        },
                        None => {
                            let t = as_f.trunc();
                            if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 && t == as_f {
                                $mk(t as $t)
                            } else if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 {
                                $mk(t as $t) // fractional part dropped, in range
                            } else {
                                return Err(out_of_range());
                            }
                        }
                    }
                }
            }};
        }
        Ok(match name.as_str() {
            "int32" | "int" => to_int!(i32, Value::I32),
            "uint32" | "uint" => to_int!(u32, Value::U32),
            "int64" => to_int!(i64, Value::I64),
            "uint64" => {
                if wrapping {
                    match &v {
                        Value::I64(i) => Value::U64(*i as u64),
                        Value::I32(i) => Value::U64(*i as i64 as u64),
                        _ => Value::U64(as_f as u64),
                    }
                } else {
                    match as_i {
                        Some(i) if i >= 0 => Value::U64(i as u64),
                        Some(_) => return Err(out_of_range()),
                        None => match &v {
                            Value::U64(u) => Value::U64(*u),
                            _ => Value::U64(as_f as u64),
                        },
                    }
                }
            }
            "int8" => to_int!(i8, |x: i8| Value::I32(x as i32)),
            "int16" => to_int!(i16, |x: i16| Value::I32(x as i32)),
            "uint8" => to_int!(u8, |x: u8| Value::I32(x as i32)),
            "uint16" => to_int!(u16, |x: u16| Value::I32(x as i32)),
            "float64" | "float" => Value::F64(as_f),
            "float32" => Value::F32(as_f as f32),
            _ => v, // class/interface cast: dynamic checks arrive with the checker
        })
    }
}

fn graph_is_module(spec: &str) -> bool {
    mersey_front::graph::is_module(spec)
}

fn walk_pattern<'a>(p: &'a Pattern, out: &mut Vec<&'a str>) {
    match p {
        Pattern::Name(n) => out.push(&n.text),
        Pattern::Array { elems, rest } => {
            for e in elems {
                walk_pattern(&e.target, out);
            }
            if let Some(r) = rest {
                walk_pattern(r, out);
            }
        }
        Pattern::Record(fields) => {
            for f in fields {
                match &f.target {
                    Some(t) => walk_pattern(t, out),
                    None => out.push(&f.name.text),
                }
            }
        }
    }
}

/// Values a module exports, read out of its scope after evaluation.
fn collect_exports(module: &'static Module, env: &Env) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let mut take = |name: &str, exported: &str| {
        if let Some(v) = env_get(env, name) {
            out.insert(exported.to_string(), v);
        }
    };
    for item in &module.items {
        let Item::Export(ex) = item else { continue };
        match &ex.kind {
            ExportKind::Decl(d) => {
                let name = match d {
                    Decl::Function(f) => &f.name.text,
                    Decl::Class(c) => &c.name.text,
                    Decl::Enum(e) => &e.name.text,
                    // Interfaces and aliases are types only: no runtime value.
                    Decl::Interface(_) | Decl::TypeAlias(_) => continue,
                };
                take(name, name);
            }
            ExportKind::Var(v) => {
                for b in &v.bindings {
                    let mut names = Vec::new();
                    walk_pattern(&b.target, &mut names);
                    for n in names {
                        take(n, n);
                    }
                }
            }
            ExportKind::Named { specs, .. } => {
                // Re-exports (`export { x } from "./y"`) work because the
                // import already bound `x` into this module's scope.
                for s in specs {
                    let exported = s.alias.as_ref().unwrap_or(&s.name);
                    take(&s.name.text, &exported.text);
                }
            }
        }
    }
    out
}

/// RAII-ish frame for tracking the class whose method is executing (for
/// `super`). Kept tiny: a manual stack with a guard.
struct Frame<'a> {
    i: &'a mut Interp,
    pushed: bool,
}

impl<'a> Frame<'a> {
    fn enter(i: &'a mut Interp, c: &Closure) -> Frame<'a> {
        let pushed = if let Some(cls) = &c.cls {
            i.class_stack.push(cls.clone());
            true
        } else {
            false
        };
        Frame { i, pushed }
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if self.pushed {
            self.i.class_stack.pop();
        }
    }
}

// ---- helpers ------------------------------------------------------------------------

fn find_in_chain<T>(class: &Rc<ClassDef>, f: impl Fn(&Rc<ClassDef>) -> Option<T>) -> Option<T> {
    let mut cls = Some(class.clone());
    while let Some(c) = cls {
        if let Some(t) = f(&c) {
            return Some(t);
        }
        cls = c.parent.clone();
    }
    None
}

/// Every name a pattern binds (`let [a, b] = …`, `let {x} = …`).
/// A hard cap on Mersey call depth, as a backstop to the stack-usage guard
/// below. It exists so the limit is *deterministic* — the same program throws
/// at the same depth regardless of build or platform.
const MAX_CALL_DEPTH: usize = 3_000;

/// How much Rust stack the engine will let a Mersey program consume before it
/// throws.
///
/// Counting frames is the obvious guard and the wrong one: a debug build's
/// interpreter frames are several times fatter than a release build's, and a
/// browser worker's stack is a fraction of a native thread's — so any fixed
/// frame count is either uselessly small somewhere or fatally large somewhere
/// else. What actually matters is bytes, so the engine measures them: it notes
/// the stack address at the host boundary and compares against it on every
/// call. 512 KB fits inside the smallest stack the engine runs on (a browser
/// worker's, once the host has taken its share) with room for the deepest
/// single frame to complete.
const STACK_BUDGET: usize = 512 * 1024;

/// The current stack address, near enough for a budget check.
fn stack_here() -> usize {
    let probe = 0u8;
    std::hint::black_box(&probe) as *const u8 as usize
}

pub(crate) fn pattern_names_of(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Name(n) => out.push(n.text.clone()),
        Pattern::Array { elems, rest } => {
            for e in elems {
                pattern_names_of(&e.target, out);
            }
            if let Some(r) = rest {
                pattern_names_of(r, out);
            }
        }
        Pattern::Record(fields) => {
            for f in fields {
                match &f.target {
                    Some(p) => pattern_names_of(p, out),
                    None => out.push(f.name.text.clone()),
                }
            }
        }
    }
}

fn class_has_field(class: &Rc<ClassDef>, name: &str) -> bool {
    find_in_chain(class, |c| {
        c.fields.iter().any(|(n, _)| n == name).then_some(())
    })
    .is_some()
}

/// Allocate a tracked array (the collector must know about it).
pub(crate) fn new_array(items: Vec<Value>) -> Value {
    let a = Rc::new(GcCell::new(items));
    gc::track_array(&a);
    Value::Array(a)
}

pub(crate) fn new_record(fields: Vec<(String, Value)>) -> Value {
    let r = Rc::new(GcCell::new(fields));
    gc::track_record(&r);
    Value::Record(r)
}

pub(crate) fn new_map(entries: Vec<(Value, Value)>) -> Value {
    let m = Rc::new(GcCell::new(entries));
    gc::track_map(&m);
    Value::MapV(m)
}

pub(crate) fn new_set(items: Vec<Value>) -> Value {
    let sset = Rc::new(GcCell::new(items));
    gc::track_set(&sset);
    Value::SetV(sset)
}

/// Field lookup in an insertion-ordered record (records are small).
pub(crate) fn rec_get(fields: &[(String, Value)], name: &str) -> Option<Value> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// Set a field, preserving its original position if it already exists.
pub(crate) fn rec_set(fields: &mut Vec<(String, Value)>, name: &str, value: Value) {
    match fields.iter_mut().find(|(k, _)| k == name) {
        Some(slot) => slot.1 = value,
        None => fields.push((name.to_string(), value)),
    }
}

/// Howard Hinnant's civil-from-days / days-from-civil (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn as_num(v: &Value) -> Option<f64> {
    Some(match v {
        Value::I32(n) => *n as f64,
        Value::I64(n) => *n as f64,
        Value::U32(n) => *n as f64,
        Value::U64(n) => *n as f64,
        Value::F32(f) => *f as f64,
        Value::F64(f) => *f,
        _ => return None,
    })
}

fn as_i64(v: &Value) -> Option<i64> {
    Some(match v {
        Value::I32(n) => *n as i64,
        Value::I64(n) => *n,
        Value::U32(n) => *n as i64,
        Value::U64(n) => *n as i64,
        _ => return None,
    })
}

/// Usual arithmetic conversions (§3.3): float wins; wider rank wins;
/// unsigned wins at equal rank.
fn promote_pair(a: &Value, b: &Value) -> Option<(Value, Value)> {
    use Value::*;
    let rank = |v: &Value| match v {
        I32(_) => Some(0),
        U32(_) => Some(1),
        I64(_) => Some(2),
        U64(_) => Some(3),
        F32(_) => Some(4),
        F64(_) => Some(5),
        _ => None,
    };
    let (ra, rb) = (rank(a)?, rank(b)?);
    let target = ra.max(rb);
    let conv = |v: &Value| -> Value {
        match (v, target) {
            (I32(x), 0) => I32(*x),
            (v, 1) => U32(match v {
                I32(x) => *x as u32,
                U32(x) => *x,
                _ => unreachable!(),
            }),
            (v, 2) => I64(match v {
                I32(x) => *x as i64,
                U32(x) => *x as i64,
                I64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 3) => U64(match v {
                I32(x) => *x as i64 as u64,
                U32(x) => *x as u64,
                I64(x) => *x as u64,
                U64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 4) => F32(as_num(v).unwrap() as f32),
            (v, _) => F64(as_num(v).unwrap()),
        }
    };
    Some((conv(a), conv(b)))
}

fn num_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (I32(x), I32(y)) => x == y,
        (U32(x), U32(y)) => x == y,
        (I64(x), I64(y)) => x == y,
        (U64(x), U64(y)) => x == y,
        (F32(x), F32(y)) => x == y,
        (F64(x), F64(y)) => x == y,
        _ => false,
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::I32(_) => "int32",
        Value::I64(_) => "int64",
        Value::U32(_) => "uint32",
        Value::U64(_) => "uint64",
        Value::F32(_) => "float32",
        Value::F64(_) => "float64",
        Value::Char(_) => "char",
        Value::Str(_) => "string",
        Value::BigIntV(_) => "bigint",
        Value::BigDecV(_) => "bigdec",
        Value::MapV(_) => "Map",
        Value::SetV(_) => "Set",
        Value::Array(_) => "array",
        Value::Record(_) => "record",
        Value::Closure(_) => "function",
        Value::Class(_) => "class",
        Value::Instance(_) => "object",
        Value::Namespace(_) => "namespace",
        Value::Dom(_) => "dom element",
        Value::JsRef(_) => "web object",
        Value::Bytes(_) => "Bytes",
        Value::RegexV(_) => "Regex",
        Value::IterV(_) => "Iter",
        Value::PromiseV(_) => "Promise",
        Value::Resolver(..) | Value::AllSlot(..) | Value::PromiseExec(..) => "function",
        Value::Native(_) => "native function",
    }
}

pub fn to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::I32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F32(f) => f.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Char(c) => c.to_string(),
        Value::Str(s) => s.iter().collect(),
        Value::BigIntV(b) => b.to_decimal(),
        Value::BigDecV(d) => d.to_decimal(),
        Value::MapV(m) => {
            let items: Vec<String> = m
                .borrow()
                .iter()
                .map(|(k, v)| format!("{} => {}", to_display(k), to_display(v)))
                .collect();
            format!("Map{{{}}}", items.join(", "))
        }
        Value::SetV(m) => {
            let items: Vec<String> = m.borrow().iter().map(to_display).collect();
            format!("Set{{{}}}", items.join(", "))
        }
        Value::Array(a) => {
            let items: Vec<String> = a.borrow().iter().map(to_display).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Record(r) => {
            let fields: Vec<String> = r
                .borrow()
                .iter()
                .map(|(k, v)| format!("{k}: {}", to_display(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Closure(_) | Value::Native(_) => "<function>".to_string(),
        Value::Class(c) => format!("<class {}>", c.name),
        Value::Instance(i) => format!("<{}>", i.borrow().class.name),
        Value::Namespace(ns) => format!("<{}>", ns.name),
        Value::Dom(id) => format!("<#{id}>"),
        Value::JsRef(h) => format!("<web:{h}>"),
        Value::Bytes(b) => format!("<Bytes[{}]>", b.borrow().len()),
        Value::RegexV(_) => "<Regex>".to_string(),
        Value::IterV(_) => "<Iter>".to_string(),
        Value::PromiseV(_) => "<Promise>".to_string(),
        Value::Resolver(..) | Value::AllSlot(..) | Value::PromiseExec(..) => {
            "<function>".to_string()
        }
    }
}

/// Literal text → runtime value; pure so the bytecode compiler can bake
/// constants at compile time. Err = (error class, message).
pub(crate) fn parse_literal(kind: LitKind, text: &str) -> Result<Value, (&'static str, String)> {
    match kind {
        LitKind::Null => Ok(Value::Null),
        LitKind::Bool => Ok(Value::Bool(text == "true")),
        LitKind::Str => {
            let inner = &text[1..text.len() - 1];
            Ok(Value::Str(Rc::new(unescape(inner).chars().collect())))
        }
        LitKind::Char => {
            let inner = &text[2..text.len() - 1]; // strip c' and '
            let s = unescape(inner);
            s.chars()
                .next()
                .map(Value::Char)
                .ok_or_else(|| ("TypeError", "empty char literal".to_string()))
        }
        LitKind::Int => parse_int_literal(text),
        LitKind::Float => {
            let is_f32 = text.ends_with('f');
            let core: String = text.trim_end_matches('f').replace('_', "");
            let v: f64 = core
                .parse()
                .map_err(|_| ("TypeError", format!("bad float literal `{text}`")))?;
            Ok(if is_f32 {
                Value::F32(v as f32)
            } else {
                Value::F64(v)
            })
        }
        LitKind::BigInt => {
            let t = text.replace('_', "");
            let body = t.trim_end_matches('n');
            let (radix, body) = if let Some(b) = body.strip_prefix("0x") {
                (16, b)
            } else if let Some(b) = body.strip_prefix("0o") {
                (8, b)
            } else if let Some(b) = body.strip_prefix("0b") {
                (2, b)
            } else {
                (10, body)
            };
            match BigInt::parse(body, radix) {
                Some(b) => Ok(Value::BigIntV(Rc::new(b))),
                None => Err(("TypeError", format!("bad bigint literal `{text}`"))),
            }
        }
        LitKind::BigDec => {
            let t = text.replace('_', "");
            match BigDec::parse(t.trim_end_matches('m')) {
                Some(b) => Ok(Value::BigDecV(Rc::new(b))),
                None => Err(("TypeError", format!("bad bigdec literal `{text}`"))),
            }
        }
    }
}

/// `-2147483648` is a perfectly good int32, but `2147483648` is not — so a
/// minus sign in front of an integer literal has to be *part of the literal*,
/// not an operation applied to it afterwards. Both tiers fold it here.
pub(crate) fn negated_int_literal(text: &str) -> Result<Value, (&'static str, String)> {
    parse_int_literal_signed(text, true)
}

fn parse_int_literal(text: &str) -> Result<Value, (&'static str, String)> {
    parse_int_literal_signed(text, false)
}

fn parse_int_literal_signed(text: &str, neg: bool) -> Result<Value, (&'static str, String)> {
    let t = text.replace('_', "");
    const SUFFIXES: &[&str] = &[
        "u64", "u32", "u16", "ul", "u8", "i64", "i32", "i16", "i8", "l", "u",
    ];
    let suffix = SUFFIXES
        .iter()
        .find(|s| t.ends_with(**s))
        .copied()
        .unwrap_or("");
    let digits = &t[..t.len() - suffix.len()];
    let (radix, body) = if let Some(b) = digits.strip_prefix("0x") {
        (16, b)
    } else if let Some(b) = digits.strip_prefix("0o") {
        (8, b)
    } else if let Some(b) = digits.strip_prefix("0b") {
        (2, b)
    } else {
        (10, digits)
    };
    let raw = u64::from_str_radix(body, radix)
        .map_err(|_| ("RangeError", format!("integer literal `{text}` overflows")))?;
    let sign = if neg { "-" } else { "" };
    let out_of = || {
        (
            "RangeError",
            format!("literal `{sign}{text}` does not fit its type"),
        )
    };
    // Widen before applying the sign, so `-2147483648` is representable even
    // though `2147483648` is not.
    let v: i128 = if neg { -(raw as i128) } else { raw as i128 };
    let fit = |lo: i128, hi: i128| -> Result<i128, (&'static str, String)> {
        if v >= lo && v <= hi {
            Ok(v)
        } else {
            Err(out_of())
        }
    };
    Ok(match suffix {
        "" | "i32" => Value::I32(fit(i32::MIN as i128, i32::MAX as i128)? as i32),
        "u" | "u32" => Value::U32(fit(0, u32::MAX as i128)? as u32),
        "l" | "i64" => Value::I64(fit(i64::MIN as i128, i64::MAX as i128)? as i64),
        "ul" | "u64" => Value::U64(fit(0, u64::MAX as i128)? as u64),
        // Small types promote to int32 immediately (§3.3 rule 1).
        "i8" => Value::I32(fit(i8::MIN as i128, i8::MAX as i128)? as i32),
        "i16" => Value::I32(fit(i16::MIN as i128, i16::MAX as i128)? as i32),
        "u8" => Value::I32(fit(0, u8::MAX as i128)? as i32),
        "u16" => Value::I32(fit(0, u16::MAX as i128)? as i32),
        _ => return Err(("TypeError", format!("unsupported suffix on `{text}`"))),
    })
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                // \u{XXXX}
                let mut val = 0u32;
                for c in chars.by_ref() {
                    if c == '{' {
                        continue;
                    }
                    if c == '}' {
                        break;
                    }
                    val = val * 16 + c.to_digit(16).unwrap_or(0);
                }
                if let Some(c) = char::from_u32(val) {
                    out.push(c);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}
