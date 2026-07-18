/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Native Mersey engine hosted inside Servo — the "native" leg of the
//! `bench/web` benchmark, the Servo counterpart of the Gecko fork's
//! `dom/mersey/MerseyScriptRunner` and the Chromium fork's `//components/mersey`.
//!
//! A `<script type="text/mersey">` runs in the engine directly (Rust
//! interpreter + Cranelift JIT), *not* as WASM and *not* as JS. Only the actual
//! web-API calls cross into Servo's SpiderMonkey realm, through the same
//! reflective bridge the WASM polyfill uses (`web/mersey-bridge.js`, embedded
//! here as `bridge.js`): five reflective operations — global / get / set / call
//! / construct — reach any object in the JS realm, with a handle table for
//! object identity. This is the honest bootstrap; the direct-C++ typed fast
//! paths the other forks grew are left for later (every fast-path pointer in the
//! host table is NULL, so the engine falls back to these reflective ops — same
//! results, verified by matching workload checksums).
//!
//! Threading/re-entrancy: one engine context per script thread, always called
//! from that thread; a bridge call the engine makes can re-enter (a JS callback
//! invoking a Mersey closure), so the runner is reached through a raw pointer,
//! not a `RefCell` (which would panic on the legitimate re-entrant stack — the
//! same discipline `mersey_capi`'s own `MsyContext` uses).

#![allow(unsafe_code)]

use std::borrow::Cow;
use std::cell::Cell;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::ptr::{self, NonNull};
use std::time::Instant;

use js::context::JSContext;
use js::conversions::{jsstr_to_string, Utf8Chars};
use js::jsapi::{CallArgs, JSObject, Value};
use js::jsval::{DoubleValue, StringValue, UndefinedValue};
use js::rust::wrappers2::{
    JS_CallFunctionName, JS_DefineFunction, JS_GetProperty, JS_NewStringCopyUTF8N,
};
use js::rust::HandleObject;
use script_bindings::reflector::DomObject;

use mersey_capi::{
    msy_context_invoke_args, msy_context_new, msy_context_run, MsyContext, MsyHostTable, MsyScalar,
};

use crate::dom::globalscope::GlobalScope;
use crate::realms::enter_auto_realm;

/// The reflective bridge JS, generated from `web/mersey-bridge.js` (import
/// stripped, `globalThis.__merseyBridge = makeBridge(...)` epilogue appended).
const BRIDGE_JS: &str = include_str!("bridge.js");

/// Capabilities granted to the engine (spec §5.4). Matches the Gecko fork:
/// the whole web surface is reachable, the engine still gates each API by import.
const CAPS: &str = "[\"dom\",\"web\",\"time\",\"random\",\"net\",\"storage\"]";

/// Per-thread engine runner. Reached through a raw pointer (see module doc).
struct Runner {
    ctx: *mut MsyContext,
    /// The page's global object (kept alive by the realm for the page lifetime).
    global: *mut JSObject,
    bridge_ready: bool,
    /// Backing store for a reply the engine reads — valid until the next host
    /// call on this runner, exactly the C-ABI contract.
    scratch: String,
    start: Instant,
}

thread_local! {
    static RUNNER: Cell<*mut Runner> = const { Cell::new(ptr::null_mut()) };
}

fn runner_ptr() -> *mut Runner {
    RUNNER.with(|c| c.get())
}

// ---- host table shims -----------------------------------------------------

extern "C" fn host_print(_data: *mut c_void, utf8: *const c_char, len: usize) {
    use std::io::Write;
    let bytes = unsafe { std::slice::from_raw_parts(utf8 as *const u8, len) };
    let out = std::io::stdout();
    let mut h = out.lock();
    let _ = h.write_all(bytes);
    let _ = h.write_all(b"\n");
    let _ = h.flush();
}

extern "C" fn host_caps(_data: *mut c_void, out_len: *mut usize) -> *const c_char {
    unsafe { *out_len = CAPS.len() };
    CAPS.as_ptr() as *const c_char
}

