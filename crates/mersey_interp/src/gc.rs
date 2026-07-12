//! Cycle collector: generational mark–sweep.
//!
//! The heap is reference-counted (`Rc`), which frees acyclic garbage
//! immediately but leaks cycles — and the most ordinary Mersey object graph
//! is cyclic: an instance holds a closure, the closure holds the scope, the
//! scope holds the instance. This collector exists for exactly that.
//!
//! **Safety model.** Collection only runs at a *host boundary* — after a
//! module finishes, or after a callback and its microtasks drain. At those
//! points no VM operand stack is live on the Rust stack, so the roots really
//! are the roots: module scopes, exports, registered callbacks, pending
//! tasks, and the classes. `gc.collect()` from Mersey therefore *requests* a
//! collection (a flag) rather than performing one mid-expression, which would
//! be able to sweep objects held only by live frames.
//!
//! **Generations.** Tracing the whole heap at every safe point makes the pause
//! grow with total live data: a program that builds one large long-lived
//! structure then pays for it at every collection, forever. So a *minor*
//! collection traces only the young generation (objects allocated since the
//! last collection) and does not traverse into old objects at all. That is
//! sound because of one invariant:
//!
//! > If an old object points at a young object, that pointer was *stored*
//! > there since the last collection — anything older was traced last time,
//! > and its survivors were promoted.
//!
//! So the young objects reachable from the old generation are exactly those
//! reachable from old objects that have been *written to*. Every collectable
//! container is therefore a [`GcCell`], whose `borrow_mut` records the write:
//! the barrier is not something a future mutation site can forget to call.
//! The old objects so recorded — the *remembered set* — are traced as extra
//! roots.
//!
//! A minor collection cannot reclaim a cycle that lies entirely in the old
//! generation, so the full trace still runs, just rarely (every `MAJOR_EVERY`
//! collections).
//!
//! Set `MERSEY_GC_VERIFY=1` to check the invariant rather than trust it: each
//! minor collection is cross-checked against a full trace, and a young object
//! that the full trace reaches but the minor trace missed — a write barrier
//! that failed to fire — aborts loudly instead of silently sweeping live data.
//!
//! Sweeping does not free memory directly (the `Rc`s may still be pointed at
//! by the very cycle we are breaking): it *clears* an unreachable object's
//! contents, which drops the edges, which drops the refcounts to zero.

use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use crate::{ClassDef, Coro, Env, GenState, Instance, PromiseState, Scope, Value};

/// A `RefCell` whose `borrow_mut` **is** the generational write barrier.
///
/// `Rc<GcCell<T>>::as_ptr()` is the address of the `GcCell` itself, which is
/// the identity the collector marks with — so the barrier can name the object
/// it is recording from `&self` alone.
pub struct GcCell<T> {
    inner: RefCell<T>,
}

impl<T> GcCell<T> {
    pub fn new(value: T) -> GcCell<T> {
        GcCell {
            inner: RefCell::new(value),
        }
    }

    pub fn borrow(&self) -> Ref<'_, T> {
        self.inner.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        note_write(self as *const GcCell<T> as usize);
        self.inner.borrow_mut()
    }
}

impl<T> Drop for GcCell<T> {
    /// Refcounting frees objects behind the collector's back, so this is where
    /// the old generation learns that one of its members is gone. It matters
    /// for more than tidiness: a stale address left in `OLD` would be reused
    /// by a later allocation, and that new object would then be treated as
    /// old — skipped by every minor trace, and eventually swept while live.
    fn drop(&mut self) {
        let ptr = self as *const GcCell<T> as usize;
        // try_with: a GcCell can outlive the thread_locals at shutdown.
        let _ = OLD.try_with(|old| {
            old.borrow_mut().remove(&ptr);
        });
        let _ = REMEMBERED.try_with(|r| {
            r.borrow_mut().remove(&ptr);
        });
    }
}

/// Record a write into an old object. Young objects are traced in full
/// anyway, so only the old generation needs remembering.
fn note_write(ptr: usize) {
    let is_old = OLD
        .try_with(|old| old.borrow().contains_key(&ptr))
        .unwrap_or(false);
    if is_old {
        let _ = REMEMBERED.try_with(|r| {
            r.borrow_mut().insert(ptr);
        });
    }
}

