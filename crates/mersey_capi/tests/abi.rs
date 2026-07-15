//! The C ABI, driven the way Blink will drive it.
//!
//! Every call in this file goes through the `extern "C"` functions and the
//! host table — the same symbols, the same struct layout, the same buffer
//! lifetime rules a C++ embedder gets. The host side is a mock of what
//! `//components/mersey` will be: a handle table of fake DOM objects, events
//! that arrive with JSON payloads, a promise the host settles *after* the
//! script finished, and a capability list the engine must enforce.
//!
//! What this buys the fork: when the Blink glue misbehaves, this file is the
//! evidence for which side of the boundary is wrong.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::raw::c_char;

use mersey_capi::{
    msy_abi_version, msy_context_free, msy_context_invoke_args, msy_context_new,
    msy_context_new_ex, msy_context_run, msy_context_run_graph, msy_context_scan_imports,
    MsyHostTable, MSY_ABI_VERSION, MSY_FLAG_NO_JIT,
};

/// The mock page: what the "browser" remembers happening.
#[derive(Default)]
struct MockPage {
    printed: Vec<String>,
    errors: Vec<String>,
    /// The fake DOM: handle -> (tag, text). Handle 0 is `window`; handle 1 is
    /// `document`; elements start at 2.
    nodes: Vec<(String, String)>,
    /// Callbacks the engine registered via addEventListener(event, cb).
    listeners: Vec<(i64, String, u32)>,
    /// A callback handed to a host promise's `.then`.
    then_cb: Option<u32>,
    caps: &'static str,
    /// Scratch the reply-pointer contract points into.
    reply: String,
}

thread_local! {
    static PAGE: RefCell<MockPage> = RefCell::new(MockPage::default());
}

fn page<R>(f: impl FnOnce(&mut MockPage) -> R) -> R {
    PAGE.with(|p| f(&mut p.borrow_mut()))
}