extern "C" fn host_time_ms(_data: *mut c_void, epoch: i32) -> f64 {
    if epoch != 0 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    } else {
        let r = runner_ptr();
        if r.is_null() {
            0.0
        } else {
            unsafe {
                let start = &(*r).start;
                start.elapsed().as_secs_f64() * 1000.0
            }
        }
    }
}

/// Call `__merseyBridge[method](args...)` with string args; the reply string
/// lands in the runner's scratch buffer (valid until the next host call).
unsafe fn call_bridge_str(method: &CStr, args: &[&str], out_len: *mut usize) -> *const c_char {
    let r = runner_ptr();
    if r.is_null() {
        *out_len = 0;
        return ptr::null();
    }
    let reply = call_bridge_string(method, args);
    (*r).scratch = reply;
    // Explicit reference so the method calls below don't autoref off a raw deref.
    let scratch: &String = &(*r).scratch;
    *out_len = scratch.len();
    scratch.as_ptr() as *const c_char
}

/// The shared body: fetch `__merseyBridge`, call `method`, return its string.
unsafe fn call_bridge_string(method: &CStr, args: &[&str]) -> String {
    let r = runner_ptr();
    if r.is_null() {
        return String::new();
    }
    let Some(mut cx) = JSContext::get_from_thread() else {
        return String::new();
    };
    let cx = &mut cx;
    let global = HandleObject::from_marked_location(&(*r).global);

    rooted!(&in(cx) let mut bridge_val = UndefinedValue());
    if !JS_GetProperty(cx, global, c"__merseyBridge".as_ptr(), bridge_val.handle_mut())
        || !bridge_val.is_object()
    {
        return String::new();
    }
    rooted!(&in(cx) let bridge_obj = bridge_val.to_object());

    rooted_vec!(let mut argv);
    for a in args {
        let chars = Utf8Chars::from(*a);
        let js = JS_NewStringCopyUTF8N(cx, &*chars as *const _);
        if js.is_null() {
            return String::new();
        }
        rooted!(&in(cx) let v = StringValue(&*js));
        argv.push(v.get());
    }

    rooted!(&in(cx) let mut rval = UndefinedValue());
    let hva = js::jsapi::HandleValueArray::from(&argv);
    let ok = JS_CallFunctionName(cx, bridge_obj.handle(), method.as_ptr(), &hva, rval.handle_mut());
    if ok && rval.is_string() {
        jsstr_to_string(cx, NonNull::new(rval.to_string()).unwrap())
    } else {
        String::new()
    }
}

/// Number-returning bridge call (`global`, `instanceOf`).
unsafe fn call_bridge_int(method: &CStr, args: &[&str]) -> i64 {
    let r = runner_ptr();
    if r.is_null() {
        return -1;
    }
    let Some(mut cx) = JSContext::get_from_thread() else {
        return -1;
    };
    let cx = &mut cx;
    let global = HandleObject::from_marked_location(&(*r).global);
    rooted!(&in(cx) let mut bridge_val = UndefinedValue());
    if !JS_GetProperty(cx, global, c"__merseyBridge".as_ptr(), bridge_val.handle_mut())
        || !bridge_val.is_object()
    {
        return -1;
    }
    rooted!(&in(cx) let bridge_obj = bridge_val.to_object());
    rooted_vec!(let mut argv);
    for a in args {
        let chars = Utf8Chars::from(*a);
        let js = JS_NewStringCopyUTF8N(cx, &*chars as *const _);
        if js.is_null() {
            return -1;
        }
        rooted!(&in(cx) let v = StringValue(&*js));
        argv.push(v.get());
    }
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let hva = js::jsapi::HandleValueArray::from(&argv);
    let ok = JS_CallFunctionName(cx, bridge_obj.handle(), method.as_ptr(), &hva, rval.handle_mut());
    if ok && rval.is_number() {
        rval.to_number() as i64
    } else {
        -1
    }
}

// ---- interned + scalar fast paths (ABI v3) --------------------------------
// A member name crosses the boundary once (web_intern), then only its integer
// id does, and scalar arguments cross as JS values — no per-call args JSON to
// build and parse. The bridge already implements the matching methods
// (intern / getId / callStr / callScalars / newScalars); these just forward,
// passing numbers as numbers and strings as strings instead of a JSON blob.

