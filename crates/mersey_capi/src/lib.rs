// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! The C embedding ABI (include/mersey.h) — the boundary the Chromium fork's
//! `//components/mersey` wraps, proven here by a plain-C host
//! (native/host_demo.c) and a Rust integration test (tests/abi.rs), with no
//! V8 and no WASM anywhere in the stack.
//!
//! It mirrors the WASM boundary function for function, and *shares its
//! implementation*: loading and running go through `mersey_interp::embed`,
//! the same code the browser loader drives. The only thing that lives here is
//! translation — C strings in, C strings out, and a host table standing in
//! for the JS bridge.
//!
//! **Re-entrancy.** A bridge call the engine makes can call straight back in:
//! `new Promise(executor)` runs its executor synchronously, a host-side array
//! callback invokes a Mersey closure. So the context must be usable from
//! inside its own call, which `&mut self` forbids — the interpreter lives in
//! an `UnsafeCell`, exactly as the WASM boundary holds it. The discipline
//! that makes this sound is the ABI's thread rule: one context, one thread,
//! so re-entry forms a call stack and never an overlap. A `RefCell` would
//! spuriously panic on the legitimate case; two threads are excluded by
//! contract (and by construction in the fork: one context per Blink
//! ExecutionContext, always called from its task runner).

use std::cell::{RefCell, UnsafeCell};
use std::ffi::c_void;
use std::os::raw::c_char;
use std::rc::Rc;

use mersey_interp::debug::{DebugController, StopReason};
use mersey_interp::{embed, new_interp, DebugHook, DebugPause, Host, Interp, WebScalar};

/// Bumped whenever the table layout or a boundary contract changes. The
/// embedder checks before installing a table. Defined once in `mersey_interp`
/// so the engine, this C ABI, and the language-level `Mersey.abiVersion` all
/// report the same number; `mersey.h`'s `#define MSY_ABI_VERSION` must match it.
pub const MSY_ABI_VERSION: u32 = mersey_interp::ABI_VERSION;

/// Tier 0 only: never map executable pages (the jitless configuration for
/// sandboxes that forbid a second JIT).
pub const MSY_FLAG_NO_JIT: u32 = 0x1;

#[no_mangle]
pub extern "C" fn msy_abi_version() -> u32 {
    MSY_ABI_VERSION
}

type ReplyFn1 = extern "C" fn(*mut c_void, i64, *const c_char, usize, *mut usize) -> *const c_char;
type ReplyFn2 = extern "C" fn(
    *mut c_void,
    i64,
    *const c_char,
    usize,
    *const c_char,
    usize,
    *mut usize,
) -> *const c_char;
type ReplyFnCtor = extern "C" fn(
    *mut c_void,
    *const c_char,
    usize,
    *const c_char,
    usize,
    *mut usize,
) -> *const c_char;

/// The C host table — field for field, include/mersey.h. Order is ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyHostTable {
    pub data: *mut c_void,

    // console + diagnostics
    pub print: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,
    pub print_level: Option<extern "C" fn(*mut c_void, *const c_char, usize, *const c_char, usize)>,
    pub error: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,

    // capabilities
    pub caps: Option<extern "C" fn(*mut c_void, *mut usize) -> *const c_char>,

    // universal web bridge
    pub web_global: Option<extern "C" fn(*mut c_void, *const c_char, usize) -> i64>,
    pub web_get: Option<ReplyFn1>,
    pub web_set: Option<ReplyFn2>,
    pub web_call: Option<ReplyFn2>,
    pub web_new: Option<ReplyFnCtor>,
    pub web_iterate: Option<extern "C" fn(*mut c_void, i64, *mut usize) -> *const c_char>,
    pub web_instanceof: Option<extern "C" fn(*mut c_void, i64, i64) -> i32>,
    pub web_release: Option<extern "C" fn(*mut c_void, i64)>,
    pub web_bytes_read: Option<extern "C" fn(*mut c_void, i64, *mut usize) -> *const u8>,
    pub web_bytes_write: Option<extern "C" fn(*mut c_void, *const u8, usize) -> i64>,

    // time + entropy
    pub time_ms: Option<extern "C" fn(*mut c_void, i32) -> f64>,
    pub random_bytes: Option<extern "C" fn(*mut c_void, *mut u8, usize) -> i32>,

    // legacy fake-DOM hooks (native demos/tests)
    pub dom_set_text:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *const c_char, usize)>,
    pub dom_get_text:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *mut usize) -> *const c_char>,
    pub dom_add_listener:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *const c_char, usize, u32)>,

    // interned bridge fast paths (ABI v3)
    pub web_intern: Option<extern "C" fn(*mut c_void, *const c_char, usize) -> u32>,
    pub web_get_id: Option<extern "C" fn(*mut c_void, i64, u32, *mut usize) -> *const c_char>,
    pub web_set_str: Option<
        extern "C" fn(*mut c_void, i64, u32, *const c_char, usize, *mut usize) -> *const c_char,
    >,
    pub web_set_num: Option<extern "C" fn(*mut c_void, i64, u32, f64, *mut usize) -> *const c_char>,
    pub web_call_str: Option<
        extern "C" fn(*mut c_void, i64, u32, *const c_char, usize, *mut usize) -> *const c_char,
    >,
    pub web_call_scalars: Option<
        extern "C" fn(*mut c_void, i64, u32, *const MsyScalar, usize, *mut usize) -> *const c_char,
    >,
    pub web_new_scalars: Option<
        extern "C" fn(*mut c_void, u32, *const MsyScalar, usize, *mut usize) -> *const c_char,
    >,

    // wide-string fast paths (ABI v5): UTF-16 args in, typed UTF-16 reply out
    pub web_get_u16: Option<extern "C" fn(*mut c_void, i64, u32, *mut MsyReply)>,
    pub web_set_u16: Option<extern "C" fn(*mut c_void, i64, u32, *const MsyArg16, *mut MsyReply)>,
    pub web_call_u16:
        Option<extern "C" fn(*mut c_void, i64, u32, *const MsyArg16, usize, *mut MsyReply)>,
    pub web_new_u16: Option<extern "C" fn(*mut c_void, u32, *const MsyArg16, usize, *mut MsyReply)>,
    // typed-binding fast path (ABI v7): a compiled numeric web method as a
    // compile-time id + raw f64 args, no name and no MsyArg16 marshalling.
    pub web_bind: Option<extern "C" fn(*mut c_void, i64, u32, *const f64, usize, *mut MsyReply)>,
    // batched DOM mutation (ABI v10): a whole render's ops in one crossing.
    // (data, ops, nops, nodes, nnodes, strs, nstrs, created_out, created_cap)
    #[allow(clippy::type_complexity)]
    pub web_apply: Option<
        extern "C" fn(
            *mut c_void,
            *const i32,
            usize,
            *const i64,
            usize,
            *const MsyStr16,
            usize,
            *mut i64,
            usize,
        ) -> usize,
    >,
}

