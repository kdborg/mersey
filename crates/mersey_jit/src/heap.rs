// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

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
    alloc_instance, append_int_utf16, array_data, gc::GcCell, instance_slots, Arena, ClassDef,
    Instance, Value, WebArg, WebReplyRaw,
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

/// A string slot's data pointer and length, derived at entry from the address
/// the frame holds — `Rc::as_ptr` of an `Rc<Vec<u16>>`, i.e. a `*const Vec<u16>`.
/// The wrapper does this once, exactly as it derives an object's fields from its
/// address, rather than passing the length through the (two-cell) frame. `out`
/// receives (data pointer, length); a null address gives (0, 0).
///
/// # Safety
/// `p` is null or the address of a live `Vec<u16>` the caller (the arena, after
/// the OSR clone) holds a reference to for the call.
pub(crate) unsafe extern "C" fn str_vec_parts(p: *const Vec<u16>, out: *mut u64) {
    let (data, len) = if p.is_null() {
        (0u64, 0u64)
    } else {
        let v: &Vec<u16> = unsafe { &*p };
        (v.as_ptr() as u64, v.len() as u64)
    };
    unsafe {
        *out = data;
        *out.add(1) = len;
    }
}

/// One part of a template to join. `kind` 0 is a string (`a` = UTF-16 data
/// pointer, `b` = length); `kind` 1 is an integer (`a` = the value). Laid out to
/// match what the codegen writes into a stack slot, one `StrPart` per template
/// part.
#[repr(C)]
pub(crate) struct StrPart {
    pub kind: i64,
    pub a: i64,
    pub b: i64,
}

/// Build one string from `n` template parts and hand it to the arena — the
/// compiled form of `TemplateJoin`. This *allocates* (a `Vec<u16>` and its `Rc`),
/// which almost nothing compiled does; it is sound for the same reason `alloc`
/// is: the arena owns the result, the handle names it, and the sweep at the end
/// of the call frees it if compiled code never stored it anywhere lasting. A
/// string is not GC-tracked (it forms no cycles), so this creates no edge the
/// cycle collector could see — the collector's graph is unchanged.
///
/// Integers are formatted exactly as the VM's `TemplateJoin` (`append_int_utf16`),
/// so the compiled and interpreted results are byte-identical. `out` receives
/// (data pointer, length, arena handle).
///
/// # Safety
/// `parts` points at `n` valid `StrPart`s; each string part's `(a, b)` is a live
/// UTF-16 slice for the call. `arena` is the current call's live arena.
pub(crate) unsafe extern "C" fn str_join(
    arena: *mut Arena,
    parts: *const StrPart,
    n: usize,
    out: *mut u64,
) {
    let parts = unsafe { std::slice::from_raw_parts(parts, n) };
    // Sized once, from the parts. `Vec::new()` starts empty and grew as they
    // arrived, so a template of a literal, a number and another literal cost two
    // or three reallocations and as many memmoves — on every evaluation.
    // `RawVecInner::finish_grow` was 8% of a compiled `encoding` iteration.
    // An integer part is counted at its widest (an i64 in decimal, sign
    // included), which over-reserves a few units rather than ever growing.
    let cap: usize = parts
        .iter()
        .map(|p| match p.kind {
            0 => p.b as usize,
            1 => 20,
            _ => 0,
        })
        .sum();
    let mut buf: Vec<u16> = Vec::with_capacity(cap);
    for p in parts {
        match p.kind {
            0 => {
                let s = unsafe { std::slice::from_raw_parts(p.a as *const u16, p.b as usize) };
                buf.extend_from_slice(s);
            }
            1 => append_int_utf16(&mut buf, p.a),
            _ => {}
        }
    }
    let rc = std::rc::Rc::new(buf);
    let ptr = rc.as_ptr() as u64;
    let len = rc.len() as u64;
    let handle = unsafe { &mut *arena }.keep(Value::Str(rc));
    unsafe {
        *out = ptr;
        *out.add(1) = len;
        *out.add(2) = handle;
    }
}