enum JsArg<'a> {
    Num(f64),
    Str(&'a str),
}

/// Call `__merseyBridge[method](args...)` with mixed number/string args; return
/// the reply string (empty on failure). Shared by the string-returning and
/// number-returning wrappers below.
unsafe fn call_bridge_vals(method: &CStr, args: &[JsArg]) -> Option<String> {
    let r = runner_ptr();
    if r.is_null() {
        return None;
    }
    let Some(mut cx) = JSContext::get_from_thread() else {
        return None;
    };
    let cx = &mut cx;
    let global = HandleObject::from_marked_location(&(*r).global);
    rooted!(&in(cx) let mut bridge_val = UndefinedValue());
    if !JS_GetProperty(cx, global, c"__merseyBridge".as_ptr(), bridge_val.handle_mut())
        || !bridge_val.is_object()
    {
        return None;
    }
    rooted!(&in(cx) let bridge_obj = bridge_val.to_object());
    rooted_vec!(let mut argv);
    for a in args {
        match a {
            JsArg::Num(n) => {
                rooted!(&in(cx) let v = DoubleValue(*n));
                argv.push(v.get());
            },
            JsArg::Str(s) => {
                let chars = Utf8Chars::from(*s);
                let js = JS_NewStringCopyUTF8N(cx, &*chars as *const _);
                if js.is_null() {
                    return None;
                }
                rooted!(&in(cx) let v = StringValue(&*js));
                argv.push(v.get());
            },
        }
    }
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let hva = js::jsapi::HandleValueArray::from(&argv);
    if !JS_CallFunctionName(cx, bridge_obj.handle(), method.as_ptr(), &hva, rval.handle_mut()) {
        return None;
    }
    if rval.is_string() {
        Some(jsstr_to_string(cx, NonNull::new(rval.to_string()).unwrap()))
    } else if rval.is_number() {
        // Numeric reply (intern) — encode so the number-returning wrapper reads it.
        Some(rval.to_number().to_string())
    } else {
        Some(String::new())
    }
}

/// Store a reply into the runner scratch and return its pointer (ABI lifetime).
unsafe fn reply(method: &CStr, args: &[JsArg], out_len: *mut usize) -> *const c_char {
    let r = runner_ptr();
    if r.is_null() {
        *out_len = 0;
        return ptr::null();
    }
    (*r).scratch = call_bridge_vals(method, args).unwrap_or_default();
    let scratch: &String = &(*r).scratch;
    *out_len = scratch.len();
    scratch.as_ptr() as *const c_char
}

extern "C" fn host_web_intern(_data: *mut c_void, name: *const c_char, len: usize) -> u32 {
    let name = unsafe { str_from(name, len) };
    match unsafe { call_bridge_vals(c"intern", &[JsArg::Str(name)]) } {
        Some(s) => s.parse::<f64>().map(|n| n as u32).unwrap_or(u32::MAX),
        None => u32::MAX,
    }
}

extern "C" fn host_web_get_id(
    _data: *mut c_void,
    target: i64,
    name_id: u32,
    out_len: *mut usize,
) -> *const c_char {
    unsafe { reply(c"getId", &[JsArg::Num(target as f64), JsArg::Num(name_id as f64)], out_len) }
}

extern "C" fn host_web_call_str(
    _data: *mut c_void,
    target: i64,
    name_id: u32,
    arg: *const c_char,
    arg_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let arg = unsafe { str_from(arg, arg_len) };
    unsafe {
        reply(
            c"callStr",
            &[JsArg::Num(target as f64), JsArg::Num(name_id as f64), JsArg::Str(arg)],
            out_len,
        )
    }
}

unsafe fn scalar_args<'a>(lead: &[JsArg<'a>], scalars: &'a [MsyScalar]) -> Vec<JsArg<'a>> {
    let mut v: Vec<JsArg> = lead.iter().map(|a| match a {
        JsArg::Num(n) => JsArg::Num(*n),
        JsArg::Str(s) => JsArg::Str(s),
    }).collect();
    for s in scalars {
        if s.is_num != 0 {
            v.push(JsArg::Num(s.num));
        } else {
            v.push(JsArg::Str(str_from(s.str_ptr, s.str_len)));
        }
    }
    v
}