/// Every collectable object, weakly held.
pub(crate) enum GcObj {
    Env(Weak<GcCell<Scope>>),
    Inst(Weak<GcCell<Instance>>),
    Arr(Weak<GcCell<Vec<Value>>>),
    Rec(Weak<GcCell<Vec<(String, Value)>>>),
    MapV(Weak<GcCell<Vec<(Value, Value)>>>),
    SetV(Weak<GcCell<Vec<Value>>>),
    Prom(Weak<GcCell<PromiseState>>),
    Gen(Weak<GcCell<GenState>>),
}

impl GcObj {
    /// The object's identity, or `None` if refcounting already freed it.
    fn ptr(&self) -> Option<usize> {
        fn p<T>(w: &Weak<GcCell<T>>) -> Option<usize> {
            w.upgrade().map(|rc| Rc::as_ptr(&rc) as usize)
        }
        match self {
            GcObj::Env(w) => p(w),
            GcObj::Inst(w) => p(w),
            GcObj::Arr(w) | GcObj::SetV(w) => p(w),
            GcObj::Rec(w) => p(w),
            GcObj::MapV(w) => p(w),
            GcObj::Prom(w) => p(w),
            GcObj::Gen(w) => p(w),
        }
    }

    /// Break every edge out of this object, so the cycle it sits in collapses
    /// under refcounting.
    fn clear(&self) {
        match self {
            GcObj::Env(w) => {
                if let Some(rc) = w.upgrade() {
                    let mut s = rc.borrow_mut();
                    s.vars.clear();
                    s.parent = None;
                }
            }
            GcObj::Inst(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().slots.clear();
                }
            }
            GcObj::Arr(w) | GcObj::SetV(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().clear();
                }
            }
            GcObj::Rec(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().clear();
                }
            }
            GcObj::MapV(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().clear();
                }
            }
            GcObj::Prom(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().clear_edges();
                }
            }
            GcObj::Gen(w) => {
                if let Some(rc) = w.upgrade() {
                    rc.borrow_mut().discard();
                }
            }
        }
    }
}