/// The string a heap cell holds: its UTF-16 data pointer and its length in code
/// units. Unlike an array or instance a string is a plain `Rc<Vec<u16>>` with no
/// GC cell, so the data address is read directly. A borrowed const string: its
/// buffer lives in the chunk's const pool, which outlives the call. `out`
/// receives (data, len); a cell that is not a string gives (0, 0).
///
/// # Safety
/// As `cell_arr`.
pub(crate) unsafe extern "C" fn cell_str(cell: *const Value, out: *mut u64) {
    let (ptr, len) = match unsafe { &*cell } {
        Value::Str(rc) => {
            let s: &[u16] = rc;
            (s.as_ptr() as u64, s.len() as u64)
        }
        _ => (0, 0),
    };
    unsafe {
        *out = ptr;
        *out.add(1) = len;
    }
}

/// The opaque a heap cell holds (`this.u`), as an arena handle — a `Bytes`, a
/// `Url`. There is no representation for one but an arena entry, so unlike a
/// string field this is *owned*: whoever takes it releases it.  0 for null.
///
/// # Safety
/// As `cell_obj`.
pub(crate) unsafe extern "C" fn cell_val(cell: *const Value, arena: *mut Arena) -> u64 {
    unsafe {
        match &*cell {
            Value::Null => 0,
            v => (*arena).keep(v.clone()),
        }
    }
}

/// Write one into a heap cell (`this.u = parsed`). Assigning over the cell drops
/// whatever it held.
///
/// # Safety
/// As `cell_set_str`.
pub(crate) unsafe extern "C" fn cell_set_val(cell: *mut Value, arena: *mut Arena, h: u64) {
    let v = unsafe { (*arena).get(h).cloned().unwrap_or(Value::Null) };
    unsafe { *cell = v };
}

/// Write a string into a heap cell (`this.name = s`). The units are copied into a
/// fresh `Value::Str`, which is what the field will own; assigning over the cell
/// drops whatever it held.
///
/// # Safety
/// `cell` addresses a live `Value` inside an object the caller holds an `Rc` to,
/// and `ptr`/`len` name a valid UTF-16 slice — or `ptr` is null for `null`.
pub(crate) unsafe extern "C" fn cell_set_str(cell: *mut Value, ptr: *const u16, len: usize) {
    let v = if ptr.is_null() {
        Value::Null
    } else {
        let units = unsafe { std::slice::from_raw_parts(ptr, len) };
        Value::Str(std::rc::Rc::new(units.to_vec()))
    };
    unsafe { *cell = v };
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

/// The host-call shims all reach the interpreter through the pointer it set on
/// the arena for exactly this call (`interp_ptr`).
///
/// # Safety
/// `arena` must point at the live `Arena` the current compiled call was entered
/// with; the interpreter sets `interp_ptr` before entry and clears it after, and
/// the interpreter frame that made the call is suspended, so a reentrant
/// `&mut Interp` here does not alias a live one.
pub(crate) unsafe extern "C" fn host_time(arena: *mut Arena, epoch: i64) -> f64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_time_ms(epoch != 0),
            None => 0.0,
        }
    }
}

/// `a == b` on two strings: 1 or 0. No arena and no interpreter — a comparison of
/// code units is all the language means by it, and boxing either side to ask
/// would cost more than the answer.
///
/// A *null* string is a null data pointer, and null equals only null: an empty
/// string is `(non-null, 0)` and must not compare equal to it.
///
/// # Safety
/// Each pointer is either null or names `len` readable code units.
pub(crate) unsafe extern "C" fn str_eq(
    a: *const u16,
    alen: usize,
    bb: *const u16,
    blen: usize,
) -> i64 {
    let (an, bn) = (a.is_null(), bb.is_null());
    if an || bn {
        return i64::from(an && bn);
    }
    if alen != blen {
        return 0;
    }
    let x = unsafe { std::slice::from_raw_parts(a, alen) };
    let y = unsafe { std::slice::from_raw_parts(bb, blen) };
    i64::from(x == y)
}

/// A string method answering with a number or a bool (`indexOf`, `startsWith`).
/// Receiver and arguments are arena handles. `i64::MIN` means it threw.
///
/// # Safety
/// As `native_call`.
pub(crate) unsafe extern "C" fn str_num(
    arena: *mut Arena,
    recv: u64,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const u64,
    argc: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = if argc == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(args_ptr, argc)
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_str_num(recv, name, args),
            None => i64::MIN,
        }
    }
}