extern "C" fn host_web_call_scalars(
    _data: *mut c_void,
    target: i64,
    name_id: u32,
    args: *const MsyScalar,
    argc: usize,
    out_len: *mut usize,
) -> *const c_char {
    unsafe {
        let scalars = if args.is_null() { &[][..] } else { std::slice::from_raw_parts(args, argc) };
        let v = scalar_args(&[JsArg::Num(target as f64), JsArg::Num(name_id as f64)], scalars);
        reply(c"callScalars", &v, out_len)
    }
}

extern "C" fn host_web_new_scalars(
    _data: *mut c_void,
    ctor_id: u32,
    args: *const MsyScalar,
    argc: usize,
    out_len: *mut usize,
) -> *const c_char {
    unsafe {
        let scalars = if args.is_null() { &[][..] } else { std::slice::from_raw_parts(args, argc) };
        let v = scalar_args(&[JsArg::Num(ctor_id as f64)], scalars);
        reply(c"newScalars", &v, out_len)
    }
}

extern "C" fn host_web_global(_data: *mut c_void, name: *const c_char, len: usize) -> i64 {
    let name = unsafe { str_from(name, len) };
    unsafe { call_bridge_int(c"global", &[name]) }
}

extern "C" fn host_web_get(
    _data: *mut c_void,
    target: i64,
    prop: *const c_char,
    prop_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let prop = unsafe { str_from(prop, prop_len) };
    let t = target.to_string();
    unsafe { call_bridge_str(c"get", &[&t, prop], out_len) }
}

extern "C" fn host_web_set(
    _data: *mut c_void,
    target: i64,
    prop: *const c_char,
    prop_len: usize,
    value_json: *const c_char,
    value_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let prop = unsafe { str_from(prop, prop_len) };
    let value = unsafe { str_from(value_json, value_len) };
    let t = target.to_string();
    unsafe { call_bridge_str(c"set", &[&t, prop, value], out_len) }
}

extern "C" fn host_web_call(
    _data: *mut c_void,
    target: i64,
    method: *const c_char,
    method_len: usize,
    args_json: *const c_char,
    args_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let method = unsafe { str_from(method, method_len) };
    let args = unsafe { str_from(args_json, args_len) };
    let t = target.to_string();
    unsafe { call_bridge_str(c"call", &[&t, method, args], out_len) }
}

extern "C" fn host_web_new(
    _data: *mut c_void,
    ctor: *const c_char,
    ctor_len: usize,
    args_json: *const c_char,
    args_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let ctor = unsafe { str_from(ctor, ctor_len) };
    let args = unsafe { str_from(args_json, args_len) };
    unsafe { call_bridge_str(c"construct", &[ctor, args], out_len) }
}

extern "C" fn host_web_iterate(
    _data: *mut c_void,
    target: i64,
    out_len: *mut usize,
) -> *const c_char {
    let t = target.to_string();
    unsafe { call_bridge_str(c"iterate", &[&t], out_len) }
}

extern "C" fn host_web_instanceof(_data: *mut c_void, target: i64, ctor: i64) -> i32 {
    let t = target.to_string();
    let c = ctor.to_string();
    unsafe { call_bridge_int(c"instanceOf", &[&t, &c]) as i32 }
}

extern "C" fn host_web_release(_data: *mut c_void, target: i64) {
    let t = target.to_string();
    let mut dummy: usize = 0;
    unsafe { call_bridge_str(c"release", &[&t], &mut dummy) };
}

unsafe fn str_from<'a>(p: *const c_char, len: usize) -> &'a str {
    if p.is_null() || len == 0 {
        return "";
    }
    let bytes = std::slice::from_raw_parts(p as *const u8, len);
    std::str::from_utf8(bytes).unwrap_or("")
}

