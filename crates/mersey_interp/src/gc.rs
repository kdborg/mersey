//! Cycle collector.
//!
//! The heap is reference-counted (`Rc`), which frees acyclic garbage
//! immediately but leaks cycles — and the most ordinary Mersey object graph
//! is cyclic: an instance holds a closure, the closure holds the scope, the
//! scope holds the instance. This is a mark–sweep collector for exactly that.
//!
//! **Safety model.** Collection only runs at a *host boundary* — after a
//! module finishes, or after a callback and its microtasks drain. At those
//! points no VM operand stack is live on the Rust stack, so the roots really
//! are the roots: module scopes, exports, registered callbacks, pending
//! tasks, and the class stack. `gc.collect()` from Mersey therefore *requests*
//! a collection (a flag) rather than performing one mid-expression, which
//! would be able to sweep objects held only by live frames.
//!
//! Sweeping does not free memory directly (the `Rc`s may still be pointed at
//! by the very cycle we are breaking): it *clears* an unreachable object's
//! contents, which drops the edges, which drops the refcounts to zero.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use crate::{ClassDef, Coro, Env, Instance, PromiseState, Scope, Value};

/// Every collectable object, weakly held.
pub(crate) enum GcObj {
    Env(Weak<RefCell<Scope>>),
    Inst(Weak<RefCell<Instance>>),
    Arr(Weak<RefCell<Vec<Value>>>),
    Rec(Weak<RefCell<Vec<(String, Value)>>>),
    MapV(Weak<RefCell<Vec<(Value, Value)>>>),
    SetV(Weak<RefCell<Vec<Value>>>),
}

thread_local! {
    /// Registry of live collectable objects (single-threaded engine).
    static HEAP: RefCell<Vec<GcObj>> = const { RefCell::new(Vec::new()) };
    /// Objects registered since the last collection.
    static SINCE_GC: RefCell<usize> = const { RefCell::new(0) };
}

/// Collect when this many objects have been registered since the last run.
const THRESHOLD: usize = 20_000;

fn register(obj: GcObj) {
    HEAP.with(|h| h.borrow_mut().push(obj));
    SINCE_GC.with(|n| *n.borrow_mut() += 1);
}

pub(crate) fn track_env(e: &Env) {
    register(GcObj::Env(Rc::downgrade(e)));
}
pub(crate) fn track_instance(i: &Rc<RefCell<Instance>>) {
    register(GcObj::Inst(Rc::downgrade(i)));
}
pub(crate) fn track_array(a: &Rc<RefCell<Vec<Value>>>) {
    register(GcObj::Arr(Rc::downgrade(a)));
}
pub(crate) fn track_record(r: &Rc<RefCell<Vec<(String, Value)>>>) {
    register(GcObj::Rec(Rc::downgrade(r)));
}
pub(crate) fn track_map(m: &Rc<RefCell<Vec<(Value, Value)>>>) {
    register(GcObj::MapV(Rc::downgrade(m)));
}
pub(crate) fn track_set(s: &Rc<RefCell<Vec<Value>>>) {
    register(GcObj::SetV(Rc::downgrade(s)));
}

/// Has enough been allocated to be worth a collection?
pub(crate) fn should_collect() -> bool {
    SINCE_GC.with(|n| *n.borrow() >= THRESHOLD)
}

/// Report the heap WITHOUT sweeping. Safe to call anywhere: a real
/// collection mid-expression could sweep objects held only by live VM
/// frames, which are not roots.
pub(crate) fn stats_only() -> GcStats {
    HEAP.with(|h| {
        let mut heap = h.borrow_mut();
        heap.retain(|obj| match obj {
            GcObj::Env(w) => w.strong_count() > 0,
            GcObj::Inst(w) => w.strong_count() > 0,
            GcObj::Arr(w) | GcObj::SetV(w) => w.strong_count() > 0,
            GcObj::Rec(w) => w.strong_count() > 0,
            GcObj::MapV(w) => w.strong_count() > 0,
        });
        GcStats { tracked: heap.len(), reachable: heap.len(), collected: 0 }
    })
}

