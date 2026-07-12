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

use std::cell::RefCell;

use mersey_front::{bind, parser, source};
use mersey_interp::{new_interp, Host, Interp};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_print(ptr: *const u8, len: usize);
    fn host_error(ptr: *const u8, len: usize);
    fn host_dom_set_text(id_ptr: *const u8, id_len: usize, txt_ptr: *const u8, txt_len: usize);
    fn host_dom_get_text(id_ptr: *const u8, id_len: usize) -> u64;
    fn host_dom_on_click(id_ptr: *const u8, id_len: usize, cb: u32);
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
}

thread_local! {
    static INTERP: RefCell<Option<Interp>> = const { RefCell::new(None) };
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
    let diags = if parsed.diagnostics.is_empty() {
        bind::bind(&parsed.module).diagnostics
    } else {
        parsed.diagnostics
    };
    if !diags.is_empty() {
        for d in &diags {
            send(host_error, &d.to_string());
        }
        return 1;
    }

    // One module per page lifetime; the AST lives as long as its callbacks.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    INTERP.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(new_interp(Box::new(WasmHost)));
        }
        let interp = slot.as_mut().expect("interp");
        match interp.run_module(module) {
            Ok(()) => 0,
            Err(t) => {
                let msg = interp.describe_thrown(&t);
                send(host_error, &msg);
                2
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn msy_invoke(cb: u32) -> u32 {
    INTERP.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(interp) = slot.as_mut() else {
            send(host_error, "no script loaded");
            return 2;
        };
        match interp.invoke_callback(cb) {
            Ok(()) => 0,
            Err(t) => {
                let msg = interp.describe_thrown(&t);
                send(host_error, &msg);
                2
            }
        }
    })
}