/// `__merseyInvoke(cb, argsJson)` — the hook the bridge calls when JS invokes a
/// Mersey closure (a promise reaction, an event listener). Forwards into the
/// engine via `msy_context_invoke_args`.
unsafe extern "C" fn mersey_invoke(cx: *mut js::jsapi::JSContext, argc: u32, vp: *mut Value) -> bool {
    let args = CallArgs::from_vp(vp, argc);
    let r = runner_ptr();
    if !r.is_null() && argc >= 2 {
        let cb = args.get(0).to_number() as u32;
        let arg1 = args.get(1);
        let args_json = if arg1.is_string() {
            let cx = JSContext::from_ptr(NonNull::new(cx).unwrap());
            jsstr_to_string(&cx, NonNull::new(arg1.to_string()).unwrap())
        } else {
            String::from("[]")
        };
        let bytes = args_json.as_bytes();
        msy_context_invoke_args((*r).ctx, cb, bytes.as_ptr() as *const c_char, bytes.len());
    }
    args.rval().set(UndefinedValue());
    true
}

fn host_table() -> MsyHostTable {
    MsyHostTable {
        data: ptr::null_mut(),
        print: Some(host_print),
        print_level: None,
        error: None,
        caps: Some(host_caps),
        web_global: Some(host_web_global),
        web_get: Some(host_web_get),
        web_set: Some(host_web_set),
        web_call: Some(host_web_call),
        web_new: Some(host_web_new),
        web_iterate: Some(host_web_iterate),
        web_instanceof: Some(host_web_instanceof),
        web_release: Some(host_web_release),
        web_bytes_read: None,
        web_bytes_write: None,
        time_ms: Some(host_time_ms),
        random_bytes: None,
        dom_set_text: None,
        dom_get_text: None,
        dom_add_listener: None,
        // Interned + scalar fast paths: a name crosses once as an id, scalar args
        // cross as JS values (no per-call args JSON). Ops these don't cover
        // (object args, property sets) fall back to the reflective ops above.
        web_intern: Some(host_web_intern),
        web_get_id: Some(host_web_get_id),
        web_set_str: None,
        web_set_num: None,
        web_call_str: Some(host_web_call_str),
        web_call_scalars: Some(host_web_call_scalars),
        web_new_scalars: Some(host_web_new_scalars),
        // Wide-string (UTF-16) and typed-binding fast paths not built yet.
        web_get_u16: None,
        web_set_u16: None,
        web_call_u16: None,
        web_new_u16: None,
        web_bind: None,
    }
}

/// Create the engine context once per thread.
unsafe fn ensure_runner(global_obj: *mut JSObject) -> *mut Runner {
    let existing = runner_ptr();
    if !existing.is_null() {
        (*existing).global = global_obj;
        return existing;
    }
    let runner = Box::into_raw(Box::new(Runner {
        ctx: ptr::null_mut(),
        global: global_obj,
        bridge_ready: false,
        scratch: String::new(),
        start: Instant::now(),
    }));
    RUNNER.with(|c| c.set(runner));

    let mut table = host_table();
    table.data = runner as *mut c_void;
    (*runner).ctx = msy_context_new(&table);
    runner
}

/// Run one inline `<script type="text/mersey">` body in the engine.
pub(crate) fn run_mersey_script(global: &GlobalScope, cx: &mut JSContext, source: &str) {
    let mut realm = enter_auto_realm(cx, global);
    let cx = &mut realm.current_realm();
    let global_obj = global.reflector().get_jsobject().get();
    unsafe {
        let runner = ensure_runner(global_obj);
        if runner.is_null() || (*runner).ctx.is_null() {
            return;
        }
        // Inject __merseyInvoke and evaluate the reflective bridge, once.
        if !(*runner).bridge_ready {
            let global_handle = HandleObject::from_marked_location(&(*runner).global);
            let name = c"__merseyInvoke";
            let _ = JS_DefineFunction(cx, global_handle, name.as_ptr(), Some(mersey_invoke), 2, 0);
            let _ = global.evaluate_js_on_global(
                cx,
                Cow::Borrowed(BRIDGE_JS),
                "mersey-bridge.js",
                None,
                None,
            );
            (*runner).bridge_ready = true;
        }
        let src = source.as_bytes();
        msy_context_run((*runner).ctx, src.as_ptr() as *const c_char, src.len());
    }
}