/// A borrowed UTF-16 string in a batch's string pool — field for field with
/// `msy_str16` in include/mersey.h.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyStr16 {
    pub ptr: *const u16,
    pub len: u32,
}

/// A UTF-16 argument, field for field with `msy_arg16` in include/mersey.h.
/// `kind`: 0 str16, 1 num, 2 ref handle, 3 bool, 4 null.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyArg16 {
    pub kind: i32,
    pub num: f64,
    pub str16: *const u16,
    pub str16_len: usize,
}

impl Default for MsyArg16 {
    fn default() -> Self {
        // `null` (kind 4): the fill value for the unused tail of a stack buffer.
        MsyArg16 {
            kind: 4,
            num: 0.0,
            str16: std::ptr::null(),
            str16_len: 0,
        }
    }
}

/// A typed reply, field for field with `msy_reply` in include/mersey.h.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyReply {
    pub tag: i32,
    pub num: f64,
    pub str16: *const u16,
    pub str16_len: usize,
}

impl Default for MsyReply {
    fn default() -> Self {
        MsyReply {
            tag: 0,
            num: 0.0,
            str16: std::ptr::null(),
            str16_len: 0,
        }
    }
}

fn to_msy_arg16(a: &mersey_interp::WebArg) -> MsyArg16 {
    use mersey_interp::WebArg;
    let num = |kind: i32, n: f64| MsyArg16 {
        kind,
        num: n,
        str16: std::ptr::null(),
        str16_len: 0,
    };
    match a {
        WebArg::Num(n) => num(1, *n),
        // The engine's string is already UTF-16: the code units cross with no
        // copy and no conversion.
        WebArg::Str(units) => MsyArg16 {
            kind: 0,
            num: 0.0,
            str16: units.as_ptr(),
            str16_len: units.len(),
        },
        WebArg::Ref(h) => num(2, *h as f64),
        WebArg::Bool(b) => num(3, if *b { 1.0 } else { 0.0 }),
        WebArg::Null => num(4, 0.0),
        // A durable Mersey callable as its stable callback id (ABI v8): the
        // host resolves it to its cached wrapper, exactly as the JSON path's
        // {"__cb__":id} would.
        WebArg::Cb(id) => num(5, *id as f64),
    }
}

/// Turn a filled reply into a `WebReply`. A string reply is UTF-16, which *is*
/// the engine's form — a plain copy of the code units, no conversion.
fn read_msy_reply(r: &MsyReply) -> mersey_interp::WebReply {
    use mersey_interp::WebReply;
    let units = || -> Vec<u16> {
        if r.str16.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(r.str16, r.str16_len) }.to_vec()
    };
    // Err/Json want a Rust String; decode there (rare paths).
    let as_string = || String::from_utf16_lossy(&units());
    match r.tag {
        1 => WebReply::Num(r.num),
        2 => WebReply::Str(units()),
        3 => WebReply::Ref(r.num as i64),
        4 => WebReply::Bool(r.num != 0.0),
        5 => WebReply::Err(as_string()),
        7 => WebReply::Json(as_string()),
        _ => WebReply::Null,
    }
}

/// A scalar argument, field for field with `msy_scalar` in include/mersey.h.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyScalar {
    pub is_num: i32,
    pub num: f64,
    pub str_ptr: *const c_char,
    pub str_len: usize,
}

fn to_msy_scalar(s: &WebScalar) -> MsyScalar {
    match s {
        WebScalar::Num(n) => MsyScalar {
            is_num: 1,
            num: *n,
            str_ptr: std::ptr::null(),
            str_len: 0,
        },
        WebScalar::Str(s) => MsyScalar {
            is_num: 0,
            num: 0.0,
            str_ptr: s.as_ptr() as *const c_char,
            str_len: s.len(),
        },
    }
}

fn as_parts(s: &str) -> (*const c_char, usize) {
    (s.as_ptr() as *const c_char, s.len())
}

