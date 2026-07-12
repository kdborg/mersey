//! Stage A engine (docs/architecture/browser-integration.md): the frontend
//! and MVP interpreter compiled to `wasm32-unknown-unknown`, exposed through
//! a minimal hand-rolled ABI so the loader needs no bindgen tooling.
//!
//! ABI (all strings UTF-8; `ptr` are offsets into the module's memory):
//!   exports:
//!     msy_alloc(len) -> ptr            allocate a buffer the host may write
//!     msy_run(ptr, len) -> status      compile + execute one module
//!     msy_invoke(cb) -> status         fire a registered event callback
//!   status: 0 ok, 1 compile diagnostics (reported via host_error), 2 runtime error
//!   imports (module "env"):
//!     host_print(ptr, len)
//!     host_error(ptr, len)
//!     host_dom_set_text(id_ptr, id_len, txt_ptr, txt_len)
//!     host_dom_get_text(id_ptr, id_len) -> u64   (ptr<<32 | len; 0 = absent;
//!                                                 host writes via msy_alloc)
//!     host_dom_on_click(id_ptr, id_len, cb)
//!
//! Memory notes (MVP): `msy_alloc` buffers and each `host_dom_get_text`
//! reply are intentionally leaked — bounded by script size and DOM reads,
//! acceptable for Stage A; a proper arena arrives with the real engine.

use std::cell::UnsafeCell;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host, Interp};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_print(ptr: *const u8, len: usize);
    fn host_error(ptr: *const u8, len: usize);
    fn host_dom_set_text(id_ptr: *const u8, id_len: usize, txt_ptr: *const u8, txt_len: usize);
    fn host_dom_get_text(id_ptr: *const u8, id_len: usize) -> u64;
    fn host_dom_on_click(id_ptr: *const u8, id_len: usize, cb: u32);
    fn host_dom_create(tag_ptr: *const u8, tag_len: usize) -> u64;
    fn host_dom_append(p_ptr: *const u8, p_len: usize, c_ptr: *const u8, c_len: usize);
    fn host_dom_remove(id_ptr: *const u8, id_len: usize);
    fn host_dom_get_value(id_ptr: *const u8, id_len: usize) -> u64;
    fn host_dom_set_value(id_ptr: *const u8, id_len: usize, v_ptr: *const u8, v_len: usize);

    // Universal web bridge: JSON in, JSON out (all returns are packed
    // (ptr<<32|len) buffers allocated with msy_alloc by the host).
    fn host_web_global(n_ptr: *const u8, n_len: usize) -> i64;
    fn host_web_get(target: i64, p_ptr: *const u8, p_len: usize) -> u64;
    fn host_web_set(
        target: i64,
        p_ptr: *const u8,
        p_len: usize,
        v_ptr: *const u8,
        v_len: usize,
    ) -> u64;
    fn host_web_call(
        target: i64,
        m_ptr: *const u8,
        m_len: usize,
        a_ptr: *const u8,
        a_len: usize,
    ) -> u64;
    fn host_web_new(c_ptr: *const u8, c_len: usize, a_ptr: *const u8, a_len: usize) -> u64;
}