/// `throw new Error(msg)`: the error is built by the interpreter and stashed, and
/// the compiled body then traps — the same route a host call that threw takes.
///
/// # Safety
/// As `native_call`; the class name is a `&'static str` baked in at compile time.
pub(crate) unsafe extern "C" fn throw_error(
    arena: *mut Arena,
    cname_ptr: *const u8,
    cname_len: usize,
    msg_ptr: *const u16,
    msg_len: usize,
) {
    unsafe {
        let class = std::str::from_utf8_unchecked(std::slice::from_raw_parts(cname_ptr, cname_len));
        // Leaked once per site at compile time; `throw` wants a `'static` name.
        let class: &'static str = std::mem::transmute::<&str, &'static str>(class);
        let msg = if msg_len == 0 || msg_ptr.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(msg_ptr, msg_len)
        };
        if let Some(ip) = (*arena).interp_ptr() {
            (*ip).jit_throw_error(class, msg);
        }
    }
}

/// A string-valued property read straight off a heap cell (`this.u.pathname`),
/// without the field's value being parked first. `out` receives (data, length,
/// owning handle); 0, or 1 if it threw.
///
/// # Safety
/// As `cell_obj`; `out` names three writable words.
pub(crate) unsafe extern "C" fn cell_prop_str(
    cell: *const Value,
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let Some(ip) = (*arena).interp_ptr() else {
            return 1;
        };
        let sh = (*ip).jit_prop_str_of(&*cell, name);
        if sh == u64::MAX {
            return 1;
        }
        match (*arena).get(sh) {
            Some(Value::Str(rc)) => {
                let units: &[u16] = rc;
                *out = units.as_ptr() as u64;
                *out.add(1) = units.len() as u64;
                *out.add(2) = sh;
                0
            }
            _ => {
                *out = 0;
                *out.add(1) = 0;
                *out.add(2) = 0;
                0
            }
        }
    }
}

/// A string-valued property of an opaque (`u.pathname`). `out` receives
/// (data, length, owning handle); 0, or 1 if it threw.
///
/// # Safety
/// As `native_call`; `out` names three writable words.
pub(crate) unsafe extern "C" fn val_prop_str(
    arena: *mut Arena,
    h: u64,
    name_ptr: *const u8,
    name_len: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let Some(ip) = (*arena).interp_ptr() else {
            return 1;
        };
        let sh = (*ip).jit_val_prop_str(h, name);
        if sh == u64::MAX {
            return 1;
        }
        match (*arena).get(sh) {
            Some(Value::Str(rc)) => {
                let units: &[u16] = rc;
                *out = units.as_ptr() as u64;
                *out.add(1) = units.len() as u64;
                *out.add(2) = sh;
                0
            }
            // Absent, or not a string: a null string, which the caller compares
            // against null exactly as it would an interpreted one.
            _ => {
                *out = 0;
                *out.add(1) = 0;
                *out.add(2) = 0;
                0
            }
        }
    }
}

/// A member call by handle whose result is an opaque, or nothing (`a.push(v)`).
/// The result's handle, 0 for null, `u64::MAX` if it threw.
///
/// # Safety
/// As `native_call`.
pub(crate) unsafe extern "C" fn member_val(
    arena: *mut Arena,
    recv: u64,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const u64,
    argc: usize,
) -> u64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = if argc == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(args_ptr, argc)
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_member_val(recv, name, args),
            None => u64::MAX,
        }
    }
}

/// A string method answering with a *nullable* number (`codePointAt`). `out` gets
/// the value or `i64::MIN` for null; returns 0, or 1 if it threw.
///
/// # Safety
/// As `native_call`; `out` names one writable word.
pub(crate) unsafe extern "C" fn str_numopt(
    arena: *mut Arena,
    recv: u64,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const u64,
    argc: usize,
    out: *mut i64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = if argc == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(args_ptr, argc)
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_str_numopt(recv, name, args, &mut *out),
            None => 1,
        }
    }
}

/// A string method answering with a string (`slice`, `trim`, `replace`). `out`
/// receives (data, length, owning handle); returns 0, or 1 if it threw.
///
/// # Safety
/// As `native_call`; `out` names three writable words.
pub(crate) unsafe extern "C" fn str_str(
    arena: *mut Arena,
    recv: u64,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const u64,
    argc: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = if argc == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(args_ptr, argc)
        };
        let Some(ip) = (*arena).interp_ptr() else {
            return 1;
        };
        let h = (*ip).jit_str_str(recv, name, args);
        if h == u64::MAX {
            return 1;
        }
        // The parts are read back out of the arena entry that owns them, so the
        // pointer compiled code carries and the reference keeping it alive name
        // the same buffer.
        match (*arena).get(h) {
            Some(Value::Str(rc)) => {
                let units: &[u16] = rc;
                *out = units.as_ptr() as u64;
                *out.add(1) = units.len() as u64;
                *out.add(2) = h;
                0
            }
            _ => 1,
        }
    }
}