thread_local! {
    /// Objects allocated since the last collection.
    static YOUNG: RefCell<Vec<GcObj>> = const { RefCell::new(Vec::new()) };
    /// The old generation: objects that have survived a collection, keyed by
    /// the identity a minor trace stops at. Kept exact by `GcCell::drop`, so
    /// a minor collection never walks it — which is the whole pause bound.
    static OLD: RefCell<HashMap<usize, GcObj>> = RefCell::new(HashMap::new());
    /// Old objects written to since the last collection: the extra roots that
    /// make a young-only trace sound.
    static REMEMBERED: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    /// Every class ever defined (weakly). Classes are never swept and they
    /// hold statics, so they are scanned in full on every collection — which
    /// is also why statics need no write barrier.
    static CLASSES: RefCell<Vec<Weak<ClassDef>>> = const { RefCell::new(Vec::new()) };
    /// Objects registered since the last collection.
    static SINCE_GC: RefCell<usize> = const { RefCell::new(0) };
    /// Minor collections since the last major one.
    static MINORS: RefCell<usize> = const { RefCell::new(0) };
    /// Size of the old generation just after the last full trace.
    static OLD_AT_MAJOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Collect when this many objects have been registered since the last run.
/// `MERSEY_GC_EVERY` lowers it so tests can force many collections over a
/// small heap.
const THRESHOLD: usize = 20_000;

thread_local! {
    /// Allocations between collections. Lowering it forces many collections
    /// over a small heap, which is how the barrier gets exercised in tests.
    static GC_EVERY: std::cell::Cell<usize> = std::cell::Cell::new(
        std::env::var("MERSEY_GC_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(THRESHOLD),
    );
    /// Full traces every Nth collection.
    static MAJOR_EVERY_N: std::cell::Cell<usize> = const { std::cell::Cell::new(MAJOR_EVERY) };
    /// Cross-check every minor collection against a full trace.
    static VERIFY: std::cell::Cell<bool> = std::cell::Cell::new(
        std::env::var("MERSEY_GC_VERIFY").map(|v| v != "0").unwrap_or(false),
    );
}

/// Collect this often (objects allocated). Tests use it to force collections.
pub fn set_threshold(n: usize) {
    GC_EVERY.with(|c| c.set(n));
}

/// Trace the whole heap every Nth collection. `0` means always — i.e. the
/// non-generational behaviour, which is what the pause-time benchmark
/// compares against.
pub fn set_major_every(n: usize) {
    MAJOR_EVERY_N.with(|c| c.set(n));
}

/// Check the write-barrier invariant on every minor collection instead of
/// trusting it. Costs a full trace per collection, so: tests and fuzzing.
pub fn set_verify(on: bool) {
    VERIFY.with(|c| c.set(on));
}

fn threshold() -> usize {
    GC_EVERY.with(|c| c.get())
}
/// A full trace runs when the old generation has grown by this factor since
/// the last one. Triggering on *growth* rather than on a fixed count is what
/// keeps a steady-state program (a big heap it keeps, plus per-event churn)
/// from paying for a full trace over and over: if the old generation is not
/// growing, there is little for a full trace to find.
const MAJOR_GROWTH: f64 = 2.0;
/// …but old-generation cycles are invisible to a minor trace, so a full trace
/// also runs at least this often, to bound how long they can accumulate.
const MAJOR_EVERY: usize = 64;

fn register(obj: GcObj) {
    YOUNG.with(|h| h.borrow_mut().push(obj));
    SINCE_GC.with(|n| *n.borrow_mut() += 1);
}

pub(crate) fn track_env(e: &Env) {
    register(GcObj::Env(Rc::downgrade(e)));
}
pub(crate) fn track_instance(i: &Rc<GcCell<Instance>>) {
    register(GcObj::Inst(Rc::downgrade(i)));
}
pub(crate) fn track_array(a: &Rc<GcCell<Vec<Value>>>) {
    register(GcObj::Arr(Rc::downgrade(a)));
}
pub(crate) fn track_record(r: &Rc<GcCell<Vec<(String, Value)>>>) {
    register(GcObj::Rec(Rc::downgrade(r)));
}
pub(crate) fn track_map(m: &Rc<GcCell<Vec<(Value, Value)>>>) {
    register(GcObj::MapV(Rc::downgrade(m)));
}
pub(crate) fn track_set(s: &Rc<GcCell<Vec<Value>>>) {
    register(GcObj::SetV(Rc::downgrade(s)));
}
pub(crate) fn track_promise(p: &Rc<GcCell<PromiseState>>) {
    register(GcObj::Prom(Rc::downgrade(p)));
}
pub(crate) fn track_gen(g: &Rc<GcCell<GenState>>) {
    register(GcObj::Gen(Rc::downgrade(g)));
}
pub(crate) fn track_class(c: &Rc<ClassDef>) {
    CLASSES.with(|cs| cs.borrow_mut().push(Rc::downgrade(c)));
}

/// Has enough been allocated to be worth a collection?
pub(crate) fn should_collect() -> bool {
    SINCE_GC.with(|n| *n.borrow() >= threshold())
}

/// Report the heap WITHOUT sweeping. Safe to call anywhere: a real collection
/// mid-expression could sweep objects held only by live VM frames, which are
/// not roots.
pub(crate) fn stats_only() -> GcStats {
    let young = YOUNG.with(|y| {
        let mut objs = y.borrow_mut();
        objs.retain(|o| o.ptr().is_some());
        objs.len()
    });
    let old = OLD.with(|o| o.borrow().len());
    let n = young + old;
    GcStats {
        tracked: n,
        reachable: n,
        collected: 0,
        major: false,
    }
}

pub struct GcStats {
    pub tracked: usize,
    pub reachable: usize,
    pub collected: usize,
    /// Did this collection trace the whole heap?
    pub major: bool,
}

/// What the engine considers live at a safe point.
#[derive(Default)]
pub(crate) struct Roots {
    pub values: Vec<Value>,
    pub envs: Vec<Env>,
    pub coros: Vec<Rc<GcCell<PromiseState>>>,
    pub classes: Vec<Rc<ClassDef>>,
}

/// Collect. Most collections are minor (young generation only); every
/// `MAJOR_EVERY`th traces the whole heap, so old-generation cycles are
/// reclaimed as well.
pub(crate) fn collect(roots: &Roots) -> GcStats {
    let every = MAJOR_EVERY_N.with(|c| c.get());
    let minors = MINORS.with(|m| *m.borrow());
    let old_now = OLD.with(|o| o.borrow().len());
    let old_then = OLD_AT_MAJOR.with(|c| c.get());
    let grown = old_now as f64 >= (old_then as f64 * MAJOR_GROWTH).max(1_000.0);
    // `set_major_every(0)` forces a full trace every time — the
    // non-generational behaviour the pause benchmark compares against.
    let major = every == 0 || minors >= every || grown;
    collect_gen(roots, major)
}

/// Force a full trace.
pub(crate) fn collect_major(roots: &Roots) -> GcStats {
    collect_gen(roots, true)
}

fn collect_gen(roots: &Roots, major: bool) -> GcStats {
    let marked = mark(roots, !major);

    // The write barrier is the load-bearing part of a minor collection: an
    // unrecorded store into an old object makes the young object it points at
    // invisible to the trace, and it is then swept while live. Under
    // MERSEY_GC_VERIFY, prove that did not happen before sweeping anything.
    if !major && verify_enabled() {
        verify_minor(roots, &marked);
    }

    let mut collected = 0usize;
    let mut reachable = 0usize;

    // Young: survivors are promoted, the rest swept.
    //
    // Nothing below may hold OLD or REMEMBERED borrowed across a `clear()`:
    // clearing drops values, a drop can free a GcCell, and `GcCell::drop`
    // reaches back into both.
    let young: Vec<GcObj> = YOUNG.with(|y| y.borrow_mut().drain(..).collect());
    let mut promoted: Vec<(usize, GcObj)> = Vec::new();
    for obj in young {
        match obj.ptr() {
            None => {} // refcounting already freed it
            Some(p) if marked.contains(&p) => {
                reachable += 1;
                promoted.push((p, obj));
            }
            Some(_) => {
                obj.clear();
                collected += 1;
            }
        }
    }

    if major {
        // Only a full trace knows the truth about the old generation. Take the
        // dead out of the map *before* clearing them, so their drops do not
        // re-enter it.
        let dead: Vec<GcObj> = OLD.with(|old| {
            let mut old = old.borrow_mut();
            let doomed: Vec<usize> = old
                .iter()
                .filter(|(p, obj)| obj.ptr().is_none() || !marked.contains(p))
                .map(|(p, _)| *p)
                .collect();
            doomed.iter().filter_map(|p| old.remove(p)).collect()
        });
        for obj in &dead {
            if obj.ptr().is_some() {
                obj.clear();
                collected += 1;
            }
        }
        reachable += OLD.with(|old| old.borrow().len());
    } else {
        // A minor trace never visited the old generation, so it cannot
        // conclude anything about it — and, importantly, does not walk it.
        reachable += OLD.with(|old| old.borrow().len());
    }

    OLD.with(|old| {
        let mut old = old.borrow_mut();
        for (p, obj) in promoted {
            old.insert(p, obj);
        }
    });

    // Sweeping wrote through GcCells — that is how the edges get dropped —
    // which re-armed the barrier for objects that are now garbage.
    REMEMBERED.with(|r| r.borrow_mut().clear());
    SINCE_GC.with(|n| *n.borrow_mut() = 0);
    MINORS.with(|m| {
        let mut m = m.borrow_mut();
        *m = if major { 0 } else { *m + 1 };
    });

    let tracked = OLD.with(|old| old.borrow().len());
    if major {
        OLD_AT_MAJOR.with(|c| c.set(tracked));
    }
    GcStats {
        tracked,
        reachable,
        collected,
        major,
    }
}

fn verify_enabled() -> bool {
    VERIFY.with(|c| c.get())
}

/// Cross-check a minor trace against a full one: every *young* object the full
/// trace reaches must also have been reached by the minor trace.
fn verify_minor(roots: &Roots, minor_marked: &HashSet<usize>) {
    let full = mark(roots, false);
    let missed: Vec<usize> = YOUNG.with(|y| {
        y.borrow()
            .iter()
            .filter_map(|obj| obj.ptr())
            .filter(|p| full.contains(p) && !minor_marked.contains(p))
            .collect()
    });
    assert!(
        missed.is_empty(),
        "GC write barrier missed a store: {} young object(s) are reachable from the full \
         trace but not from the minor trace, and were about to be swept while live (first: \
         {:#x})",
        missed.len(),
        missed[0]
    );
}

/// Trace from the roots. When `minor`, traversal stops at old objects — the
/// remembered set covers whatever young objects they point at.
fn mark(roots: &Roots, minor: bool) -> HashSet<usize> {
    OLD.with(|old| {
        REMEMBERED.with(|remembered| {
            let old = old.borrow();
            let remembered = remembered.borrow();
            let mut m = Marker {
                marked: HashSet::new(),
                envs: Vec::new(),
                minor,
                old: &old,
                remembered: &remembered,
            };

            // Classes are never swept and hold statics, so they are always
            // scanned in full.
            let classes: Vec<Rc<ClassDef>> = CLASSES.with(|cs| {
                let mut cs = cs.borrow_mut();
                cs.retain(|w| w.strong_count() > 0);
                cs.iter().filter_map(|w| w.upgrade()).collect()
            });
            for c in &classes {
                m.class(c);
            }

            for v in &roots.values {
                m.value(v);
            }
            for e in &roots.envs {
                m.env(e);
            }
            for c in &roots.coros {
                m.promise(c);
            }
            for c in &roots.classes {
                m.class(c);
            }

            // The remembered set: the old objects written to since the last
            // collection. Without these a minor trace would miss every young
            // object that only an old object points at. This is a lookup per
            // *dirty* object, not a walk of the old generation.
            if minor {
                let dirty: Vec<GcObj> = remembered
                    .iter()
                    .filter_map(|p| old.get(p).map(|o| o.dup()))
                    .collect();
                for obj in &dirty {
                    if let Some(p) = obj.ptr() {
                        m.marked.insert(p);
                    }
                    m.scan(obj);
                }
            }

            while let Some(e) = m.envs.pop() {
                m.env_contents(&e);
            }
            m.marked
        })
    })
}

impl GcObj {
    /// A second weak handle to the same object (`Weak` is cheap to clone).
    fn dup(&self) -> GcObj {
        match self {
            GcObj::Env(w) => GcObj::Env(w.clone()),
            GcObj::Inst(w) => GcObj::Inst(w.clone()),
            GcObj::Arr(w) => GcObj::Arr(w.clone()),
            GcObj::Rec(w) => GcObj::Rec(w.clone()),
            GcObj::MapV(w) => GcObj::MapV(w.clone()),
            GcObj::SetV(w) => GcObj::SetV(w.clone()),
            GcObj::Prom(w) => GcObj::Prom(w.clone()),
            GcObj::Gen(w) => GcObj::Gen(w.clone()),
        }
    }
}

struct Marker<'a> {
    marked: HashSet<usize>,
    /// Scopes still to scan (kept off the Rust stack: scope chains are deep).
    envs: Vec<Env>,
    minor: bool,
    old: &'a HashMap<usize, GcObj>,
    remembered: &'a HashSet<usize>,
}

impl Marker<'_> {
    /// Mark `ptr`, and say whether its children should be traversed: an
    /// already-marked object is done, and in a minor collection an untouched
    /// old object is not traversed at all — which is what bounds the pause.
    fn enter(&mut self, ptr: usize) -> bool {
        if self.minor && self.old.contains_key(&ptr) && !self.remembered.contains(&ptr) {
            self.marked.insert(ptr);
            return false;
        }
        self.marked.insert(ptr)
    }

    /// Scan an object's outgoing edges regardless of its generation (used for
    /// the remembered set, whose members are old by definition).
    fn scan(&mut self, obj: &GcObj) {
        match obj {
            GcObj::Env(w) => {
                if let Some(rc) = w.upgrade() {
                    self.envs.push(rc);
                }
            }
            GcObj::Inst(w) => {
                if let Some(rc) = w.upgrade() {
                    let (slots, class) = {
                        let i = rc.borrow();
                        (i.slots.clone(), i.class.clone())
                    };
                    for v in &slots {
                        self.value(v);
                    }
                    self.class(&class);
                }
            }
            GcObj::Arr(w) | GcObj::SetV(w) => {
                if let Some(rc) = w.upgrade() {
                    let items = rc.borrow().clone();
                    for v in &items {
                        self.value(v);
                    }
                }
            }
            GcObj::Rec(w) => {
                if let Some(rc) = w.upgrade() {
                    let items = rc.borrow().clone();
                    for (_, v) in &items {
                        self.value(v);
                    }
                }
            }
            GcObj::MapV(w) => {
                if let Some(rc) = w.upgrade() {
                    let items = rc.borrow().clone();
                    for (k, v) in &items {
                        self.value(k);
                        self.value(v);
                    }
                }
            }
            GcObj::Prom(w) => {
                if let Some(rc) = w.upgrade() {
                    self.promise_contents(&rc);
                }
            }
            GcObj::Gen(w) => {
                if let Some(rc) = w.upgrade() {
                    self.gen_contents(&rc);
                }
            }
        }
    }

    fn env(&mut self, e: &Env) {
        if self.enter(Rc::as_ptr(e) as usize) {
            self.envs.push(e.clone());
        }
    }

    fn env_contents(&mut self, e: &Env) {
        let (vars, parent) = {
            let s = e.borrow();
            (
                s.vars.values().cloned().collect::<Vec<Value>>(),
                s.parent.clone(),
            )
        };
        for v in &vars {
            self.value(v);
        }
        if let Some(p) = parent {
            self.env(&p);
        }
    }

    fn class(&mut self, c: &Rc<ClassDef>) {
        // Classes are never swept, so they are not generational: always scan.
        if !self.marked.insert(Rc::as_ptr(c) as usize) {
            return;
        }
        if let Some(e) = &c.env {
            self.env(e);
        }
        let statics: Vec<Value> = c.statics.borrow().values().cloned().collect();
        for v in &statics {
            self.value(v);
        }
        if let Some(p) = &c.parent {
            self.class(p);
        }
    }

    fn promise(&mut self, p: &Rc<GcCell<PromiseState>>) {
        if self.enter(Rc::as_ptr(p) as usize) {
            self.promise_contents(p);
        }
    }

    fn promise_contents(&mut self, p: &Rc<GcCell<PromiseState>>) {
        let (value, waiters, reactions) = {
            let st = p.borrow();
            (
                st.value.clone(),
                st.waiters().to_vec(),
                st.reactions().to_vec(),
            )
        };
        self.value(&value);
        for coro in &waiters {
            self.coro(coro);
        }
        for (ok, err, down) in &reactions {
            if let Some(v) = ok {
                self.value(v);
            }
            if let Some(v) = err {
                self.value(v);
            }
            self.promise(down);
        }
    }

    /// A suspended coroutine: its operand stack and scope chain may be the
    /// only reference to everything it was working on.
    fn coro(&mut self, coro: &Coro) {
        for e in &coro.scopes {
            self.env(e);
        }
        for v in &coro.stack {
            self.value(v);
        }
        if let Some(c) = &coro.cls {
            self.class(c);
        }
        self.promise(&coro.result);
    }

    fn gen_contents(&mut self, g: &Rc<GcCell<GenState>>) {
        let saved = g.borrow().saved();
        if let Some(coro) = saved {
            self.coro(&coro);
        }
    }

    fn value(&mut self, v: &Value) {
        match v {
            Value::Array(a) | Value::SetV(a) => {
                if self.enter(Rc::as_ptr(a) as usize) {
                    let items = a.borrow().clone();
                    for item in &items {
                        self.value(item);
                    }
                }
            }
            Value::Record(r) => {
                if self.enter(Rc::as_ptr(r) as usize) {
                    let items = r.borrow().clone();
                    for (_, item) in &items {
                        self.value(item);
                    }
                }
            }
            Value::MapV(m) => {
                if self.enter(Rc::as_ptr(m) as usize) {
                    let items = m.borrow().clone();
                    for (k, val) in &items {
                        self.value(k);
                        self.value(val);
                    }
                }
            }
            Value::Instance(i) => {
                if self.enter(Rc::as_ptr(i) as usize) {
                    let (slots, class) = {
                        let inst = i.borrow();
                        (inst.slots.clone(), inst.class.clone())
                    };
                    for f in &slots {
                        self.value(f);
                    }
                    self.class(&class);
                }
            }
            Value::Closure(c) => {
                // Closures are immutable once created, so they are never
                // tracked and never generational: always scan.
                if self.marked.insert(Rc::as_ptr(c) as usize) {
                    self.env(&c.env);
                    if let Some(t) = &c.this {
                        self.value(t);
                    }
                    if let Some(cl) = &c.cls {
                        self.class(cl);
                    }
                }
            }
            Value::Class(c) => self.class(c),
            Value::Namespace(ns) => {
                if self.marked.insert(Rc::as_ptr(ns) as usize) {
                    let entries: Vec<Value> = ns.entries.values().cloned().collect();
                    for v in &entries {
                        self.value(v);
                    }
                }
            }
            // A suspended generator holds a whole coroutine: its saved operand
            // stack can be the only reference to the values it was building.
            Value::IterV(g) => {
                if self.enter(Rc::as_ptr(g) as usize) {
                    self.gen_contents(g);
                }
            }
            Value::PromiseV(p) | Value::Resolver(p, _) | Value::PromiseExec(p) => self.promise(p),
            _ => {}
        }
    }
}
