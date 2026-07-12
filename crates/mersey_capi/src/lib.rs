//! Stage B embedding API implementation (see include/mersey.h and
//! docs/architecture/embedding-api.md). This is the boundary Chromium's
//! `//components/mersey` wraps — proven here by a plain-C host
//! (native/host_demo.c) with no V8 and no WASM anywhere in the stack.

use std::ffi::c_void;
use std::os::raw::c_char;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host, Interp};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsyHostTable {
    pub data: *mut c_void,
    pub print: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,
    pub error: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,
    pub dom_set_text:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *const c_char, usize)>,
    pub dom_get_text:
        Option<extern "C" fn(*mut c_void, *const c_char, usize, *mut usize) -> *const c_char>,
    pub dom_on_click: Option<extern "C" fn(*mut c_void, *const c_char, usize, u32)>,
}

struct CHost {
    table: MsyHostTable,
}

fn as_parts(s: &str) -> (*const c_char, usize) {
    (s.as_ptr() as *const c_char, s.len())
}

impl Host for CHost {
    fn print(&mut self, s: &str) {
        if let Some(f) = self.table.print {
            let (p, l) = as_parts(s);
            f(self.table.data, p, l);
        }
    }
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
    fn dom_on_click(&mut self, id: &str, cb: u32) {
        if let Some(f) = self.table.dom_on_click {
            let (ip, il) = as_parts(id);
            f(self.table.data, ip, il, cb);
        }
    }
}

pub struct MsyContext {
    interp: Interp,
    error_cb: Option<extern "C" fn(*mut c_void, *const c_char, usize)>,
    error_data: *mut c_void,
}

impl MsyContext {
    fn report(&self, msg: &str) {
        if let Some(f) = self.error_cb {
            let (p, l) = as_parts(msg);
            f(self.error_data, p, l);
        }
    }
}

/// # Safety
/// `host` must point to a valid table; the copied callbacks must remain
/// callable for the context's lifetime.
#[no_mangle]
pub unsafe extern "C" fn msy_context_new(host: *const MsyHostTable) -> *mut MsyContext {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let table = *host;
    let mut interp = new_interp(Box::new(CHost { table }));
    // Native contexts get the Tier 1 JIT, exactly like the CLI.
    interp.jit = Some(mersey_jit::hook);
    Box::into_raw(Box::new(MsyContext {
        interp,
        error_cb: table.error,
        error_data: table.data,
    }))
}

/// # Safety
/// `ctx` must be a pointer returned by `msy_context_new`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn msy_context_free(ctx: *mut MsyContext) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

/// # Safety
/// `ctx` valid; `src` points to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn msy_context_run(
    ctx: *mut MsyContext,
    src: *const c_char,
    len: usize,
) -> u32 {
    let Some(ctx) = ctx.as_mut() else { return 2 };
    let bytes = std::slice::from_raw_parts(src as *const u8, len);
    let decoded = match source::decode("<host>", bytes) {
        Ok(s) => s,
        Err(d) => {
            ctx.report(&d.to_string());
            return 1;
        }
    };
    let parsed = parser::parse(&decoded);
    let mut diags = parsed.diagnostics;
    if diags.is_empty() {
        diags = bind::bind(&parsed.module).diagnostics;
    }
    if diags.is_empty() {
        diags = check::check(&parsed.module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            ctx.report(&d.to_string());
        }
        return 1;
    }
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    match ctx.interp.run_module(module) {
        Ok(()) => 0,
        Err(t) => {
            let msg = ctx.interp.describe_thrown(&t);
            ctx.report(&msg);
            2
        }
    }
}

/// # Safety
/// `ctx` valid.
#[no_mangle]
pub unsafe extern "C" fn msy_context_invoke(ctx: *mut MsyContext, cb: u32) -> u32 {
    let Some(ctx) = ctx.as_mut() else { return 2 };
    match ctx.interp.invoke_callback(cb) {
        Ok(()) => 0,
        Err(t) => {
            let msg = ctx.interp.describe_thrown(&t);
            ctx.report(&msg);
            2
        }
    }
}