/// `random.fill(buf)` from compiled code: the buffer's arena handle in, 0 or 1
/// (threw) out. See `Interp::jit_random_fill` for why this bypasses the general
/// native path entirely.
///
/// # Safety
/// As `host_time`.
pub(crate) unsafe extern "C" fn random_fill(arena: *mut Arena, handle: u64) -> i64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_random_fill(handle),
            None => 1,
        }
    }
}

/// The current handle of a top-level web global (`ctx`, `body`), read live.
///
/// # Safety
/// As `host_time`; `name_ptr`/`name_len` name a valid UTF-8 slice (a string
/// constant embedded in the compiled body) that outlives the call.
pub(crate) unsafe extern "C" fn global_web(
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_global_web(name),
            None => 0,
        }
    }
}

/// The arena handle for a top-level binding holding an opaque engine value (a
/// `Bytes`, a `Url`) — how a compiled body gets hold of one it did not make.
/// 0 means the binding does not hold one, and the caller bails.
///
/// # Safety
/// As `global_web`.
pub(crate) unsafe extern "C" fn global_val(
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
) -> u64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_global_val(name),
            None => 0,
        }
    }
}

/// `[]`, `{}`, a set literal: a fresh container in the arena, by handle.
/// `kind` is 0 an array, 1 a map, 2 a set.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn array_new(arena: *mut Arena, kind: i64) -> u64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_array_new(kind),
            None => 0,
        }
    }
}

/// `a.push(v)`, the value as `box_num`'s (kind, bits). 0, or 1 if it is no array.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn array_push(arena: *mut Arena, h: u64, kind: i64, bits: i64) -> i64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_array_push(h, kind, bits),
            None => 1,
        }
    }
}

/// A second arena reference to the same opaque, for a function returning one it
/// only borrowed — the caller takes its handle out of the arena, which must not
/// take it away from whoever still holds it.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn clone_val(arena: *mut Arena, h: u64) -> u64 {
    unsafe {
        match (*arena).get(h).cloned() {
            Some(v) => (*arena).keep(v),
            None => 0,
        }
    }
}

/// `b[i]` on an opaque. The byte, or `i64::MIN` if it threw.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn val_index_get(arena: *mut Arena, h: u64, idx: i64) -> i64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_val_index_get(h, idx),
            None => i64::MIN,
        }
    }
}

/// `b[i] = v` on an opaque. 0, or 1 if it threw.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn val_index_set(arena: *mut Arena, h: u64, idx: i64, v: i64) -> i64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_val_index_set(h, idx, v),
            None => 1,
        }
    }
}

/// A top-level binding holding a number, as raw bits. See `Interp::jit_global_num`
/// for why this is a call and not a value read once at entry.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn global_num(
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_global_num(name),
            None => 0,
        }
    }
}

/// A top-level binding holding a string, as compiled code carries one. `out`
/// receives (data, length); the arena entry keeping the buffer alive for the call
/// is the interpreter's, so the value read here is a borrow and owns nothing.
/// Both words are 0 when the binding holds no string.
///
/// # Safety
/// As `global_val`; `out` names two writable words.
pub(crate) unsafe extern "C" fn global_str(
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
    out: *mut u64,
) {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let h = match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_global_str(name),
            None => 0,
        };
        let (ptr, len) = match (*arena).get(h) {
            Some(Value::Str(rc)) => {
                let units: &[u16] = rc;
                (units.as_ptr() as u64, units.len() as u64)
            }
            _ => (0, 0),
        };
        *out = ptr;
        *out.add(1) = len;
    }
}

/// A `std:` native call — `random.fill`, `bytes.encodeUtf8`, `parse.url`.
///
/// The name arrives already joined ("random.fill"), because the compiler knows
/// it and a shim that formatted one per call would spend more on the string
/// than the call saves. Arguments and the result cross as arena handles: it is
/// the one representation that fits every `Value` without this tier having to
/// model it. Returns the result handle, 0 for a null/void result, or
/// `u64::MAX` if it threw (stashed for `after_jit`, as every host shim does).
///
/// # Safety
/// As `global_val`; `args_ptr`/`argc` name a valid `u64` slice of handles.
pub(crate) unsafe extern "C" fn native_call(
    arena: *mut Arena,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const u64,
    argc: usize,
    // `Interp::NATIVE_FAST`'s index for this native, resolved once at compile
    // time, or `u32::MAX`. Pointer-width because that is what the shim
    // declaration gives every argument; the name is still carried for the
    // general path.
    id: usize,
) -> u64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = if argc == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(args_ptr, argc)
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_native_call(name, id as u32, args),
            None => u64::MAX,
        }
    }
}