fn read_packed(packed: u64) -> String {
    if packed == 0 {
        return String::new();
    }
    let ptr = (packed >> 32) as usize as *const u8;
    let len = (packed & 0xFFFF_FFFF) as usize;
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn send(f: unsafe extern "C" fn(*const u8, usize), s: &str) {
    unsafe { f(s.as_ptr(), s.len()) }
}

struct WasmHost;

impl Host for WasmHost {
    fn print(&mut self, s: &str) {
        send(host_print, s);
    }
    fn dom_set_text(&mut self, id: &str, text: &str) {
        unsafe { host_dom_set_text(id.as_ptr(), id.len(), text.as_ptr(), text.len()) }
    }
    fn dom_get_text(&mut self, id: &str) -> Option<String> {
        let packed = unsafe { host_dom_get_text(id.as_ptr(), id.len()) };
        if packed == 0 {
            return None;
        }
        let ptr = (packed >> 32) as usize as *const u8;
        let len = (packed & 0xFFFF_FFFF) as usize;
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
    fn dom_on_click(&mut self, id: &str, cb: u32) {
        unsafe { host_dom_on_click(id.as_ptr(), id.len(), cb) }
    }
    fn dom_create(&mut self, tag: &str) -> String {
        read_packed(unsafe { host_dom_create(tag.as_ptr(), tag.len()) })
    }
    fn dom_append(&mut self, parent: &str, child: &str) {
        unsafe { host_dom_append(parent.as_ptr(), parent.len(), child.as_ptr(), child.len()) }
    }
    fn dom_remove(&mut self, id: &str) {
        unsafe { host_dom_remove(id.as_ptr(), id.len()) }
    }
    fn dom_get_value(&mut self, id: &str) -> String {
        read_packed(unsafe { host_dom_get_value(id.as_ptr(), id.len()) })
    }
    fn dom_set_value(&mut self, id: &str, value: &str) {
        unsafe { host_dom_set_value(id.as_ptr(), id.len(), value.as_ptr(), value.len()) }
    }

    fn web_global(&mut self, name: &str) -> i64 {
        unsafe { host_web_global(name.as_ptr(), name.len()) }
    }
    fn web_get(&mut self, target: i64, prop: &str) -> String {
        read_packed(unsafe { host_web_get(target, prop.as_ptr(), prop.len()) })
    }
    fn web_set(&mut self, target: i64, prop: &str, value_json: &str) -> String {
        read_packed(unsafe {
            host_web_set(
                target,
                prop.as_ptr(),
                prop.len(),
                value_json.as_ptr(),
                value_json.len(),
            )
        })
    }
    fn web_call(&mut self, target: i64, method: &str, args_json: &str) -> String {
        read_packed(unsafe {
            host_web_call(
                target,
                method.as_ptr(),
                method.len(),
                args_json.as_ptr(),
                args_json.len(),
            )
        })
    }
    fn web_new(&mut self, ctor: &str, args_json: &str) -> String {
        read_packed(unsafe {
            host_web_new(ctor.as_ptr(), ctor.len(), args_json.as_ptr(), args_json.len())
        })
    }
}

thread_local! {
    static INTERP: UnsafeCell<Option<Interp>> = const { UnsafeCell::new(None) };
}

/// Access the engine, tolerating **re-entrancy**: a bridge call can make the
/// host call straight back into us (a `new Promise(executor)` runs its
/// executor synchronously; a JS array callback invokes a Mersey closure), so
/// the engine must be usable from inside its own call. WASM is
/// single-threaded and the engine never moves once created, so this forms a
/// simple call stack — a `RefCell` would spuriously panic here.
fn with_interp<R>(f: impl FnOnce(&mut Interp) -> R) -> Option<R> {
    INTERP.with(|cell| {
        let slot = unsafe { &mut *cell.get() };
        slot.as_mut().map(f)
    })
}

fn ensure_interp() {
    INTERP.with(|cell| {
        let slot = unsafe { &mut *cell.get() };
        if slot.is_none() {
            *slot = Some(new_interp(Box::new(WasmHost)));
        }
    });
}

/// Allocate `len` bytes the host can write into (deliberately leaked).
#[no_mangle]
pub extern "C" fn msy_alloc(len: usize) -> *mut u8 {
    let mut buf = vec![0u8; len.max(1)];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn msy_run(ptr: *const u8, len: usize) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        send(host_error, &format!("engine panic: {info}"));
    }));
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };

    let src = match source::decode("<script>", bytes) {
        Ok(s) => s,
        Err(d) => {
            send(host_error, &d.to_string());
            return 1;
        }
    };
    let parsed = parser::parse(&src);
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        diags = check::check(&parsed.module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            send(host_error, &d.to_string());
        }
        return 1;
    }

    // One module per page lifetime; the AST lives as long as its callbacks.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    ensure_interp();
    with_interp(|interp| match interp.run_module(module) {
        Ok(()) => 0,
        Err(t) => {
            let msg = interp.describe_thrown(&t);
            send(host_error, &msg);
            2
        }
    })
    .unwrap_or(2)
}

/// Fire a callback with JSON arguments (event objects, promise values).
#[no_mangle]
pub extern "C" fn msy_invoke_args(cb: u32, ptr: *const u8, len: usize) -> u32 {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let args = String::from_utf8_lossy(bytes).into_owned();
    with_interp(|interp| match interp.invoke_callback_json(cb, &args) {
        Ok(()) => 0,
        Err(t) => {
            let msg = interp.describe_thrown(&t);
            send(host_error, &msg);
            2
        }
    })
    .unwrap_or_else(|| {
        send(host_error, "no script loaded");
        2
    })
}

#[no_mangle]
pub extern "C" fn msy_invoke(cb: u32) -> u32 {
    with_interp(|interp| match interp.invoke_callback(cb) {
        Ok(()) => 0,
        Err(t) => {
            let msg = interp.describe_thrown(&t);
            send(host_error, &msg);
            2
        }
    })
    .unwrap_or_else(|| {
        send(host_error, "no script loaded");
        2
    })
}