pub struct GcStats {
    pub tracked: usize,
    pub reachable: usize,
    pub collected: usize,
}

/// Mark from `roots`, then clear everything unreachable.
pub(crate) fn collect(roots: &Roots) -> GcStats {
    let mut marked: HashSet<usize> = HashSet::new();
    let mut env_stack: Vec<Env> = Vec::new();

    for v in &roots.values {
        mark_value(v, &mut marked, &mut env_stack);
    }
    for e in &roots.envs {
        mark_env(e, &mut marked, &mut env_stack);
    }
    for c in &roots.coros {
        mark_coro(c, &mut marked, &mut env_stack);
    }
    for c in &roots.classes {
        mark_class(c, &mut marked, &mut env_stack);
    }
    while let Some(e) = env_stack.pop() {
        mark_env_contents(&e, &mut marked, &mut env_stack);
    }

    let mut collected = 0;
    let mut reachable = 0;
    HEAP.with(|h| {
        let mut heap = h.borrow_mut();
        heap.retain(|obj| match obj {
            GcObj::Env(w) => match w.upgrade() {
                Some(rc) => {
                    if marked.contains(&(Rc::as_ptr(&rc) as usize)) {
                        reachable += 1;
                        true
                    } else {
                        // Break the cycle: drop this scope's bindings.
                        rc.borrow_mut().vars.clear();
                        rc.borrow_mut().parent = None;
                        collected += 1;
                        false
                    }
                }
                None => false, // already freed by refcounting
            },
            GcObj::Inst(w) => match w.upgrade() {
                Some(rc) => {
                    if marked.contains(&(Rc::as_ptr(&rc) as usize)) {
                        reachable += 1;
                        true
                    } else {
                        rc.borrow_mut().slots.clear();
                        collected += 1;
                        false
                    }
                }
                None => false,
            },
            GcObj::Arr(w) | GcObj::SetV(w) => match w.upgrade() {
                Some(rc) => {
                    if marked.contains(&(Rc::as_ptr(&rc) as usize)) {
                        reachable += 1;
                        true
                    } else {
                        rc.borrow_mut().clear();
                        collected += 1;
                        false
                    }
                }
                None => false,
            },
            GcObj::Rec(w) => match w.upgrade() {
                Some(rc) => {
                    if marked.contains(&(Rc::as_ptr(&rc) as usize)) {
                        reachable += 1;
                        true
                    } else {
                        rc.borrow_mut().clear();
                        collected += 1;
                        false
                    }
                }
                None => false,
            },
            GcObj::MapV(w) => match w.upgrade() {
                Some(rc) => {
                    if marked.contains(&(Rc::as_ptr(&rc) as usize)) {
                        reachable += 1;
                        true
                    } else {
                        rc.borrow_mut().clear();
                        collected += 1;
                        false
                    }
                }
                None => false,
            },
        });
        SINCE_GC.with(|n| *n.borrow_mut() = 0);
        GcStats { tracked: heap.len(), reachable, collected }
    })
}

/// What the engine considers live at a safe point.
#[derive(Default)]
pub(crate) struct Roots {
    pub values: Vec<Value>,
    pub envs: Vec<Env>,
    pub coros: Vec<Rc<RefCell<PromiseState>>>,
    pub classes: Vec<Rc<ClassDef>>,
}

fn mark_ptr(p: usize, marked: &mut HashSet<usize>) -> bool {
    marked.insert(p)
}

fn mark_env(e: &Env, marked: &mut HashSet<usize>, stack: &mut Vec<Env>) {
    if mark_ptr(Rc::as_ptr(e) as usize, marked) {
        stack.push(e.clone());
    }
}

fn mark_env_contents(e: &Env, marked: &mut HashSet<usize>, stack: &mut Vec<Env>) {
    let (vars, parent) = {
        let s = e.borrow();
        (s.vars.values().cloned().collect::<Vec<Value>>(), s.parent.clone())
    };
    for v in &vars {
        mark_value(v, marked, stack);
    }
    if let Some(p) = parent {
        mark_env(&p, marked, stack);
    }
}