/// Park a compiled string in the arena as a `Value::Str`, so a native can take
/// it as an argument. `have` is the handle the string already owns (0 for a
/// constant or a borrow); see `Interp::jit_box_str` for why that is worth
/// passing. Returns the handle to hand the native.
///
/// # Safety
/// As `global_val`; `ptr`/`len` name a valid UTF-16 slice that outlives the call.
pub(crate) unsafe extern "C" fn box_str(
    arena: *mut Arena,
    ptr: *const u16,
    len: usize,
    have: u64,
) -> u64 {
    unsafe {
        let units = if len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(ptr, len)
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_box_str(units, have),
            None => 0,
        }
    }
}

/// The same for a number: `kind` 0 is an `int32` (the low 32 bits of `bits`),
/// 1 is a `float64` (`bits` as an IEEE pattern).
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn box_num(arena: *mut Arena, kind: i64, bits: i64) -> u64 {
    unsafe {
        match (*arena).interp_ptr() {
            Some(ip) => {
                if kind == 0 {
                    (*ip).jit_box_i32(bits as i32)
                } else {
                    (*ip).jit_box_f64(f64::from_bits(bits as u64))
                }
            }
            None => 0,
        }
    }
}

/// `length` on an opaque. -1 means there is no integer length here and the
/// caller bails to the interpreter.
///
/// # Safety
/// As `global_val`.
pub(crate) unsafe extern "C" fn val_len(arena: *mut Arena, handle: u64) -> i64 {
    // Straight off the arena. Measuring a container does not need the interpreter,
    // and the hop through `interp_ptr` into `Interp::jit_val_len` was half of what
    // a `.length` cost in a compiled loop. These are the kinds `get_member`
    // answers `"length"` (or, for a map and a set, `"size"`) with; anything else
    // is -1 and the caller bails, as before.
    match unsafe { (*arena).get(handle) } {
        Some(Value::Bytes(b)) => b.borrow().len() as i64,
        Some(Value::Str(s)) => s.len() as i64,
        Some(Value::Array(a)) => a.borrow().len() as i64,
        Some(Value::MapV(m)) => m.borrow().len() as i64,
        Some(Value::SetV(m)) => m.borrow().len() as i64,
        _ => -1,
    }
}

/// A numeric-argument web method call whose result is discarded
/// (`ctx.fillRect(x, y, w, h)`). Returns 0 on success, 1 if it threw (the
/// interpreter stashed the error and the compiled body then traps).
///
/// # Safety
/// As `global_web`; `args_ptr`/`argc` name a valid `f64` slice for the call.
pub(crate) unsafe extern "C" fn web_call_num(
    arena: *mut Arena,
    target: i64,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const f64,
    argc: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = std::slice::from_raw_parts(args_ptr, argc);
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_call_num(target, name, args),
            None => 0,
        }
    }
}

/// A numeric-valued web property read (`buf.length`): reuses the interpreter's
/// `web_get`. Returns the integer, or `i64::MIN` if the read threw (the error is
/// stashed for `after_jit`).
///
/// # Safety
/// As `web_call_num`; `name_ptr`/`name_len` name a valid UTF-8 slice.
pub(crate) unsafe extern "C" fn web_get_num(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_get_num(target, id, name),
            None => 0,
        }
    }
}

/// One argument to a typed web call. `kind` 0 = number (`a` holds the `f64`
/// bits), 1 = host handle (`a` = handle), 2 = string (`a` = UTF-16 pointer,
/// `b` = length). Matches what the web-call codegen writes into a stack slot.
#[repr(C)]
pub(crate) struct WebArgDesc {
    pub kind: i64,
    pub a: i64,
    pub b: i64,
}

