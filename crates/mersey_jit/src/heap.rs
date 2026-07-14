//! The heap, as compiled code reaches it.
//!
//! This is the only place in the engine that turns a raw address back into an
//! object, and it is deliberately three functions long.
//!
//! **Compiled code never touches a reference count.** It reads scalars out of
//! heap cells and writes scalars back into them; an object or an array it holds
//! is a *borrowed* address, never a new reference and never one it stores. Three
//! things follow, and together they are the whole safety argument for Tier 1's
//! heap:
//!
//! 1. It cannot free anything, so nothing it holds an address of can go away
//!    while it runs.
//! 2. It cannot allocate, so the collector cannot run underneath it. (It could
//!    not move anything if it did — the heap does not move, which was the
//!    decision that made all of this available.)
//! 3. It creates no edges between objects, so the cycle collector's graph is
//!    exactly what it was before the call. No write barrier, nothing to root.
//!
//! The caller — the interpreter — owns the `Rc`s and outlives the call.

use std::rc::Rc;

use mersey_interp::{
    alloc_instance, array_data, gc::GcCell, instance_slots, Arena, ClassDef, Instance, Value,
};

/// Where an instance's fields live. Null in, null out: compiled code checks for
/// a null receiver before it uses this, and a null object must not be a deref.
///
/// # Safety
/// `p` is either null or an address the engine handed us for a live instance,
/// which the caller holds an `Rc` to for the duration of the call.
pub(crate) unsafe extern "C" fn inst_slots(p: *const GcCell<Instance>) -> *mut Value {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    instance_slots(unsafe { &*p }).unwrap_or(std::ptr::null_mut())
}

/// Where an array's elements live.
///
/// # Safety
/// As `inst_slots`.
pub(crate) unsafe extern "C" fn arr_data(p: *const GcCell<Vec<Value>>) -> *mut Value {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    match array_data(unsafe { &*p }) {
        Some((d, _)) => d,
        None => std::ptr::null_mut(),
    }
}

/// How many elements it has. `-1` for a null array, which compiled code turns
/// into the same `TypeError` the interpreter raises — rather than a bounds check
/// that happens to fail.
///
/// # Safety
/// As `inst_slots`.
pub(crate) unsafe extern "C" fn arr_len(p: *const GcCell<Vec<Value>>) -> i64 {
    if p.is_null() {
        return -1;
    }
    match array_data(unsafe { &*p }) {
        Some((_, n)) => n as i64,
        None => -1,
    }
}

/// The object a heap cell holds: its address, and the address of its fields.
///
/// Compiled code hands over the *cell* — `object + slot * 16`, or `elements +
/// i * 16` — and not the pointer inside it, because the pointer inside it is not
/// the pointer anyone wants. An `Rc`'s word is the address of the **box**, with
/// the reference counts in front of the value; `Rc::as_ptr` is the address of the
/// value. Compiled code that read the word and passed it here would be handing
/// over a different pointer from the one the interpreter marshals for the same
/// object, and only one of them would be right.
///
/// So the pointer is never *taken* from the cell by compiled code. It is asked
/// for, here, where `Rc` can be asked properly and the answer is the same one the
/// interpreter gives. `out` receives (address, fields); a cell holding `null` — or
/// anything that is not an instance, which the type system says it cannot be —
/// gives (0, 0), and every use checks for that first.
///
/// # Safety
/// `cell` is the address of a live `Value` inside an object or array the caller
/// holds an `Rc` to.
pub(crate) unsafe extern "C" fn cell_obj(cell: *const Value, out: *mut u64) {
    let (p, base) = match unsafe { &*cell } {
        Value::Instance(rc) => {
            let p = Rc::as_ptr(rc);
            (p as u64, instance_slots(rc).unwrap_or(std::ptr::null_mut()))
        }
        _ => (0, std::ptr::null_mut()),
    };
    unsafe {
        *out = p;
        *out.add(1) = base as u64;
    }
}

/// The array a heap cell holds: its address, its elements, and how many. As
/// `cell_obj`, and for the same reason.
///
/// # Safety
/// As `cell_obj`.
pub(crate) unsafe extern "C" fn cell_arr(cell: *const Value, out: *mut u64) {
    let (p, data, len) = match unsafe { &*cell } {
        Value::Array(rc) => {
            let p = Rc::as_ptr(rc) as u64;
            match array_data(rc) {
                Some((d, n)) => (p, d, n as i64),
                None => (p, std::ptr::null_mut(), -1),
            }
        }
        _ => (0, std::ptr::null_mut(), -1),
    };
    unsafe {
        *out = p;
        *out.add(1) = data as u64;
        *out.add(2) = len as u64;
    }
}