fn s(ptr: *const c_char, len: usize) -> String {
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Store a reply and hand back (ptr, len) with the documented lifetime:
/// valid until the next host call.
fn reply(out_len: *mut usize, text: String) -> *const c_char {
    page(|p| {
        p.reply = text;
        unsafe { *out_len = p.reply.len() };
        p.reply.as_ptr() as *const c_char
    })
}

extern "C" fn h_print(_d: *mut c_void, ptr: *const c_char, len: usize) {
    let msg = s(ptr, len);
    page(|p| p.printed.push(msg));
}

extern "C" fn h_error(_d: *mut c_void, ptr: *const c_char, len: usize) {
    let msg = s(ptr, len);
    page(|p| p.errors.push(msg));
}

extern "C" fn h_caps(_d: *mut c_void, out_len: *mut usize) -> *const c_char {
    let caps = page(|p| p.caps);
    unsafe { *out_len = caps.len() };
    caps.as_ptr() as *const c_char
}

extern "C" fn h_web_global(_d: *mut c_void, name: *const c_char, len: usize) -> i64 {
    match s(name, len).as_str() {
        "window" => 0,
        "document" => 1,
        // A promise-producing API: `fetch` is handle 80; the promise it
        // returns is handle 90.
        "fetch" => 80,
        _ => -1,
    }
}

extern "C" fn h_web_get(
    _d: *mut c_void,
    target: i64,
    prop: *const c_char,
    prop_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let prop = s(prop, prop_len);
    let text = page(|p| {
        let idx = target as usize;
        match (idx, prop.as_str()) {
            (_, "textContent") if idx >= 2 && idx < p.nodes.len() + 2 => {
                format!("{{\"ok\":\"{}\"}}", p.nodes[idx - 2].1)
            }
            (_, "tagName") if idx >= 2 && idx < p.nodes.len() + 2 => {
                format!("{{\"ok\":\"{}\"}}", p.nodes[idx - 2].0)
            }
            _ => "{\"err\":\"no such property\"}".to_string(),
        }
    });
    reply(out_len, text)
}

#[allow(clippy::too_many_arguments)]
extern "C" fn h_web_set(
    _d: *mut c_void,
    target: i64,
    prop: *const c_char,
    prop_len: usize,
    value: *const c_char,
    value_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let prop = s(prop, prop_len);
    let value = s(value, value_len);
    let text = page(|p| {
        let idx = target as usize;
        if prop == "textContent" && idx >= 2 && idx < p.nodes.len() + 2 {
            p.nodes[idx - 2].1 = value.trim_matches('"').to_string();
            "{\"ok\":null}".to_string()
        } else {
            "{\"err\":\"cannot set\"}".to_string()
        }
    });
    reply(out_len, text)
}

#[allow(clippy::too_many_arguments)]
extern "C" fn h_web_call(
    _d: *mut c_void,
    target: i64,
    method: *const c_char,
    method_len: usize,
    args: *const c_char,
    args_len: usize,
    out_len: *mut usize,
) -> *const c_char {
    let method = s(method, method_len);
    let args = s(args, args_len);
    let text = page(|p| match (target, method.as_str()) {
        // document.createElement("div") -> a fresh handle
        (1, "createElement") => {
            let tag = args.trim_matches(['[', ']', '"']).to_uppercase();
            p.nodes.push((tag, String::new()));
            format!("{{\"ok\":{{\"__ref__\":{}}}}}", p.nodes.len() + 1)
        }
        // element.addEventListener("click", cb)
        (t, "addEventListener") if t >= 2 => {
            // args: ["click",{"__cb__":N}]
            let ev = args
                .split('"')
                .nth(1)
                .unwrap_or("")
                .to_string();
            let cb: u32 = args
                .split("__cb__\":")
                .nth(1)
                .and_then(|r| r.trim_end_matches(['}', ']']).parse().ok())
                .unwrap_or(0);
            p.listeners.push((t, ev, cb));
            "{\"ok\":null}".to_string()
        }
        // Calling the imported `fetch` itself: method "" means "the handle is
        // callable" (the bridge contract). It returns a promise handle.
        (80, "") => "{\"ok\":{\"__ref__\":90}}".to_string(),
        // promise.then(resolve, reject): the host stores the resolve callback
        // and settles LATER — after the script has returned — which is the
        // browser's actual shape.
        (90, "then") => {
            let cb: u32 = args
                .split("__cb__\":")
                .nth(1)
                .and_then(|r| r.trim_end_matches(['}', ']']).parse().ok())
                .unwrap_or(0);
            p.then_cb = Some(cb);
            "{\"ok\":null}".to_string()
        }
        _ => "{\"err\":\"no such method\"}".to_string(),
    });
    reply(out_len, text)
}

extern "C" fn h_time_ms(_d: *mut c_void, _epoch: i32) -> f64 {
    12345.0
}

extern "C" fn h_random(_d: *mut c_void, buf: *mut u8, n: usize) -> i32 {
    // Deterministic "entropy" for the test.
    for i in 0..n {
        unsafe { *buf.add(i) = (i * 37 % 251) as u8 };
    }
    0
}

fn table() -> MsyHostTable {
    MsyHostTable {
        data: std::ptr::null_mut(),
        print: Some(h_print),
        print_level: None,
        error: Some(h_error),
        caps: Some(h_caps),
        web_global: Some(h_web_global),
        web_get: Some(h_web_get),
        web_set: Some(h_web_set),
        web_call: Some(h_web_call),
        web_new: None,
        web_iterate: None,
        web_instanceof: None,
        web_release: None,
        web_bytes_read: None,
        web_bytes_write: None,
        time_ms: Some(h_time_ms),
        random_bytes: Some(h_random),
        dom_set_text: None,
        dom_get_text: None,
        dom_add_listener: None,
        // Interned + scalar + wide-string fast paths (ABI v3–v5): this mock host
        // uses only the reflective ops, so the engine falls back to them.
        web_intern: None,
        web_get_id: None,
        web_set_str: None,
        web_set_num: None,
        web_call_str: None,
        web_call_scalars: None,
        web_new_scalars: None,
        web_get_u16: None,
        web_set_u16: None,
        web_call_u16: None,
        web_new_u16: None,
    }
}

fn reset(caps: &'static str) {
    page(|p| {
        *p = MockPage::default();
        p.caps = caps;
    });
}

fn run(ctx: *mut mersey_capi::MsyContext, src: &str) -> u32 {
    unsafe { msy_context_run(ctx, src.as_ptr() as *const c_char, src.len()) }
}

#[test]
fn the_abi_version_is_what_the_header_says() {
    assert_eq!(msy_abi_version(), MSY_ABI_VERSION);
}

/// Language + JIT through the C boundary: a hot loop over objects, compiled
/// and correct, with output arriving through the host's print hook.
#[test]
fn compute_and_print_cross_the_boundary() {
    reset("[]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    assert!(!ctx.is_null());
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
class P { public x: float64 = 0.0; public bump(): void { this.x = this.x + 1.0; } }
function work(n: int32): float64 {
    const p = new P();
    for (let i = 0; i < n; i++) { p.bump(); }
    return p.x;
}
console.log("sum:", work(100000));
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(page(|p| p.printed.clone()), vec!["sum: 100000".to_string()]);
    unsafe { msy_context_free(ctx) };
}

/// The universal bridge over C: resolve a global, create an element, write
/// and read a property — the reflective path Blink's table will implement.
#[test]
fn the_web_bridge_works_over_c() {
    reset("[\"web\",\"dom\"]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
import { document } from "browser:dom";
const el = document.createElement("div");
el.textContent = "made from mersey";
console.log("tag:", el.tagName, "text:", el.textContent);
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(
        page(|p| p.printed.clone()),
        vec!["tag: DIV text: made from mersey".to_string()]
    );
    assert_eq!(page(|p| p.nodes.clone()), vec![("DIV".to_string(), "made from mersey".to_string())]);
    unsafe { msy_context_free(ctx) };
}

/// Events with payloads: the engine registers a listener; the "browser" fires
/// it later with a JSON event object; the callback reads a field of it.
#[test]
fn events_arrive_with_payloads() {
    reset("[\"web\",\"dom\"]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
import { document } from "browser:dom";
const el = document.createElement("button");
el.addEventListener("click", (e: unknown) => {
    console.log("clicked");
});
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    let (_, ev, cb) = page(|p| p.listeners[0].clone());
    assert_eq!(ev, "click");
    // The browser fires the event from its task runner:
    let args = "[{\"type\":\"click\"}]";
    let rc = unsafe { msy_context_invoke_args(ctx, cb, args.as_ptr() as *const c_char, args.len()) };
    assert_eq!(rc, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(
        page(|p| p.printed.clone()),
        vec!["clicked".to_string()]
    );
    unsafe { msy_context_free(ctx) };
}

/// A promise settled by the host AFTER the script returned — the actual shape
/// of every async browser API. The engine's `.then` callback crosses as a
/// {"__cb__":N}; the host holds it, then invokes it re-entrantly later.
#[test]
fn a_host_promise_settles_after_the_script_returns() {
    reset("[\"web\"]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
import { fetch } from "browser:dom";
async function go(): Promise<void> {
    const r = await fetch("https://example.test/x");
    console.log("settled");
}
go();
console.log("script done");
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(page(|p| p.printed.clone()), vec!["script done".to_string()]);
    let cb = page(|p| p.then_cb).expect("the .then callback crossed");
    let args = "[42]";
    let rc = unsafe { msy_context_invoke_args(ctx, cb, args.as_ptr() as *const c_char, args.len()) };
    assert_eq!(rc, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(
        page(|p| p.printed.clone()),
        vec!["script done".to_string(), "settled".to_string()]
    );
    unsafe { msy_context_free(ctx) };
}

/// Module graphs over the ABI: scan tells the host what to fetch, the host
/// hands back the assembled graph, imports resolve across files.
#[test]
fn module_graphs_load_through_the_loader_contract() {
    reset("[]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };

    let main_src = r#"
import { console } from "std:console";
import { double } from "./math.mersey";
console.log("doubled:", double(21));
"#;
    // Step 1: the host asks what main imports.
    let mut len = 0usize;
    let ptr = unsafe {
        msy_context_scan_imports(ctx, main_src.as_ptr() as *const c_char, main_src.len(), &mut len)
    };
    let scan = s(ptr, len);
    assert!(scan.contains("./math.mersey"), "{scan}");

    // Step 2: the host "fetched" the dep and submits the graph.
    let math_src = "export function double(x: int32): int32 { return x * 2; }";
    let payload = format!(
        "{{\"entry\":\"main.mersey\",\"modules\":[{{\"spec\":\"math.mersey\",\"source\":\"{}\"}},{{\"spec\":\"main.mersey\",\"source\":\"{}\"}}]}}",
        math_src.replace('"', "\\\""),
        main_src.replace('"', "\\\"").replace('\n', "\\n"),
    );
    let code = unsafe {
        msy_context_run_graph(ctx, payload.as_ptr() as *const c_char, payload.len())
    };
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(page(|p| p.printed.clone()), vec!["doubled: 42".to_string()]);
    unsafe { msy_context_free(ctx) };
}

/// Deny-by-default: with no `random` capability the API throws; with it, the
/// host's entropy hook is what answers.
#[test]
fn capabilities_gate_the_host() {
    reset("[]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(
        ctx,
        r#"
import { random } from "std:random";
random.bytes(4);
"#,
    );
    assert_eq!(code, 2, "must throw without the capability");
    assert!(
        page(|p| p.errors.join(" ")).contains("random"),
        "{:?}",
        page(|p| p.errors.clone())
    );
    unsafe { msy_context_free(ctx) };

    reset("[\"random\"]");
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
import { random } from "std:random";
console.log("got:", random.bytes(4).length);
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(page(|p| p.printed.clone()), vec!["got: 4".to_string()]);
    unsafe { msy_context_free(ctx) };
}

/// The jitless flag: same program, same answer, no executable pages — the
/// configuration for a sandbox that forbids a second JIT.
#[test]
fn the_no_jit_flag_still_computes() {
    reset("[]");
    let t = table();
    let ctx = unsafe { msy_context_new_ex(&t, MSY_FLAG_NO_JIT) };
    let code = run(
        ctx,
        r#"
import { console } from "std:console";
function fib(n: int32): int32 { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); }
console.log("fib:", fib(20));
"#,
    );
    assert_eq!(code, 0, "errors: {:?}", page(|p| p.errors.clone()));
    assert_eq!(page(|p| p.printed.clone()), vec!["fib: 6765".to_string()]);
    unsafe { msy_context_free(ctx) };
}

/// Diagnostics arrive through `error`, and the return code says "did not
/// start" — the contract Blink uses to decide what to surface in DevTools.
#[test]
fn diagnostics_cross_as_errors() {
    reset("[]");
    let t = table();
    let ctx = unsafe { msy_context_new(&t) };
    let code = run(ctx, "let x: int32 = \"not a number\";");
    assert_eq!(code, 1);
    assert!(
        page(|p| p.errors.join(" ")).contains("E0"),
        "{:?}",
        page(|p| p.errors.clone())
    );
    unsafe { msy_context_free(ctx) };
}