/// A web method call from compiled code with mixed argument kinds (numbers,
/// handles, strings) whose result is discarded — `getRandomValues(buf)`,
/// `appendChild(el)`, `setItem(k, v)`. Decodes the descriptor into `WebArg`s and
/// hands them to the interpreter's own call path. Same 0/1 return as
/// `web_call_num`.
///
/// # Safety
/// As `web_call_num`; each string descriptor's `(a, b)` is a live UTF-16 slice
/// for the call, and `desc`/`argc` name a valid `WebArgDesc` array.
pub(crate) unsafe extern "C" fn web_call_v(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
    desc: *const WebArgDesc,
    argc: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let descs = std::slice::from_raw_parts(desc, argc);
        let mut args: Vec<WebArg> = Vec::with_capacity(argc);
        for d in descs {
            args.push(match d.kind {
                0 => WebArg::Num(f64::from_bits(d.a as u64)),
                1 => WebArg::Ref(d.a),
                2 => WebArg::Str(std::slice::from_raw_parts(d.a as *const u16, d.b as usize)),
                _ => WebArg::Null,
            });
        }
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_call_args(target, id, name, &args),
            None => 0,
        }
    }
}

/// A web method call whose result is a string, captured by compiled code
/// (`getItem(k)`). Same argument descriptor as `web_call_v`; `out` receives
/// (data pointer, length, arena handle) — all zero for a null result — and the
/// return is 0 on success, 1 if the call threw. The result string is kept in the
/// arena so its handle names it and the sweep frees it.
///
/// # Safety
/// As `web_call_v`; `out` points at three writable words.
pub(crate) unsafe extern "C" fn web_call_str_v(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
    desc: *const WebArgDesc,
    argc: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let descs = std::slice::from_raw_parts(desc, argc);
        let mut args: Vec<WebArg> = Vec::with_capacity(argc);
        for d in descs {
            args.push(match d.kind {
                0 => WebArg::Num(f64::from_bits(d.a as u64)),
                1 => WebArg::Ref(d.a),
                2 => WebArg::Str(std::slice::from_raw_parts(d.a as *const u16, d.b as usize)),
                _ => WebArg::Null,
            });
        }
        *out = 0;
        *out.add(1) = 0;
        *out.add(2) = 0;
        match (*arena).interp_ptr() {
            Some(ip) => match (*ip).jit_web_call_str_value(target, id, name, &args) {
                None => 1, // threw
                Some(Value::Str(rc)) => {
                    let s: &[u16] = &rc;
                    let data = s.as_ptr() as u64;
                    let len = s.len() as u64;
                    let handle = (*arena).keep(Value::Str(rc));
                    *out = data;
                    *out.add(1) = len;
                    *out.add(2) = handle;
                    0
                }
                // A host handle (`createElement`): the id itself in the first
                // word; a `Ty::Web` result reads only that.
                Some(Value::JsRef(h)) => {
                    *out = h as u64;
                    0
                }
                // Null or another reply: a null string / null handle (0,0,0).
                Some(_) => 0,
            },
            None => 0,
        }
    }
}

/// A web property set from compiled code (`el.textContent = str`). The value is
/// one `WebArgDesc`-style triple: `kind` 0 a number (`a` = f64 bits), 2 a string
/// (`a` = ptr, `b` = len). Same 0/1 return as `web_call_v`.
///
/// # Safety
/// As `web_call_v`; a string value's `(a, b)` is a live UTF-16 slice for the call.
pub(crate) unsafe extern "C" fn web_set_v(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
    kind: i64,
    a: i64,
    b: i64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let value = match kind {
            0 => WebArg::Num(f64::from_bits(a as u64)),
            2 => WebArg::Str(std::slice::from_raw_parts(a as *const u16, b as usize)),
            _ => WebArg::Null,
        };
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_set(target, id, name, &value),
            None => 0,
        }
    }
}

/// A host constructor from compiled code (`new URL(s)`). Decodes the argument
/// descriptor (as `web_call_v`) and hands it to the interpreter's `web_new`; the
/// resulting handle is written to `out[0]` (0 for a null result), and the return
/// is 0 on success, 1 if it threw. `id` is the pre-interned constructor id, or
/// `u32::MAX` to intern `name` lazily.
///
/// # Safety
/// As `web_call_v`; `out` points at one writable word.
pub(crate) unsafe extern "C" fn web_new_v(
    arena: *mut Arena,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
    desc: *const WebArgDesc,
    argc: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let descs = std::slice::from_raw_parts(desc, argc);
        let mut args: Vec<WebArg> = Vec::with_capacity(argc);
        for d in descs {
            args.push(match d.kind {
                0 => WebArg::Num(f64::from_bits(d.a as u64)),
                1 => WebArg::Ref(d.a),
                2 => WebArg::Str(std::slice::from_raw_parts(d.a as *const u16, d.b as usize)),
                _ => WebArg::Null,
            });
        }
        *out = 0;
        match (*arena).interp_ptr() {
            Some(ip) => match (*ip).jit_web_new_value(id, name, &args) {
                None => 1, // threw
                Some(Value::JsRef(h)) => {
                    *out = h as u64;
                    0
                }
                Some(_) => 0, // null / non-handle: a null handle
            },
            None => 0,
        }
    }
}