/// Allocate an instance of `cls`, owned by the arena.
///
/// This is `new`, minus the constructor (which is compiled code's next call):
/// the instance, its folded field values, its fresh containers, its GC
/// registration. The arena keeps the `Rc`; what compiled code gets is the
/// address, the address of the fields, and the arena handle that names — and is
/// the only way to let go of — the reference. `out` receives those three.
///
/// # Safety
/// `cls` is a class pointer baked into the code at compile time, kept alive by
/// the `JitCode` that owns it. `arena` is the live arena of the current call.
pub(crate) unsafe extern "C" fn alloc(cls: *const ClassDef, arena: *mut Arena, out: *mut u64) {
    // The Rc this pointer came from is held in JitCode::classes; cloning it here
    // needs the strong count bumped first, exactly like `cell_obj`'s clone.
    unsafe { Rc::increment_strong_count(cls) };
    let cls = unsafe { Rc::from_raw(cls) };
    let v = alloc_instance(&cls);
    let (p, base) = match &v {
        Value::Instance(rc) => (
            Rc::as_ptr(rc) as u64,
            instance_slots(rc).unwrap_or(std::ptr::null_mut()) as u64,
        ),
        _ => (0, 0),
    };
    let h = unsafe { &mut *arena }.keep(v);
    unsafe {
        *out = p;
        *out.add(1) = base;
        *out.add(2) = h;
    }
}

/// Take an arena-owned reference to an object compiled code only borrows — the
/// step that makes a stored copy survive the original's release, and a returned
/// borrow safe to hand to the caller.
///
/// # Safety
/// `p` is the address of a live instance: it came from `Rc::as_ptr` of an `Rc`
/// that is alive right now (an argument, a field, an arena slot), which is
/// exactly the situation `Rc::increment_strong_count` exists for.
pub(crate) unsafe extern "C" fn clone_obj(p: *const GcCell<Instance>, arena: *mut Arena) -> u64 {
    if p.is_null() {
        return 0;
    }
    unsafe { Rc::increment_strong_count(p) };
    let rc = unsafe { Rc::from_raw(p) };
    unsafe { &mut *arena }.keep(Value::Instance(rc))
}

/// Let go of an arena reference: what overwriting an object local does with the
/// old value. Releasing handle 0 — a borrow — is a no-op, everywhere.
///
/// # Safety
/// `arena` is the live arena of the current call.
pub(crate) unsafe extern "C" fn release(arena: *mut Arena, h: u64) {
    unsafe { &mut *arena }.release(h);
}

/// Prove the layout compiled code is about to assume, against a real value.
///
/// Compiled code loads a `float64` field with a single instruction, at
/// `object + slot * 16 + 8`. Those numbers come from `Value`'s `repr(u8)`, which
/// the language guarantees — but a guarantee that is never checked is a comment,
/// and getting this wrong would not fail to compile: it would read the wrong
/// bytes and keep going. So it is checked, once, before any heap-touching code is
/// emitted, and if it is ever false the compiler simply declines to compile the
/// heap.
pub(crate) fn layout_holds() -> bool {
    use mersey_interp::repr;
    if std::mem::size_of::<Value>() != repr::SIZE || std::mem::align_of::<Value>() != 8 {
        return false;
    }
    // The tag byte at offset 0, and each payload at *its own* offset — which is
    // the part that is easy to get wrong, and did get wrong: `repr(u8)` aligns a
    // payload to itself, so a `float64` is at 8 and an `int32` is at 4. Only the
    // bytes the payload occupies are compared; the rest is padding, and padding
    // holds nothing anyone may read.
    let ok = |v: Value, tag: u8, at: i32, payload: &[u8]| -> bool {
        let raw: &[u8] =
            unsafe { std::slice::from_raw_parts(&v as *const Value as *const u8, repr::SIZE) };
        let at = at as usize;
        raw[0] == tag && raw[at..at + payload.len()] == *payload
    };
    ok(
        Value::F64(1.5),
        repr::TAG_F64,
        repr::OFF_F64,
        &1.5f64.to_ne_bytes(),
    ) && ok(
        Value::I64(-7),
        repr::TAG_I64,
        repr::OFF_I64,
        &(-7i64).to_ne_bytes(),
    ) && ok(
        Value::I32(-7),
        repr::TAG_I32,
        repr::OFF_I32,
        &(-7i32).to_ne_bytes(),
    ) && ok(Value::Bool(true), repr::TAG_BOOL, repr::OFF_BOOL, &[1u8])
        && ok(Value::Null, repr::TAG_NULL, 0, &[])
}

#[cfg(test)]
mod tests {
    /// The compiler emits `object + slot * 16 + offset` and reads the bytes it
    /// finds. If this is ever false, that arithmetic is reading something else.
    #[test]
    fn value_layout_is_what_compiled_code_assumes() {
        assert!(super::layout_holds());
    }
}
