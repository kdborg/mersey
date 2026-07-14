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

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::os::raw::c_char;

use mersey_interp::{embed, new_interp, Host, Interp};

/// Bumped whenever the table layout or a boundary contract changes. The
/// embedder checks before installing a table.
pub const MSY_ABI_VERSION: u32 = 2;

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
    pub print_level:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *const c_char, usize)>,
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
pub unsafe extern "C" fn msy_context_release_callback(ctx: *mut MsyContext, cb: u32) {
    if let Some(ctx) = ctx.as_ref() {
        ctx.interp().release_callback(cb);
    }
}