/// A string-valued web property read from compiled code (`url.pathname`). Like
/// `web_get_num`, but the result is a string: `out` receives (data pointer,
/// length, arena handle), all zero for a null result, and the return is 0 on
/// success, 1 if the read threw. The string is kept in the arena so its handle
/// names it for the sweep.
///
/// # Safety
/// As `web_get_num`; `out` points at three writable words.
pub(crate) unsafe extern "C" fn web_get_str_v(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
    out: *mut u64,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        *out = 0;
        *out.add(1) = 0;
        *out.add(2) = 0;
        match (*arena).interp_ptr() {
            Some(ip) => match (*ip).jit_web_get_str_value(target, id, name) {
                None => 1, // threw
                Some(Value::Str(rc)) => {
                    let s: &[u16] = &rc;
                    let data = s.as_ptr() as u64;
                    let len = s.len() as u64;
                    let handle = (*arena).keep(Value::Str(rc));
                    *out = data;
                    *out.add(1) = len;
                    *out.add(2) = handle;
                    0
                }
                Some(_) => 0, // null / non-string: a null string
            },
            None => 0,
        }
    }
}

/// The length of a string-valued web property, without materializing the string
/// (`url.pathname.length`). Like `web_get_num`, but the host reply is a string
/// and only its code-unit count crosses back — no arena keep for a string the
/// compiled code would drop next instruction. Returns the length, or `i64::MIN`
/// if the read (or a null `.length`) threw.
///
/// # Safety
/// As `web_get_num`.
pub(crate) unsafe extern "C" fn web_get_str_len_v(
    arena: *mut Arena,
    target: i64,
    id: u32,
    name_ptr: *const u8,
    name_len: usize,
) -> i64 {
    unsafe {
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_get_str_len(target, id, name),
            None => 0,
        }
    }
}

/// The typed-binding fast path (`ctx.fillRect(...)` as a bind id): a numeric web
/// call that crosses as a compile-time id, not an interned name. `name`/`name_len`
/// still name the method, used only if the host has no typed binding and the call
/// falls back to the interned path. Same return protocol as `web_call_num`.
///
/// # Safety
/// As `web_call_num`.
pub(crate) unsafe extern "C" fn web_bind_call(
    arena: *mut Arena,
    target: i64,
    bind_id: u32,
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const f64,
    argc: usize,
) -> i64 {
    unsafe {
        // The fast path, and the reason this binding exists: call the host's
        // `web_bind` entry *directly*, with no interpreter reentry and no dynamic
        // `Host` dispatch. The compiled call discards the result, so only a
        // thrown host error (reply tag 5) needs anything more — and even then
        // only to build and stash the throw, never a second call to the host.
        if let Some((f, data)) = (*arena).web_bind_fn() {
            let mut reply = WebReplyRaw {
                tag: 0,
                num: 0.0,
                str16: std::ptr::null(),
                str16_len: 0,
            };
            f(data, target, bind_id, args_ptr, argc, &mut reply);
            if reply.tag != 5 {
                return 0;
            }
            let msg: &[u16] = if reply.str16.is_null() || reply.str16_len == 0 {
                &[]
            } else {
                std::slice::from_raw_parts(reply.str16, reply.str16_len)
            };
            return match (*arena).interp_ptr() {
                Some(ip) => (*ip).jit_stash_host_error(msg),
                None => 1,
            };
        }
        // No direct binding (a host that leaves `web_bind` null): the ordinary
        // interned path, which is always correct.
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name_ptr, name_len));
        let args = std::slice::from_raw_parts(args_ptr, argc);
        match (*arena).interp_ptr() {
            Some(ip) => (*ip).jit_web_bind(target, bind_id, name, args),
            None => 0,
        }
    }
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
