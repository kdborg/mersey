// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

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
pub struct GcCell<T: GcContents> {
    inner: RefCell<T>,
}

impl<T: GcContents> GcCell<T> {
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

    /// Borrow, or `None` if the interpreter is already inside this object.
    ///
    /// The cycle collector runs at points where nothing *should* be borrowed,
    /// but "should" is not a guarantee it is willing to crash on: an object it
    /// cannot read is treated as live and left alone, which costs a missed cycle
    /// and never a wrong answer.
    pub(crate) fn try_borrow(&self) -> Option<Ref<'_, T>> {
        self.inner.try_borrow().ok()
    }

    pub(crate) fn try_borrow_mut(&self) -> Option<RefMut<'_, T>> {
        note_write(self as *const GcCell<T> as usize);
        self.inner.try_borrow_mut().ok()
    }
}

/// An object holding values the collector must see — and, just as importantly,
/// that *dropping* must not recurse through.
pub trait GcContents {
    /// Move every value out of this object.
    fn take_children(&mut self, out: &mut Vec<Value>);
}

thread_local! {
    /// Young-list length at which dead entries are swept out (see `prune`).
    static PRUNE_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(THRESHOLD) };
    /// Allocations since the last cycle collection at which another is worth
    /// running (see `collect_cycles`). Grows with the live heap.
    static CYCLE_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(THRESHOLD) };
    /// Values whose drop has been deferred (see `Drop for GcCell`).
    static DRAIN: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    /// Is a drop-drain already running on this thread?
    static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl<T: GcContents> Drop for GcCell<T> {
    /// Two jobs, both about what refcounting does behind the collector's back.
    ///
    /// **Keeping the old generation exact.** A stale address left in `OLD` would
    /// be reused by a later allocation, and that new object would then be
    /// treated as old: skipped by every minor trace, and eventually swept while
    /// live.
    ///
    /// **Not overflowing the stack.** `Rc` frees a linked structure by
    /// recursion — dropping the head drops the next, which drops the next. A
    /// list of 300,000 nodes is an ordinary thing to build with an ordinary
    /// loop, and it would become 300,000 Rust frames and abort the process.
    /// Hostile input must not be able to crash the engine (§5.2), so a drop does
    /// not recurse: it moves its children onto a queue, and the outermost drop
    /// drains that queue in a loop.
    fn drop(&mut self) {
        let ptr = self as *const GcCell<T> as usize;
        // try_with: a GcCell can outlive the thread_locals at shutdown.
        let _ = OLD.try_with(|old| {
            old.borrow_mut().remove(&ptr);
        });
        let _ = REMEMBERED.try_with(|r| {
            r.borrow_mut().remove(&ptr);
        });

        let mut children = Vec::new();
        self.inner.get_mut().take_children(&mut children);
        if children.is_empty() {
            return;
        }
        let Ok(already_draining) = DRAINING.try_with(|d| d.get()) else {
            return; // thread shutdown: let the ordinary drop run
        };
        if DRAIN.try_with(|q| q.borrow_mut().extend(children)).is_err() {
            return;
        }
        if already_draining {
            return; // an outer drop is already draining; it will take these
        }
        let _ = DRAINING.try_with(|d| d.set(true));
        loop {
            let next = DRAIN.with(|q| q.borrow_mut().pop());
            match next {
                // Dropping this may free more objects, whose children land on
                // the queue rather than on the Rust stack.
                Some(v) => drop(v),
                None => break,
            }
        }
        let _ = DRAINING.try_with(|d| d.set(false));
    }
}

impl GcContents for Vec<Value> {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.append(self);
    }
}

impl GcContents for Vec<(String, Value)> {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.extend(std::mem::take(self).into_iter().map(|(_, v)| v));
    }
}

impl GcContents for Vec<(Value, Value)> {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        for (k, v) in std::mem::take(self) {
            out.push(k);
            out.push(v);
        }
    }
}

impl GcContents for HashMap<String, Value> {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.extend(std::mem::take(self).into_values());
    }
}

impl GcContents for Scope {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.extend(std::mem::take(&mut self.vars).into_values());
        self.parent = None;
    }
}

