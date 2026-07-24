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
    let mut buf: Vec<u16> = Vec::new();
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
#[allow(dead_code)]
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