/// A host reply buffer as an owned String. NULL means "{}" — an empty reply,
/// not an error — so a host may leave any hook unimplemented.
fn read_reply(ptr: *const c_char, len: usize) -> String {
    if ptr.is_null() {
        return "{}".to_string();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

struct CHost {
    table: MsyHostTable,
    /// Parsed once from the table's `caps` JSON at context creation.
    caps: Vec<String>,
}

impl CHost {
    fn new(table: MsyHostTable) -> CHost {
        let caps = match table.caps {
            Some(f) => {
                let mut len = 0usize;
                let ptr = f(table.data, &mut len);
                let json = read_reply(ptr, len);
                match mersey_interp::webjson::parse(&json) {
                    Some(mersey_interp::webjson::Json::Arr(items)) => items
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            // NULL grants nothing: deny-by-default is the point (§5.4).
            None => Vec::new(),
        };
        CHost { table, caps }
    }
}

impl Host for CHost {
    fn print(&mut self, s: &str) {
        if let Some(f) = self.table.print {
            let (p, l) = as_parts(s);
            f(self.table.data, p, l);
        }
    }
    fn print_level(&mut self, level: &str, s: &str) {
        match self.table.print_level {
            Some(f) => {
                let (lp, ll) = as_parts(level);
                let (p, l) = as_parts(s);
                f(self.table.data, lp, ll, p, l);
            }
            None => self.print(s),
        }
    }

    fn caps(&self) -> Vec<String> {
        self.caps.clone()
    }
    fn drop_cap(&mut self, cap: &str) {
        self.caps.retain(|c| c != cap);
    }

    // ---- universal web bridge ------------------------------------------

    fn web_global(&mut self, name: &str) -> i64 {
        match self.table.web_global {
            Some(f) => {
                let (p, l) = as_parts(name);
                f(self.table.data, p, l)
            }
            None => -1,
        }
    }
    fn web_get(&mut self, target: i64, prop: &str) -> String {
        let Some(f) = self.table.web_get else {
            return "{}".to_string();
        };
        let (p, l) = as_parts(prop);
        let mut len = 0usize;
        let r = f(self.table.data, target, p, l, &mut len);
        read_reply(r, len)
    }
    fn web_set(&mut self, target: i64, prop: &str, value_json: &str) -> String {
        let Some(f) = self.table.web_set else {
            return "{}".to_string();
        };
        let (p, l) = as_parts(prop);
        let (vp, vl) = as_parts(value_json);
        let mut len = 0usize;
        let r = f(self.table.data, target, p, l, vp, vl, &mut len);
        read_reply(r, len)
    }
    fn web_call(&mut self, target: i64, method: &str, args_json: &str) -> String {
        let Some(f) = self.table.web_call else {
            return "{}".to_string();
        };
        let (p, l) = as_parts(method);
        let (ap, al) = as_parts(args_json);
        let mut len = 0usize;
        let r = f(self.table.data, target, p, l, ap, al, &mut len);
        read_reply(r, len)
    }
    fn web_new(&mut self, ctor: &str, args_json: &str) -> String {
        let Some(f) = self.table.web_new else {
            return "{}".to_string();
        };
        let (p, l) = as_parts(ctor);
        let (ap, al) = as_parts(args_json);
        let mut len = 0usize;
        let r = f(self.table.data, p, l, ap, al, &mut len);
        read_reply(r, len)
    }
    fn web_iterate(&mut self, target: i64) -> String {
        let Some(f) = self.table.web_iterate else {
            return "{}".to_string();
        };
        let mut len = 0usize;
        let r = f(self.table.data, target, &mut len);
        read_reply(r, len)
    }
    fn web_instanceof(&mut self, target: i64, ctor: i64) -> bool {
        match self.table.web_instanceof {
            Some(f) => f(self.table.data, target, ctor) != 0,
            None => false,
        }
    }
    fn web_release(&mut self, target: i64) {
        if let Some(f) = self.table.web_release {
            f(self.table.data, target);
        }
    }
    fn web_bytes_read(&mut self, target: i64) -> Option<Vec<u8>> {
        let f = self.table.web_bytes_read?;
        let mut len = 0usize;
        let ptr = f(self.table.data, target, &mut len);
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
    }
    fn web_bytes_write(&mut self, bytes: &[u8]) -> i64 {
        match self.table.web_bytes_write {
            Some(f) => f(self.table.data, bytes.as_ptr(), bytes.len()),
            None => -1,
        }
    }

    // ---- interned fast paths (skip JSON; a name crosses once) ----------
    // A host that leaves any of these NULL declines interning: web_intern
    // returns u32::MAX and the engine uses the reflective path above.
    fn web_intern(&mut self, name: &str) -> u32 {
        match self.table.web_intern {
            Some(f) => {
                let (p, l) = as_parts(name);
                f(self.table.data, p, l)
            }
            None => u32::MAX,
        }
    }
    fn web_get_id(&mut self, target: i64, name_id: u32) -> String {
        let Some(f) = self.table.web_get_id else {
            return "{}".to_string();
        };
        let mut len = 0usize;
        let r = f(self.table.data, target, name_id, &mut len);
        read_reply(r, len)
    }
    fn web_set_str(&mut self, target: i64, name_id: u32, value: &str) -> String {
        let Some(f) = self.table.web_set_str else {
            return "{}".to_string();
        };
        let (vp, vl) = as_parts(value);
        let mut len = 0usize;
        let r = f(self.table.data, target, name_id, vp, vl, &mut len);
        read_reply(r, len)
    }
    fn web_set_num(&mut self, target: i64, name_id: u32, value: f64) -> String {
        let Some(f) = self.table.web_set_num else {
            return "{}".to_string();
        };
        let mut len = 0usize;
        let r = f(self.table.data, target, name_id, value, &mut len);
        read_reply(r, len)
    }
    fn web_call_str(&mut self, target: i64, name_id: u32, arg: &str) -> String {
        let Some(f) = self.table.web_call_str else {
            return "{}".to_string();
        };
        let (ap, al) = as_parts(arg);
        let mut len = 0usize;
        let r = f(self.table.data, target, name_id, ap, al, &mut len);
        read_reply(r, len)
    }
    fn web_call_scalars(&mut self, target: i64, name_id: u32, args: &[WebScalar]) -> String {
        // Empty reply = "not supported": the engine falls back to the reflective
        // path. A host that leaves this NULL declines wholesale.
        let Some(f) = self.table.web_call_scalars else {
            return String::new();
        };
        let cargs: Vec<MsyScalar> = args.iter().map(to_msy_scalar).collect();
        let mut len = 0usize;
        let r = f(
            self.table.data,
            target,
            name_id,
            cargs.as_ptr(),
            cargs.len(),
            &mut len,
        );
        read_reply(r, len)
    }
    fn web_new_scalars(&mut self, ctor_id: u32, args: &[WebScalar]) -> String {
        let Some(f) = self.table.web_new_scalars else {
            return String::new();
        };
        let cargs: Vec<MsyScalar> = args.iter().map(to_msy_scalar).collect();
        let mut len = 0usize;
        let r = f(
            self.table.data,
            ctor_id,
            cargs.as_ptr(),
            cargs.len(),
            &mut len,
        );
        read_reply(r, len)
    }

    // ---- wide-string fast paths (UTF-32 in, UTF-16 out) ----------------
    fn web_get_u16(&mut self, target: i64, name_id: u32) -> Option<mersey_interp::WebReply> {
        let f = self.table.web_get_u16?;
        let mut reply = MsyReply::default();
        f(self.table.data, target, name_id, &mut reply);
        Some(read_msy_reply(&reply))
    }
    fn web_set_u16(
        &mut self,
        target: i64,
        name_id: u32,
        value: &mersey_interp::WebArg,
    ) -> Option<mersey_interp::WebReply> {
        let f = self.table.web_set_u16?;
        let arg = to_msy_arg16(value);
        let mut reply = MsyReply::default();
        f(self.table.data, target, name_id, &arg, &mut reply);
        Some(read_msy_reply(&reply))
    }
    fn web_call_u16(
        &mut self,
        target: i64,
        name_id: u32,
        args: &[mersey_interp::WebArg],
    ) -> Option<mersey_interp::WebReply> {
        let f = self.table.web_call_u16?;
        // On the stack for the common small-arity call — no heap `Vec` per web
        // call, matching the engine's own stack-array marshalling upstream.
        let mut reply = MsyReply::default();
        if args.len() <= 8 {
            let mut cargs: [MsyArg16; 8] = std::array::from_fn(|_| MsyArg16::default());
            for (k, a) in args.iter().enumerate() {
                cargs[k] = to_msy_arg16(a);
            }
            f(
                self.table.data,
                target,
                name_id,
                cargs.as_ptr(),
                args.len(),
                &mut reply,
            );
        } else {
            let cargs: Vec<MsyArg16> = args.iter().map(to_msy_arg16).collect();
            f(
                self.table.data,
                target,
                name_id,
                cargs.as_ptr(),
                cargs.len(),
                &mut reply,
            );
        }
        Some(read_msy_reply(&reply))
    }
    fn web_new_u16(
        &mut self,
        ctor_id: u32,
        args: &[mersey_interp::WebArg],
    ) -> Option<mersey_interp::WebReply> {
        let f = self.table.web_new_u16?;
        let cargs: Vec<MsyArg16> = args.iter().map(to_msy_arg16).collect();
        let mut reply = MsyReply::default();
        f(
            self.table.data,
            ctor_id,
            cargs.as_ptr(),
            cargs.len(),
            &mut reply,
        );
        Some(read_msy_reply(&reply))
    }
    fn web_bind(
        &mut self,
        target: i64,
        bind_id: u32,
        args: &[f64],
    ) -> Option<mersey_interp::WebReply> {
        let f = self.table.web_bind?;
        let mut reply = MsyReply::default();
        f(
            self.table.data,
            target,
            bind_id,
            args.as_ptr(),
            args.len(),
            &mut reply,
        );
        Some(read_msy_reply(&reply))
    }
    fn web_apply(&mut self, ops: &[i32], nodes: &[i64], strs: &[String]) -> Option<Vec<i64>> {
        let f = self.table.web_apply?;
        // UTF-16 encode the string pool; the Vec<Vec<u16>> owns the buffers the
        // borrowed MsyStr16 pointers reference, so it must outlive the call.
        let utf16: Vec<Vec<u16>> = strs.iter().map(|s| s.encode_utf16().collect()).collect();
        let cstrs: Vec<MsyStr16> = utf16
            .iter()
            .map(|u| MsyStr16 {
                ptr: u.as_ptr(),
                len: u.len() as u32,
            })
            .collect();
        // Exactly one handle comes back per CREATE op (op code 0 at a group head).
        let created_cap = ops.chunks_exact(4).filter(|c| c[0] == 0).count();
        let mut created = vec![0i64; created_cap];
        let n = f(
            self.table.data,
            ops.as_ptr(),
            ops.len() / 4,
            nodes.as_ptr(),
            nodes.len(),
            cstrs.as_ptr(),
            cstrs.len(),
            created.as_mut_ptr(),
            created.len(),
        );
        created.truncate(n.min(created_cap));
        Some(created)
    }
    fn web_bind_raw(&self) -> Option<(mersey_interp::WebBindFn, *mut std::ffi::c_void)> {
        let f = self.table.web_bind?;
        // `MsyReply` and `mersey_interp::WebReplyRaw` are the same `#[repr(C)]`
        // layout (asserted below), so the two function pointers differ only in
        // the name of their last argument's pointee — same ABI. Reinterpreting
        // one as the other lets the JIT call the host with no interpreter reentry
        // and no `dyn Host` dispatch.
        const _: () = {
            assert!(
                std::mem::size_of::<MsyReply>()
                    == std::mem::size_of::<mersey_interp::WebReplyRaw>()
            );
            assert!(
                std::mem::align_of::<MsyReply>()
                    == std::mem::align_of::<mersey_interp::WebReplyRaw>()
            );
        };
        let raw: mersey_interp::WebBindFn = unsafe { std::mem::transmute(f) };
        Some((raw, self.table.data))
    }

    // ---- time + entropy --------------------------------------------------

    fn time_ms(&mut self, epoch: bool) -> f64 {
        match self.table.time_ms {
            Some(f) => f(self.table.data, i32::from(epoch)),
            None => 0.0,
        }
    }
    fn random_bytes(&mut self, n: usize) -> Result<Vec<u8>, String> {
        // The table hook is raw entropy; *policy* is the caps list. A host that
        // wires the hook but grants no `random` capability has still said no —
        // deny-by-default belongs to the grant, not to which pointers happen to
        // be non-NULL.
        if !self.caps.iter().any(|c| c == "random") {
            return Err("no `random` capability granted to this context".to_string());
        }
        let Some(f) = self.table.random_bytes else {
            return Err("host provides no entropy".to_string());
        };
        let mut buf = vec![0u8; n];
        if f(self.table.data, buf.as_mut_ptr(), n) != 0 {
            return Err("host entropy failed".to_string());
        }
        Ok(buf)
    }

    // ---- legacy fake-DOM hooks -------------------------------------------

    fn dom_set_text(&mut self, id: &str, text: &str) {
        if let Some(f) = self.table.dom_set_text {
            let (ip, il) = as_parts(id);
            let (tp, tl) = as_parts(text);
            f(self.table.data, ip, il, tp, tl);
        }
    }
    fn dom_get_text(&mut self, id: &str) -> Option<String> {
        let f = self.table.dom_get_text?;
        let (ip, il) = as_parts(id);
        let mut out_len: usize = 0;
        let ptr = f(self.table.data, ip, il, &mut out_len);
        if ptr.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, out_len) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
    fn dom_add_listener(&mut self, id: &str, event: &str, cb: u32) {
        if let Some(f) = self.table.dom_add_listener {
            let (ip, il) = as_parts(id);
            let (ep, el) = as_parts(event);
            f(self.table.data, ip, il, ep, el, cb);
        }
    }
}

pub struct MsyContext {
    /// See the module doc: re-entrant by contract, single-threaded by
    /// contract, so interior mutability forms a call stack, never an overlap.
    interp: UnsafeCell<Interp>,
    error_cb: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,
    error_data: *mut c_void,
    /// Backing store for strings the engine returns to the host — valid until
    /// the next msy_* call on this context, which is the documented contract.
    scratch: UnsafeCell<String>,
    /// The browser-console REPL session (msy_context_repl_turn) — one
    /// growing, always-typechecked module against this context's engine.
    repl: UnsafeCell<mersey_interp::ReplSession>,
    /// Debugger state, SHARED with the installed hook rather than owned by
    /// it: the hook lives inside the Interp, but the host reaches the same
    /// controller through the context — including re-entrantly, from inside
    /// the paused callback, which is where DevTools sets the next step.
    debug: Rc<RefCell<DebugState>>,
}

/// The engine's evaluate-in-frame closure, lifetime-erased. It borrows the
/// interpreter and the paused frames, so it is only valid for the duration of
/// the blocking on-paused callout (see `CDebugHook::on_stmt`).
type DebugEvalFn<'a> = dyn FnMut(usize, &str) -> Result<String, String> + 'a;

/// Breakpoint/step policy plus the depth of the pause being serviced.
#[derive(Default)]
struct DebugState {
    ctl: DebugController,
    /// Frame count of the current pause; 0 when running. Keeping it here is
    /// what lets `msy_context_debug_step_over/out` take no depth argument —
    /// the engine knows it, so no host has to.
    depth: usize,
    /// A raw pointer to the engine's evaluate-in-frame closure, valid ONLY while
    /// blocked inside the on-paused callback (null otherwise). `msy_context_debug_evaluate`
    /// reads it. Sound because pausing is blocking and single-threaded: the
    /// closure on the engine's stack outlives every call made from the callout.
    /// `None` when running (a fat pointer has no cheap null, hence the Option).
    eval_ptr: Option<*mut DebugEvalFn<'static>>,
}

/// The C-ABI debugger hook: decide with the shared controller, then hand the
/// host one JSON snapshot and BLOCK in its callback.
struct CDebugHook {
    state: Rc<RefCell<DebugState>>,
    on_paused: MsyDebugPausedFn,
    data: *mut c_void,
}

impl DebugHook for CDebugHook {
    fn on_stmt(
        &mut self,
        pause: &DebugPause,
        locals: &mut dyn FnMut(usize) -> Vec<Vec<(String, String)>>,
        eval: &mut dyn FnMut(usize, &str) -> Result<String, String>,
    ) {
        // Borrow, decide, release — the callback below re-enters through the
        // context and must find the RefCell free.
        let stop = self.state.borrow_mut().ctl.should_stop(pause);
        let Some(reason) = stop else { return };

        let json = pause_json(reason, pause, locals);
        {
            let mut st = self.state.borrow_mut();
            st.depth = pause.frames.len();
            // A host that inspects and returns without choosing gets a plain
            // resume; anything else it wants, it sets from inside the call.
            st.ctl.resume();
            // Publish the evaluator for msy_context_debug_evaluate; it is valid
            // only until on_paused returns (cleared below). Lifetime-erased —
            // sound because the callout blocks on this thread.
            let e: *mut DebugEvalFn = eval;
            st.eval_ptr = Some(unsafe {
                std::mem::transmute::<*mut DebugEvalFn, *mut DebugEvalFn<'static>>(e)
            });
        }
        // Blocks for as long as the host keeps the engine paused.
        let (p, l) = as_parts(&json);
        (self.on_paused)(self.data, p, l);
        let mut st = self.state.borrow_mut();
        st.eval_ptr = None;
        st.depth = 0;
    }
}

/// The pause snapshot documented in `mersey.h`. Scopes for every frame are
/// materialized here: a stop is human-paced, so paying once beats making the
/// host re-enter the engine (and re-entering mid-callout is exactly what the
/// hook's borrow discipline forbids).
fn pause_json(
    reason: StopReason,
    pause: &DebugPause,
    locals: &mut dyn FnMut(usize) -> Vec<Vec<(String, String)>>,
) -> String {
    use mersey_interp::webjson::write_str;
    let mut out = String::from("{\"reason\":");
    write_str(&mut out, reason.as_str());
    out.push_str(",\"frames\":[");
    for (i, f) in mersey_interp::debug::frame_infos(pause).iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        write_str(&mut out, &f.name);
        out.push_str(",\"module\":");
        write_str(&mut out, &f.module);
        out.push_str(&format!(",\"line\":{},\"column\":{}", f.line, f.col));
        out.push_str(",\"scopes\":[");
        let scopes = locals(i);
        for (si, scope) in scopes.iter().enumerate() {
            if si > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            write_str(
                &mut out,
                &mersey_interp::debug::scope_name(si, scopes.len()),
            );
            out.push_str(",\"variables\":[");
            for (vi, (name, value)) in scope.iter().enumerate() {
                if vi > 0 {
                    out.push(',');
                }
                out.push_str("{\"name\":");
                write_str(&mut out, name);
                out.push_str(",\"value\":");
                write_str(&mut out, value);
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

impl MsyContext {
    fn report(&self, msg: &str) {
        if let Some(f) = self.error_cb {
            let (p, l) = as_parts(msg);
            f(self.error_data, p, l);
        }
    }

    /// # Safety
    /// Single thread (ABI rule); re-entry nests like a call stack.
    #[allow(clippy::mut_from_ref)]
    unsafe fn interp(&self) -> &mut Interp {
        &mut *self.interp.get()
    }
}

/// # Safety
/// `host` points to a valid table built against MSY_ABI_VERSION; the copied
/// callbacks must remain callable for the context's lifetime.
#[no_mangle]
pub unsafe extern "C" fn msy_context_new(host: *const MsyHostTable) -> *mut MsyContext {
    msy_context_new_ex(host, 0)
}

/// # Safety
/// As `msy_context_new`.
#[no_mangle]
pub unsafe extern "C" fn msy_context_new_ex(
    host: *const MsyHostTable,
    flags: u32,
) -> *mut MsyContext {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let table = *host;
    let mut interp = new_interp(Box::new(CHost::new(table)));
    // Native contexts get Tier 1 unless the embedder's sandbox forbids
    // executable pages — then MSY_FLAG_NO_JIT keeps everything on Tier 0.
    #[cfg(feature = "jit")]
    if flags & MSY_FLAG_NO_JIT == 0 {
        interp.jit = Some(mersey_jit::hook);
    }
    // Without the `jit` feature there is no Tier 1 to install; every context is
    // interpreter-only, and MSY_FLAG_NO_JIT is redundant but harmless.
    #[cfg(not(feature = "jit"))]
    let _ = flags;
    Box::into_raw(Box::new(MsyContext {
        interp: UnsafeCell::new(interp),
        error_cb: table.error,
        error_data: table.data,
        scratch: UnsafeCell::new(String::new()),
        repl: UnsafeCell::new(mersey_interp::ReplSession::new()),
        debug: Rc::new(RefCell::new(DebugState::default())),
    }))
}

/// # Safety
/// `ctx` must be a pointer returned by `msy_context_new*`, not yet freed, and
/// not currently inside a call (do not free from inside a host hook).
#[no_mangle]
pub unsafe extern "C" fn msy_context_free(ctx: *mut MsyContext) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

/// # Safety
/// `ctx` valid; `src` points to `len` readable bytes. The returned pointer is
/// valid until the next msy_* call on `ctx`.
#[no_mangle]
pub unsafe extern "C" fn msy_context_scan_imports(
    ctx: *mut MsyContext,
    src: *const c_char,
    len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let Some(ctx) = ctx.as_ref() else {
        return std::ptr::null();
    };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    let scratch = &mut *ctx.scratch.get();
    *scratch = embed::scan_imports_json(bytes);
    if !out_len.is_null() {
        *out_len = scratch.len();
    }
    scratch.as_ptr() as *const c_char
}

/// # Safety
/// `ctx` valid; `payload` points to `len` readable bytes of loader JSON.
#[no_mangle]
pub unsafe extern "C" fn msy_context_run_graph(
    ctx: *mut MsyContext,
    payload: *const c_char,
    len: usize,
) -> u32 {
    let Some(ctx) = ctx.as_ref() else { return 2 };
    let bytes = std::slice::from_raw_parts(payload as *const u8, len);
    let text = String::from_utf8_lossy(bytes).into_owned();
    embed::run_graph_json(ctx.interp(), &text, &mut |msg| ctx.report(msg))
}

/// # Safety
/// `ctx` valid; `src` points to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn msy_context_run(
    ctx: *mut MsyContext,
    src: *const c_char,
    len: usize,
) -> u32 {
    let Some(ctx) = ctx.as_ref() else { return 2 };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    embed::run_single(ctx.interp(), "<host>", bytes, &mut |msg| ctx.report(msg))
}

/// # Safety
/// `ctx` valid. May be called re-entrantly from inside a host hook.
#[no_mangle]
pub unsafe extern "C" fn msy_context_invoke(ctx: *mut MsyContext, cb: u32) -> u32 {
    let Some(ctx) = ctx.as_ref() else { return 2 };
    match ctx.interp().invoke_callback(cb) {
        Ok(()) => 0,
        Err(t) => {
            let msg = ctx.interp().describe_thrown(&t);
            ctx.report(&msg);
            2
        }
    }
}

/// Invoke a Mersey closure with typed arguments (ABI v11).
///
/// The JSON door (`msy_context_invoke_args`) is still there and still correct;
/// this one carries the same information without the string. A host reduces a
/// callback's arguments to scalars and handles *before* it can serialise them,
/// so the JSON was only ever a transport — one `JSONArray` built, one string
/// allocated and UTF-8 encoded, one parse on the other side, per callback. Every
/// promise in an async workload pays that.
///
/// # Safety
/// `ctx` valid; `args` points to `argc` readable `MsyArg16`, each string
/// argument pointing at `str16_len` readable UTF-16 units for the duration of
/// the call. May be called re-entrantly from inside a host hook.
#[no_mangle]
pub unsafe extern "C" fn msy_context_invoke16(
    ctx: *mut MsyContext,
    cb: u32,
    args: *const MsyArg16,
    argc: usize,
) -> u32 {
    let Some(ctx) = ctx.as_ref() else { return 2 };
    let items: Vec<mersey_interp::Value> = if args.is_null() || argc == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(args, argc)
            .iter()
            .map(|a| msy_arg16_to_value(a))
            .collect()
    };
    match ctx.interp().invoke_callback_args(cb, items) {
        Ok(()) => 0,
        Err(t) => {
            let msg = ctx.interp().describe_thrown(&t);
            ctx.report(&msg);
            2
        }
    }
}

/// One `MsyArg16` as the engine's own value. The kinds are the wide tier's:
/// 0 string, 1 number, 2 host handle, 3 bool, anything else null.
///
/// # Safety
/// A kind-0 argument's `str16` must point at `str16_len` readable units.
unsafe fn msy_arg16_to_value(a: &MsyArg16) -> mersey_interp::Value {
    match a.kind {
        0 if !a.str16.is_null() => mersey_interp::Value::Str(std::rc::Rc::new(
            std::slice::from_raw_parts(a.str16, a.str16_len).to_vec(),
        )),
        0 => mersey_interp::Value::Str(std::rc::Rc::new(Vec::new())),
        1 => mersey_interp::Value::F64(a.num),
        2 => mersey_interp::Value::JsRef(a.num as i64),
        3 => mersey_interp::Value::Bool(a.num != 0.0),
        _ => mersey_interp::Value::Null,
    }
}

/// # Safety
/// `ctx` valid; `args` points to `len` readable bytes of a JSON array. May be
/// called re-entrantly from inside a host hook.
#[no_mangle]
pub unsafe extern "C" fn msy_context_invoke_args(
    ctx: *mut MsyContext,
    cb: u32,
    args: *const c_char,
    len: usize,
) -> u32 {
    let Some(ctx) = ctx.as_ref() else { return 2 };
    let bytes = std::slice::from_raw_parts(args as *const u8, len);
    let json = String::from_utf8_lossy(bytes).into_owned();
    match ctx.interp().invoke_callback_json(cb, &json) {
        Ok(()) => 0,
        Err(t) => {
            let msg = ctx.interp().describe_thrown(&t);
            ctx.report(&msg);
            2
        }
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
/// One browser-console REPL turn (ReplSession semantics: one growing module,
/// re-checked whole each turn, executing only the new items — see the engine
/// crate). Reply, valid until the next msy_* call on this context: the echo
/// text ("" when none; "runtime error:"-prefixed when the accepted turn
/// threw), or "!"-prefixed diagnostics for a rejected turn.
///
/// # Safety
/// `ctx` from msy_context_new; `src` points at `len` readable bytes.
pub unsafe extern "C" fn msy_context_repl_turn(
    ctx: *mut MsyContext,
    src: *const c_char,
    len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let ctx = &*ctx;
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    let fragment = String::from_utf8_lossy(bytes);
    let text = {
        use mersey_interp::ReplOutcome;
        let session = &mut *ctx.repl.get();
        match session.turn(ctx.interp(), &fragment) {
            ReplOutcome::Ran(Some(echo)) => echo,
            ReplOutcome::Ran(None) => String::new(),
            ReplOutcome::Threw(msg) => msg,
            ReplOutcome::Rejected(diags) => format!("!{diags}"),
        }
    };
    let scratch = &mut *ctx.scratch.get();
    *scratch = text;
    *out_len = scratch.len();
    scratch.as_ptr() as *const c_char
}

/// The REPL session's visible top-level names as a JSON array (see
/// msy_context_repl_turn). Same reply lifetime as every other string.
///
/// # Safety
/// `ctx` from msy_context_new.
#[no_mangle]
pub unsafe extern "C" fn msy_context_repl_complete(
    ctx: *mut MsyContext,
    out_len: *mut usize,
) -> *const c_char {
    let ctx = &*ctx;
    let names = (*ctx.repl.get()).completions();
    let mut json = String::from("[");
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        mersey_interp::webjson::write_str(&mut json, n);
    }
    json.push(']');
    let scratch = &mut *ctx.scratch.get();
    *scratch = json;
    *out_len = scratch.len();
    scratch.as_ptr() as *const c_char
}

/* ---- Debugger (see the header for the contract) --------------------------- */

/// The paused callback. It BLOCKS: the engine sits mid-statement until it
/// returns, which is how a host holds a pause (nested message loop).
pub type MsyDebugPausedFn = extern "C" fn(*mut c_void, *const c_char, usize);

/// Attach a debugger. Forces the tree-walker for sync code.
///
/// # Safety
/// `ctx` from msy_context_new; not callable from inside the paused callback
/// (installing a hook from within one would drop the running hook).
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_enable(
    ctx: *mut MsyContext,
    on_paused: Option<MsyDebugPausedFn>,
    data: *mut c_void,
) {
    let Some(ctx) = ctx.as_ref() else { return };
    // A NULL callback has no way to hold a pause, so it is a no-op rather
    // than a hook that stops the world with nobody listening.
    let Some(on_paused) = on_paused else { return };
    ctx.interp().set_debug_hook(Box::new(CDebugHook {
        state: ctx.debug.clone(),
        on_paused,
        data,
    }));
}

/// Detach and restore the VM tier.
///
/// # Safety
/// `ctx` valid; not callable from inside the paused callback.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_disable(ctx: *mut MsyContext) {
    let Some(ctx) = ctx.as_ref() else { return };
    ctx.interp().clear_debug_hook();
    let mut st = ctx.debug.borrow_mut();
    st.ctl.clear_breakpoints();
    st.ctl.resume();
}

/// REPLACE the breakpoint set for one source.
///
/// # Safety
/// `ctx` valid; `source` points at `source_len` readable bytes (may be NULL
/// when `source_len` is 0); `lines` points at `count` readable `uint32_t`.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_set_breakpoints(
    ctx: *mut MsyContext,
    source: *const c_char,
    source_len: usize,
    lines: *const u32,
    count: usize,
) {
    let Some(ctx) = ctx.as_ref() else { return };
    let src = if source.is_null() || source_len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(source as *const u8, source_len))
            .into_owned()
    };
    let lines = if lines.is_null() || count == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(lines, count)
    };
    ctx.debug.borrow_mut().ctl.set_breakpoints(&src, lines);
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_clear_breakpoints(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.debug.borrow_mut().ctl.clear_breakpoints();
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_pause(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.debug.borrow_mut().ctl.request_pause();
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_resume(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.debug.borrow_mut().ctl.resume();
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_step_over(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        let mut st = ctx.debug.borrow_mut();
        let depth = st.depth;
        st.ctl.step_over(depth);
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_step_in(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.debug.borrow_mut().ctl.step_in();
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_step_out(ctx: *mut MsyContext) {
    if let Some(ctx) = ctx.as_ref() {
        let mut st = ctx.debug.borrow_mut();
        let depth = st.depth;
        st.ctl.step_out(depth);
    }
}

/// Evaluate an expression against a paused frame's live scope — the debug
/// console's evaluate-in-frame. Only meaningful while the engine is paused
/// (called from inside the on-paused callback); it returns `!not paused`
/// otherwise. `frame` is 0 for the innermost (paused) frame, counting outward.
/// The reply is the display result on success, or an error prefixed with `!`
/// (a parse error, a runtime throw, an unbound name, or "not paused"). Runs
/// with runtime semantics, no static re-check. Same reply lifetime as every
/// other string: valid until the next call on this context.
///
/// # Safety
/// `ctx` valid; `expr` points at `expr_len` readable bytes (may be NULL when
/// `expr_len` is 0); `out_len` is written.
#[no_mangle]
pub unsafe extern "C" fn msy_context_debug_evaluate(
    ctx: *mut MsyContext,
    frame: u32,
    expr: *const c_char,
    expr_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let ctx = &*ctx;
    let expr_str = if expr.is_null() || expr_len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(expr as *const u8, expr_len))
            .into_owned()
    };
    // Copy the pointer out and drop the borrow before the call: the evaluator
    // re-enters the interpreter, and the RefCell must be free (same discipline
    // as every other paused-callout re-entry).
    let ptr = ctx.debug.borrow().eval_ptr;
    let text = match ptr {
        None => "!not paused".to_string(),
        Some(p) => match (&mut *p)(frame as usize, &expr_str) {
            Ok(v) => v,
            Err(e) => format!("!{e}"),
        },
    };
    let scratch = &mut *ctx.scratch.get();
    *scratch = text;
    *out_len = scratch.len();
    scratch.as_ptr() as *const c_char
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_release_callback(ctx: *mut MsyContext, cb: u32) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.interp().release_callback(cb);
    }
}