impl GcContents for Instance {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.append(&mut self.slots);
    }
}

impl GcContents for PromiseState {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        out.push(std::mem::replace(&mut self.value, Value::Null));
        self.take_edges(out);
    }
}

impl GcContents for GenState {
    fn take_children(&mut self, out: &mut Vec<Value>) {
        self.take_coro(out);
    }
}

/// Record a write into an old object. Young objects are traced in full anyway,
/// so only the old generation needs remembering.
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
        fn p<T: GcContents>(w: &Weak<GcCell<T>>) -> Option<usize> {
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

    /// A second handle on the same weak reference (for the cycle scan, which
    /// must not hold `YOUNG` borrowed while it works).
    fn clone_obj(&self) -> Option<GcObj> {
        Some(match self {
            GcObj::Env(w) => GcObj::Env(w.clone()),
            GcObj::Inst(w) => GcObj::Inst(w.clone()),
            GcObj::Arr(w) => GcObj::Arr(w.clone()),
            GcObj::SetV(w) => GcObj::SetV(w.clone()),
            GcObj::Rec(w) => GcObj::Rec(w.clone()),
            GcObj::MapV(w) => GcObj::MapV(w.clone()),
            GcObj::Prom(w) => GcObj::Prom(w.clone()),
            GcObj::Gen(w) => GcObj::Gen(w.clone()),
        })
    }

    /// Upgrade to a strong node for the cycle scan; `None` if refcounting has
    /// already freed it.
    fn node(&self) -> Option<Node> {
        Some(match self {
            GcObj::Env(w) => Node::Env(w.upgrade()?),
            GcObj::Inst(w) => Node::Inst(w.upgrade()?),
            GcObj::Arr(w) | GcObj::SetV(w) => Node::Arr(w.upgrade()?),
            GcObj::Rec(w) => Node::Rec(w.upgrade()?),
            GcObj::MapV(w) => Node::MapV(w.upgrade()?),
            GcObj::Prom(w) => Node::Prom(w.upgrade()?),
            GcObj::Gen(w) => Node::Gen(w.upgrade()?),
        })
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
    static SINCE_GC: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    let crowded = YOUNG.with(|h| {
        let mut h = h.borrow_mut();
        h.push(obj);
        h.len() >= PRUNE_AT.with(|p| p.get())
    });
    SINCE_GC.with(|n| n.set(n.get() + 1));
    if crowded {
        prune();
    }
}

/// Drop the tracking entries of objects refcounting has already freed.
///
/// **This is not a collection, and that is the point.** A `Weak` whose strong
/// count has reached zero refers to an object that is *gone*: its value has been
/// dropped and nothing can ever reach it again. Discarding that entry needs no
/// roots and no trace — which is what makes it safe to run in the middle of a
/// running loop, where a real collection is not. A trace would sweep every
/// object it could not reach from the roots, and the interpreter holds live
/// values in Rust locals that are not roots and cannot be made into them.
///
/// Without this the young list *was* the leak. Every allocation leaves a `Weak`
/// behind, and a `Weak` keeps its allocation alive even after the value inside
/// it is dropped — so the list grew forever and dragged an empty `RcBox` along
/// for each entry. Every `for` body allocates a scope per iteration (that is
/// what gives each iteration its own binding), so *any* long loop grew without
/// bound: 3 million iterations of a loop over two integers held 389 MB, and a
/// 200-million-iteration loop was killed by the OOM killer rather than
/// finishing. Nothing about it was reachable; nothing about it was collected.
fn prune() {
    let live = YOUNG.with(|y| {
        let mut y = y.borrow_mut();
        y.retain(|obj| obj.ptr().is_some());
        y.len()
    });
    // Amortised: scan again only once the list has doubled. A program whose
    // young objects are mostly *live* (building a big structure) would otherwise
    // rescan all of them on every allocation, which is quadratic.
    PRUNE_AT.with(|p| p.set((live * 2).max(threshold())));
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
    SINCE_GC.with(|n| n.get() >= threshold())
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
    SINCE_GC.with(|n| n.set(0));
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
                proms: Vec::new(),
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

            // Drain both worklists: scanning a scope can find a promise, and
            // scanning a promise can find a scope.
            loop {
                if let Some(e) = m.envs.pop() {
                    m.env_contents(&e);
                    continue;
                }
                if let Some(p) = m.proms.pop() {
                    m.promise_contents(&p);
                    continue;
                }
                break;
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
    /// Promises still to scan. A `.then` chain is user data and can be as long
    /// as the program likes, so it is not allowed to become Rust recursion.
    proms: Vec<Rc<GcCell<PromiseState>>>,
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
            self.proms.push(p.clone());
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
        if let Some(g) = &coro.gen {
            if self.enter(Rc::as_ptr(g) as usize) {
                self.gen_contents(g);
            }
        }
        for e in &coro.scopes {
            self.env(e);
        }
        for v in &coro.stack {
            self.value(v);
        }
        // A suspended coroutine's frame holds its slot-resolved locals, and may
        // be the only thing left holding them.
        for v in &coro.frame {
            self.value(v);
        }
        if let Some(c) = &coro.cls {
            self.class(c);
        }
        self.promise(&coro.result);
    }

    fn gen_contents(&mut self, g: &Rc<GcCell<GenState>>) {
        let (saved, pending, adapted) = {
            let st = g.borrow();
            (st.saved(), st.pending_next(), st.adapter_edges())
        };
        // A derived iterator (`it.map(f)`) is kept alive by nothing but the one
        // holding it: the iterator below it and the closure it applies are only
        // reachable through here.
        if let Some((inner, func)) = adapted {
            if self.enter(Rc::as_ptr(&inner) as usize) {
                self.gen_contents(&inner);
            }
            if let Some(f) = func {
                self.value(&f);
            }
        }
        if let Some(coro) = saved {
            self.coro(&coro);
        }
        // An async generator's in-flight `next()` promise: whoever is awaiting
        // it is holding it, but the generator is what will settle it.
        if let Some(p) = pending {
            self.promise(&p);
        }
    }

    /// Mark a value and everything it reaches.
    ///
    /// Iterative, not recursive: an object graph is user data, and a chain half
    /// a million links long is built with an ordinary loop. Recursing over it
    /// would overflow the Rust stack *inside the collector*, which is a process
    /// abort — the crash a program cannot catch (§5.2).
    fn value(&mut self, v: &Value) {
        let mut work: Vec<Value> = vec![v.clone()];
        while let Some(v) = work.pop() {
            self.value_step(&v, &mut work);
        }
    }

    /// Mark `v` itself and push its children onto `work`.
    fn value_step(&mut self, v: &Value, work: &mut Vec<Value>) {
        match v {
            Value::Array(a) | Value::SetV(a) => {
                if self.enter(Rc::as_ptr(a) as usize) {
                    work.extend(a.borrow().iter().cloned());
                }
            }
            Value::Record(r) => {
                if self.enter(Rc::as_ptr(r) as usize) {
                    work.extend(r.borrow().iter().map(|(_, v)| v.clone()));
                }
            }
            Value::MapV(m) => {
                if self.enter(Rc::as_ptr(m) as usize) {
                    for (k, val) in m.borrow().iter() {
                        work.push(k.clone());
                        work.push(val.clone());
                    }
                }
            }
            Value::Instance(i) => {
                if self.enter(Rc::as_ptr(i) as usize) {
                    let (slots, class) = {
                        let inst = i.borrow();
                        (inst.slots.clone(), inst.class.clone())
                    };
                    work.extend(slots);
                    self.class(&class);
                }
            }
            Value::Closure(c) => {
                // Closures are immutable once created, so they are never
                // tracked and never generational: always scan.
                if self.marked.insert(Rc::as_ptr(c) as usize) {
                    self.env(&c.env);
                    if let Some(t) = &c.this {
                        work.push(t.clone());
                    }
                    if let Some(cl) = &c.cls {
                        self.class(cl);
                    }
                }
            }
            Value::Class(c) => self.class(c),
            Value::Namespace(ns) => {
                if self.marked.insert(Rc::as_ptr(ns) as usize) {
                    work.extend(ns.entries.values().cloned());
                }
            }
            // A suspended generator holds a whole coroutine: its saved operand
            // stack can be the only reference to the values it was building.
            Value::IterV(g) => {
                if self.enter(Rc::as_ptr(g) as usize) {
                    self.gen_contents(g);
                }
            }
            Value::PromiseV(p) | Value::Resolve(p) | Value::Reject(p) | Value::PromiseExec(p) => {
                self.promise(p)
            }
            _ => {}
        }
    }
}

// ---- cycle collection without a root set ---------------------------------
//
// A tracing collector needs roots, and the interpreter cannot give it a full
// set: live values sit in Rust locals — an operand stack, a half-built argument
// list, the `Value` a bytecode op just popped — and there is no way to enumerate
// them. That is why `collect` only runs at host boundaries, and why a program
// that stayed inside one long loop could not collect at all. Non-cyclic garbage
// is freed by refcounting regardless, but a cycle (a scope holding a closure
// that captured it — which is every `for` body that makes a closure) survived
// until the loop ended. A long enough loop was killed by the OOM killer.
//
// The way out is to stop asking for roots. Every heap object here is behind an
// `Rc`, so a reference from a Rust local *is already counted*: it is in the
// object's strong count. So liveness can be **derived from the counts**:
//
//     external(X) = strong_count(X) − (references held by other heap objects)
//
// If `external(X) > 0`, something outside the heap graph — a local, a global, a
// module export, the interpreter itself — is holding X, and X is live. Trace
// from those, and whatever is left is a cycle nobody can reach. No roots, no
// enumeration of the interpreter's stack, and therefore safe to run *anywhere*,
// including in the middle of a loop.
//
// The one thing that must be exact is the internal count. Undercounting is safe
// (an object looks externally referenced, so it is kept — a missed cycle, not a
// freed live object). **Overcounting is not**: it would drive `external` to zero
// for a live object and sweep it. So edges are read straight off each object's
// own fields, once per `Rc` handle it actually holds, and nothing is followed
// through an `Rc` this collector does not treat as a node (a `ClassDef`, a
// `Namespace`) — a shared handle reached from two owners would otherwise be
// counted twice.

/// A heap object that can take part in a cycle, held strongly for the scan.
enum Node {
    Env(Env),
    Inst(Rc<GcCell<Instance>>),
    /// Arrays and sets: the same representation.
    Arr(Rc<GcCell<Vec<Value>>>),
    Rec(Rc<GcCell<Vec<(String, Value)>>>),
    MapV(Rc<GcCell<Vec<(Value, Value)>>>),
    Prom(Rc<GcCell<PromiseState>>),
    Gen(Rc<GcCell<GenState>>),
    /// A closure is immutable and never swept, but it *must* be a node: it holds
    /// the scope that captured it, and that edge is exactly the one that closes
    /// the commonest cycle in the language. Leaving closures out would make
    /// every captured scope look externally referenced.
    Fn(Rc<Closure>),
}

use crate::Closure;

impl Node {
    fn ptr(&self) -> usize {
        match self {
            Node::Env(e) => Rc::as_ptr(e) as usize,
            Node::Inst(i) => Rc::as_ptr(i) as usize,
            Node::Arr(a) => Rc::as_ptr(a) as usize,
            Node::Rec(r) => Rc::as_ptr(r) as usize,
            Node::MapV(m) => Rc::as_ptr(m) as usize,
            Node::Prom(p) => Rc::as_ptr(p) as usize,
            Node::Gen(g) => Rc::as_ptr(g) as usize,
            Node::Fn(c) => Rc::as_ptr(c) as usize,
        }
    }

    /// The object's strong count, *not counting the handle this scan holds*.
    fn refs(&self) -> usize {
        let n = match self {
            Node::Env(e) => Rc::strong_count(e),
            Node::Inst(i) => Rc::strong_count(i),
            Node::Arr(a) => Rc::strong_count(a),
            Node::Rec(r) => Rc::strong_count(r),
            Node::MapV(m) => Rc::strong_count(m),
            Node::Prom(p) => Rc::strong_count(p),
            Node::Gen(g) => Rc::strong_count(g),
            Node::Fn(c) => Rc::strong_count(c),
        };
        n - 1 // our own
    }

    /// Every node this one holds a strong reference to, once per handle.
    ///
    /// `None` means the object is borrowed right now (the interpreter is inside
    /// it), so its edges cannot be read. The caller treats that as "live and
    /// untouchable" rather than guessing.
    fn edges(&self, out: &mut Vec<usize>) -> Option<()> {
        match self {
            Node::Env(e) => {
                let s = e.try_borrow()?;
                for v in s.vars.values() {
                    value_edge(v, out);
                }
                if let Some(p) = &s.parent {
                    out.push(Rc::as_ptr(p) as usize);
                }
            }
            Node::Inst(i) => {
                // `class` is deliberately not followed: a `ClassDef` is shared,
                // never swept, and its statics belong to it, not to us.
                for v in &i.try_borrow()?.slots {
                    value_edge(v, out);
                }
            }
            Node::Arr(a) => {
                for v in a.try_borrow()?.iter() {
                    value_edge(v, out);
                }
            }
            Node::Rec(r) => {
                for (_, v) in r.try_borrow()?.iter() {
                    value_edge(v, out);
                }
            }
            Node::MapV(m) => {
                for (k, v) in m.try_borrow()?.iter() {
                    value_edge(k, out);
                    value_edge(v, out);
                }
            }
            Node::Prom(p) => {
                let st = p.try_borrow()?;
                value_edge(&st.value, out);
                for coro in st.waiters() {
                    coro_edges(coro, out);
                }
                for (ok, err, down) in st.reactions() {
                    if let Some(v) = ok {
                        value_edge(v, out);
                    }
                    if let Some(v) = err {
                        value_edge(v, out);
                    }
                    out.push(Rc::as_ptr(down) as usize);
                }
            }
            Node::Gen(g) => {
                let st = g.try_borrow()?;
                if let Some(coro) = st.saved() {
                    coro_edges(&coro, out);
                }
                if let Some(p) = st.pending_next() {
                    out.push(Rc::as_ptr(&p) as usize);
                }
                if let Some((inner, f)) = st.adapter_edges() {
                    out.push(Rc::as_ptr(&inner) as usize);
                    if let Some(v) = &f {
                        value_edge(v, out);
                    }
                }
            }
            Node::Fn(c) => {
                out.push(Rc::as_ptr(&c.env) as usize);
                if let Some(t) = &c.this {
                    value_edge(t, out);
                }
                // `cls` is a shared ClassDef: not a node, not followed.
            }
        }
        Some(())
    }

    /// Break every edge out of this object, collapsing the cycle it sits in.
    /// A closure is immutable, so it has nothing to break — clearing the scope
    /// it captured drops the closure, which is what collapses the cycle.
    fn clear_node(&self) {
        match self {
            Node::Env(e) => {
                if let Some(mut s) = e.try_borrow_mut() {
                    s.vars.clear();
                    s.parent = None;
                }
            }
            Node::Inst(i) => {
                if let Some(mut b) = i.try_borrow_mut() {
                    b.slots.clear();
                }
            }
            Node::Arr(a) => {
                if let Some(mut b) = a.try_borrow_mut() {
                    b.clear();
                }
            }
            Node::Rec(r) => {
                if let Some(mut b) = r.try_borrow_mut() {
                    b.clear();
                }
            }
            Node::MapV(m) => {
                if let Some(mut b) = m.try_borrow_mut() {
                    b.clear();
                }
            }
            Node::Prom(p) => {
                if let Some(mut b) = p.try_borrow_mut() {
                    b.clear_edges();
                }
            }
            Node::Gen(g) => {
                if let Some(mut b) = g.try_borrow_mut() {
                    b.discard();
                }
            }
            Node::Fn(_) => {}
        }
    }
}

/// The node a value points at, if any — one push per `Rc` handle it holds.
///
/// `Class` and `Namespace` are absent on purpose: they are shared, never swept,
/// and following them would let one object's references be charged to another.
fn value_edge(v: &Value, out: &mut Vec<usize>) {
    match v {
        Value::Array(a) | Value::SetV(a) => out.push(Rc::as_ptr(a) as usize),
        Value::Record(r) => out.push(Rc::as_ptr(r) as usize),
        Value::MapV(m) => out.push(Rc::as_ptr(m) as usize),
        Value::Instance(i) => out.push(Rc::as_ptr(i) as usize),
        Value::Closure(c) => out.push(Rc::as_ptr(c) as usize),
        Value::IterV(g) => out.push(Rc::as_ptr(g) as usize),
        Value::PromiseV(p) | Value::Resolve(p) | Value::Reject(p) | Value::PromiseExec(p) => {
            out.push(Rc::as_ptr(p) as usize)
        }
        _ => {}
    }
}

fn coro_edges(coro: &Coro, out: &mut Vec<usize>) {
    if let Some(g) = &coro.gen {
        out.push(Rc::as_ptr(g) as usize);
    }
    for e in &coro.scopes {
        out.push(Rc::as_ptr(e) as usize);
    }
    for v in &coro.stack {
        value_edge(v, out);
    }
    for v in &coro.frame {
        value_edge(v, out);
    }
    out.push(Rc::as_ptr(&coro.result) as usize);
}

/// Closures a value points at, so they can be added to the node set.
fn value_closures(v: &Value, out: &mut Vec<Rc<Closure>>) {
    if let Value::Closure(c) = v {
        out.push(c.clone());
    }
}

/// Collect cycles using nothing but the reference counts. Returns how many
/// objects were freed. Safe to call at any point where no `GcCell` is borrowed.
pub(crate) fn collect_cycles() -> usize {
    let (nodes, _index, live) = analyse();
    let mut freed = 0;
    for (i, n) in nodes.iter().enumerate() {
        if !live[i] {
            n.clear_node();
            freed += 1;
        }
    }
    let survivors = nodes.len() - freed;
    drop(nodes); // refcounting now frees the collapsed cycles
    prune();
    SINCE_GC.with(|n| n.set(0));
    CYCLE_AT.with(|c| c.set(survivors.max(threshold())));
    freed
}

/// The soundness property, checkable: **every object the tracing collector can
/// reach from the real roots must be considered live by the reference-count
/// analysis.** If one is not, an internal edge has been counted twice somewhere
/// and the cycle collector would sweep an object that is still in use.
///
/// The two collectors decide liveness by completely different means — one walks
/// from a root set, the other subtracts internal references from strong counts —
/// so agreement between them is real evidence rather than a restatement.
pub(crate) fn verify_cycles(roots: &Roots) -> Result<(), String> {
    let (nodes, index, live) = analyse();
    let reachable = mark(roots, false);
    for p in &reachable {
        if let Some(&i) = index.get(p) {
            if !live[i] {
                return Err(format!(
                    "object {p:#x} is reachable from the roots, but the reference-count \
                     analysis considers it garbage: an internal edge has been overcounted"
                ));
            }
        }
    }
    drop(nodes);
    Ok(())
}

#[allow(clippy::type_complexity)]
fn analyse() -> (Vec<Node>, HashMap<usize, usize>, Vec<bool>) {
    // 1. Every tracked object, held strongly for the duration.
    let mut nodes: Vec<Node> = Vec::new();
    let mut index: HashMap<usize, usize> = HashMap::new();
    let add = |n: Node, nodes: &mut Vec<Node>, index: &mut HashMap<usize, usize>| {
        let p = n.ptr();
        if let std::collections::hash_map::Entry::Vacant(e) = index.entry(p) {
            e.insert(nodes.len());
            nodes.push(n);
        }
    };
    let tracked: Vec<GcObj> = YOUNG.with(|y| {
        OLD.with(|o| {
            let mut v: Vec<GcObj> = y.borrow().iter().filter_map(|g| g.clone_obj()).collect();
            v.extend(o.borrow().values().filter_map(|g| g.clone_obj()));
            v
        })
    });
    for obj in &tracked {
        if let Some(n) = obj.node() {
            add(n, &mut nodes, &mut index);
        }
    }

    // 2. Closures reachable from those objects are nodes too — they hold the
    //    scopes that captured them. A closure held *only* by something outside
    //    the tracked heap is never discovered, and the scope it captured then
    //    looks externally referenced: a missed cycle, which is the safe way to
    //    be wrong.
    let mut scan_from = 0usize;
    loop {
        let mut found: Vec<Rc<Closure>> = Vec::new();
        for n in &nodes[scan_from..] {
            let mut vals: Vec<Value> = Vec::new();
            node_values(n, &mut vals);
            for v in &vals {
                value_closures(v, &mut found);
            }
        }
        scan_from = nodes.len();
        if found.is_empty() {
            break;
        }
        for c in found {
            add(Node::Fn(c), &mut nodes, &mut index);
        }
        if scan_from == nodes.len() {
            break;
        }
    }

    // 3. Edges, and the objects whose edges cannot be read because the
    //    interpreter is inside them right now.
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(nodes.len());
    let mut pinned: Vec<bool> = vec![false; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        let mut e = Vec::new();
        if n.edges(&mut e).is_none() {
            pinned[i] = true;
            e.clear();
        }
        edges.push(e);
    }

    // 4. Internal references, then the ones the counts say come from outside.
    let mut internal: Vec<usize> = vec![0; nodes.len()];
    for e in &edges {
        for p in e {
            if let Some(&j) = index.get(p) {
                internal[j] += 1;
            }
        }
    }

    // 5. Anything the heap does not fully account for is held by something the
    //    heap cannot see — a Rust local, a global, the interpreter — and is live.
    let mut live: Vec<bool> = vec![false; nodes.len()];
    let mut work: Vec<usize> = Vec::new();
    for i in 0..nodes.len() {
        if pinned[i] || nodes[i].refs() > internal[i] {
            live[i] = true;
            work.push(i);
        }
    }
    while let Some(i) = work.pop() {
        for p in &edges[i] {
            if let Some(&j) = index.get(p) {
                if !live[j] {
                    live[j] = true;
                    work.push(j);
                }
            }
        }
    }

    // 6. Whatever is left is unreachable from anywhere at all.
    (nodes, index, live)
}

/// Every value a node holds, for the closure hunt in step 2.
fn node_values(n: &Node, out: &mut Vec<Value>) {
    match n {
        Node::Env(e) => {
            if let Some(s) = e.try_borrow() {
                out.extend(s.vars.values().cloned());
            }
        }
        Node::Inst(i) => {
            if let Some(b) = i.try_borrow() {
                out.extend(b.slots.iter().cloned());
            }
        }
        Node::Arr(a) => {
            if let Some(b) = a.try_borrow() {
                out.extend(b.iter().cloned());
            }
        }
        Node::Rec(r) => {
            if let Some(b) = r.try_borrow() {
                out.extend(b.iter().map(|(_, v)| v.clone()));
            }
        }
        Node::MapV(m) => {
            if let Some(b) = m.try_borrow() {
                for (k, v) in b.iter() {
                    out.push(k.clone());
                    out.push(v.clone());
                }
            }
        }
        Node::Prom(p) => {
            if let Some(st) = p.try_borrow() {
                out.push(st.value.clone());
                for coro in st.waiters() {
                    out.extend(coro.stack.iter().cloned());
                    out.extend(coro.frame.iter().cloned());
                }
                for (ok, err, _) in st.reactions() {
                    out.extend(ok.iter().cloned());
                    out.extend(err.iter().cloned());
                }
            }
        }
        Node::Gen(g) => {
            if let Some(st) = g.try_borrow() {
                if let Some(coro) = st.saved() {
                    out.extend(coro.stack);
                    out.extend(coro.frame);
                }
                if let Some((_, Some(f))) = st.adapter_edges() {
                    out.push(f);
                }
            }
        }
        Node::Fn(c) => {
            if let Some(t) = &c.this {
                out.push(t.clone());
            }
        }
    }
}

/// Has enough been allocated since the last cycle collection to be worth one?
///
/// The target grows with the live heap: a scan is O(heap), so a program with a
/// large live heap must be allowed to allocate proportionally more before paying
/// for another one.
pub(crate) fn should_collect_cycles() -> bool {
    SINCE_GC.with(|n| n.get() >= CYCLE_AT.with(|c| c.get()))
}