fn mark_class(c: &Rc<ClassDef>, marked: &mut HashSet<usize>, stack: &mut Vec<Env>) {
    if !mark_ptr(Rc::as_ptr(c) as usize, marked) {
        return;
    }
    if let Some(e) = &c.env {
        mark_env(e, marked, stack);
    }
    let statics: Vec<Value> = c.statics.borrow().values().cloned().collect();
    for v in &statics {
        mark_value(v, marked, stack);
    }
    if let Some(p) = &c.parent {
        mark_class(p, marked, stack);
    }
}

fn mark_promise(
    p: &Rc<RefCell<PromiseState>>,
    marked: &mut HashSet<usize>,
    stack: &mut Vec<Env>,
) {
    if !mark_ptr(Rc::as_ptr(p) as usize, marked) {
        return;
    }
    mark_coro(p, marked, stack);
}

fn mark_coro(p: &Rc<RefCell<PromiseState>>, marked: &mut HashSet<usize>, stack: &mut Vec<Env>) {
    let st = p.borrow();
    mark_value(&st.value, marked, stack);
    for coro in st.waiters() {
        for e in &coro.scopes {
            mark_env(e, marked, stack);
        }
        for v in &coro.stack {
            mark_value(v, marked, stack);
        }
        if let Some(c) = &coro.cls {
            mark_class(c, marked, stack);
        }
        mark_promise_weak(&coro.result, marked, stack);
    }
    for (ok, err, down) in st.reactions() {
        if let Some(v) = ok {
            mark_value(v, marked, stack);
        }
        if let Some(v) = err {
            mark_value(v, marked, stack);
        }
        mark_promise_weak(down, marked, stack);
    }
}

fn mark_promise_weak(
    p: &Rc<RefCell<PromiseState>>,
    marked: &mut HashSet<usize>,
    stack: &mut Vec<Env>,
) {
    if mark_ptr(Rc::as_ptr(p) as usize, marked) {
        let value = p.borrow().value.clone();
        mark_value(&value, marked, stack);
    }
}

fn mark_value(v: &Value, marked: &mut HashSet<usize>, stack: &mut Vec<Env>) {
    match v {
        Value::Array(a) | Value::SetV(a) => {
            if mark_ptr(Rc::as_ptr(a) as usize, marked) {
                let items = a.borrow().clone();
                for item in &items {
                    mark_value(item, marked, stack);
                }
            }
        }
        Value::Record(r) => {
            if mark_ptr(Rc::as_ptr(r) as usize, marked) {
                let items = r.borrow().clone();
                for (_, item) in &items {
                    mark_value(item, marked, stack);
                }
            }
        }
        Value::MapV(m) => {
            if mark_ptr(Rc::as_ptr(m) as usize, marked) {
                let items = m.borrow().clone();
                for (k, val) in &items {
                    mark_value(k, marked, stack);
                    mark_value(val, marked, stack);
                }
            }
        }
        Value::Instance(i) => {
            if mark_ptr(Rc::as_ptr(i) as usize, marked) {
                let (fields, class) = {
                    let inst = i.borrow();
                    (inst.slots.clone(), inst.class.clone())
                };
                for f in &fields {
                    mark_value(f, marked, stack);
                }
                mark_class(&class, marked, stack);
            }
        }
        Value::Closure(c) => {
            if mark_ptr(Rc::as_ptr(c) as usize, marked) {
                mark_env(&c.env, marked, stack);
                if let Some(t) = &c.this {
                    mark_value(t, marked, stack);
                }
                if let Some(cl) = &c.cls {
                    mark_class(cl, marked, stack);
                }
            }
        }
        Value::Class(c) => mark_class(c, marked, stack),
        Value::Namespace(ns) => {
            if mark_ptr(Rc::as_ptr(ns) as usize, marked) {
                let entries: Vec<Value> = ns.entries.values().cloned().collect();
                for v in &entries {
                    mark_value(v, marked, stack);
                }
            }
        }
        Value::PromiseV(p) | Value::Resolver(p, _) | Value::PromiseExec(p) => {
            mark_promise(p, marked, stack)
        }
        _ => {}
    }
}
