//! Tier 1 with host constructors and string web properties.
//!
//! `new URL(s)` (a bridge `new`) and `url.pathname` / `url.search` (string-valued
//! bridge property reads) are the pieces the `url` web benchmark is made of. Each
//! is checked three ways — JIT, bytecode VM, tree-walker — and all three must
//! agree exactly. The mock host declines the wide (`_u16`) fast path, so every
//! tier resolves through the same string `web_new`/`web_get`, and any divergence
//! is the compiled path getting the plumbing wrong.

use std::cell::RefCell;
use std::rc::Rc;

use mersey_front::{bind, check, parser, source};
use mersey_interp::{new_interp, Host, Thrown};

/// A minimal URL host: `new URL(s)` stores the string and hands back a handle;
/// `url.pathname` / `url.search` parse it back out. Deterministic, so the three
/// tiers must agree — and realistic enough (varying-length components) that a
/// mismarshalled length or handle would show up in the checksum.
struct UrlHost {
    out: Rc<RefCell<String>>,
    urls: Rc<RefCell<Vec<String>>>,
}

fn json_ok_ref(h: i64) -> String {
    format!("{{\"ok\":{{\"__ref__\":{h}}}}}")
}

fn json_ok_str(s: &str) -> String {
    // The component strings here contain no JSON metacharacters.
    format!("{{\"ok\":\"{s}\"}}")
}

impl Host for UrlHost {
    fn print(&mut self, s: &str) {
        let mut b = self.out.borrow_mut();
        b.push_str(s);
        b.push('\n');
    }
    fn dom_set_text(&mut self, _id: &str, _t: &str) {}
    fn dom_get_text(&mut self, _id: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}

    fn web_new(&mut self, ctor: &str, args_json: &str) -> String {
        if ctor != "URL" {
            return format!("{{\"err\":\"unknown constructor {ctor}\"}}");
        }
        // `args_json` is a one-element JSON array holding the URL string.
        let url = args_json
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim()
            .trim_matches('"')
            .to_string();
        let mut urls = self.urls.borrow_mut();
        let handle = urls.len() as i64;
        urls.push(url);
        json_ok_ref(handle)
    }

    fn web_get(&mut self, target: i64, prop: &str) -> String {
        let urls = self.urls.borrow();
        let Some(url) = urls.get(target as usize) else {
            return "{\"err\":\"bad handle\"}".into();
        };
        // Parse `scheme://host/pathname?search` just enough for the two
        // components the benchmark reads.
        let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
        let path_and_query = after_scheme.find('/').map(|i| &after_scheme[i..]).unwrap_or("/");
        let (pathname, search) = match path_and_query.split_once('?') {
            Some((p, q)) => (p.to_string(), format!("?{q}")),
            None => (path_and_query.to_string(), String::new()),
        };
        match prop {
            "pathname" => json_ok_str(&pathname),
            "search" => json_ok_str(&search),
            _ => "{\"err\":\"unknown property\"}".into(),
        }
    }
}

fn try_run(src_text: &str, use_vm: bool, jit: bool) -> Result<String, String> {
    let src = source::decode("<test>", src_text.as_bytes()).expect("decode");
    let parsed = parser::parse(&src);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics.first());
    assert!(bind::bind(module).diagnostics.is_empty(), "bind errors");
    let diags = check::check(module).diagnostics;
    assert!(diags.is_empty(), "check: {:?}", diags.first().map(|d| &d.message));

    let out = Rc::new(RefCell::new(String::new()));
    let host = Box::new(UrlHost {
        out: out.clone(),
        urls: Rc::new(RefCell::new(Vec::new())),
    });
    let mut i = new_interp(host);
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
    }
    match i.run_module(module) {
        Ok(()) => Ok(out.borrow().clone()),
        Err(t) => Err(describe(&mut i, &t)),
    }
}

fn describe(i: &mut mersey_interp::Interp, t: &Thrown) -> String {
    i.describe_thrown(t)
}

fn run(src: &str, use_vm: bool, jit: bool) -> String {
    try_run(src, use_vm, jit).unwrap_or_else(|e| panic!("runtime: {e}"))
}

/// The three tiers must produce the same output, character for character.
fn agree(src: &str) -> String {
    let jit = run(src, true, true);
    let vm = run(src, true, false);
    let tree = run(src, false, false);
    assert_eq!(jit, vm, "JIT and bytecode VM disagree");
    assert_eq!(vm, tree, "bytecode VM and tree-walker disagree");
    jit
}

/// The `url` benchmark's shape: a hot loop that constructs a URL and reads two
/// string components back, summing their lengths. The loop is hot enough to
/// compile (and, past OSR, to run compiled from the top) so the `new URL` and the
/// two `web_get`s all execute in Tier-1 code.
const URL_WORKLOAD: &str = r#"
import { console } from "std:console";
import { URL } from "browser:dom";

function work(n: int32): int32 {
    let sum = 0;
    let i = 0;
    while (i < n) {
        const u = new URL(`https://example.com/path/${i}?q=mersey&n=${i}`);
        sum = sum + u.pathname.length + u.search.length;
        i += 1;
    }
    return sum;
}

console.log(work(2000));
"#;

#[test]
fn url_new_and_string_props_agree_across_tiers() {
    let out = agree(URL_WORKLOAD);
    // pathname `/path/<i>` and search `?q=mersey&n=<i>` for i in 0..2000.
    // Independently: sum over i of (len("/path/") + digits(i)) + (len("?q=mersey&n=") + digits(i)).
    // len("/path/")=6, len("?q=mersey&n=")=12 -> 18 + 2*digits(i).
    // digits: 0..9 ->1 (10), 10..99 ->2 (90), 100..999 ->3 (900), 1000..1999 ->4 (1000).
    // sum digits = 10 + 180 + 2700 + 4000 = 6890.
    // total = 18*2000 + 2*6890 = 36000 + 13780 = 49780.
    assert_eq!(out.trim(), "49780", "url component length sum");
}

/// A single `new URL` and one property read — the smallest case, so a plumbing
/// bug (handle marshalling, string capture) is isolated from the loop.
#[test]
fn url_single_construct_and_read_agrees() {
    let src = r#"
import { console } from "std:console";
import { URL } from "browser:dom";
function one(): int32 {
    const u = new URL(`https://example.com/abc?x=1`);
    return u.pathname.length + u.search.length;
}
console.log(one());
"#;
    // pathname "/abc" (4) + search "?x=1" (4) = 8.
    assert_eq!(agree(src).trim(), "8");
}

// ---- DOM: createElement (handle), `as` cast, textContent set, appendChild -----

/// A mock DOM host that records every mutation. `document` is handle 0, its
/// `body` handle 1; `createElement` mints fresh handles. The op log lets the test
/// assert the three tiers drive the same sequence of bridge operations — the DOM
/// workload's return value (just `n`) would agree trivially otherwise.
struct DomHost {
    out: Rc<RefCell<String>>,
    log: Rc<RefCell<Vec<String>>>,
    next: Rc<RefCell<i64>>,
}

impl Host for DomHost {
    fn print(&mut self, s: &str) {
        let mut b = self.out.borrow_mut();
        b.push_str(s);
        b.push('\n');
    }
    fn dom_set_text(&mut self, _id: &str, _t: &str) {}
    fn dom_get_text(&mut self, _id: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}

    fn web_global(&mut self, name: &str) -> i64 {
        if name == "document" {
            0
        } else {
            -1
        }
    }
    fn web_get(&mut self, target: i64, prop: &str) -> String {
        match (target, prop) {
            (0, "body") => "{\"ok\":{\"__ref__\":1}}".into(),
            _ => "{\"err\":\"unknown property\"}".into(),
        }
    }
    fn web_set(&mut self, target: i64, prop: &str, value_json: &str) -> String {
        self.log.borrow_mut().push(format!("set #{target}.{prop}={value_json}"));
        "{\"ok\":null}".into()
    }
    fn web_call(&mut self, target: i64, method: &str, args_json: &str) -> String {
        match method {
            "createElement" => {
                let mut n = self.next.borrow_mut();
                let h = *n;
                *n += 1;
                self.log.borrow_mut().push(format!("create({args_json})->#{h}"));
                format!("{{\"ok\":{{\"__ref__\":{h}}}}}")
            }
            "appendChild" => {
                self.log.borrow_mut().push(format!("append #{target} {args_json}"));
                "{\"ok\":null}".into()
            }
            _ => "{\"err\":\"unknown method\"}".into(),
        }
    }
}

/// Run the DOM program on one tier, returning (stdout, operation log).
fn dom_run(src: &str, use_vm: bool, jit: bool) -> (String, Vec<String>) {
    let s = source::decode("<test>", src.as_bytes()).expect("decode");
    let parsed = parser::parse(&s);
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics.first());
    assert!(bind::bind(module).diagnostics.is_empty(), "bind");
    let diags = check::check(module).diagnostics;
    assert!(diags.is_empty(), "check: {:?}", diags.first().map(|d| &d.message));

    let out = Rc::new(RefCell::new(String::new()));
    let log = Rc::new(RefCell::new(Vec::new()));
    let host = Box::new(DomHost {
        out: out.clone(),
        log: log.clone(),
        next: Rc::new(RefCell::new(2)), // 0 = document, 1 = body
    });
    let mut i = new_interp(host);
    i.use_vm = use_vm;
    if jit {
        i.jit = Some(mersey_jit::hook);
    }
    i.run_module(module).unwrap_or_else(|t| panic!("runtime: {}", describe(&mut i, &t)));
    let o = out.borrow().clone();
    let l = log.borrow().clone();
    (o, l)
}

/// The `dom` benchmark's shape: createElement, cast, set textContent, appendChild
/// in a hot loop. All three tiers must drive the identical bridge-op sequence.
const DOM_WORKLOAD: &str = r#"
import { console } from "std:console";
import { document } from "browser:dom";

const body = document.body as Element;

function work(n: int32): int32 {
    let i = 0;
    while (i < n) {
        const el = document.createElement("div") as HTMLElement;
        el.textContent = `item ${i}`;
        body.appendChild(el);
        i += 1;
    }
    return n;
}

console.log(work(2000));
"#;

#[test]
fn dom_create_set_append_agree_across_tiers() {
    let (jit_out, jit_log) = dom_run(DOM_WORKLOAD, true, true);
    let (vm_out, vm_log) = dom_run(DOM_WORKLOAD, true, false);
    let (tree_out, tree_log) = dom_run(DOM_WORKLOAD, false, false);
    assert_eq!(jit_out, vm_out, "JIT vs VM stdout");
    assert_eq!(vm_out, tree_out, "VM vs tree stdout");
    assert_eq!(jit_out.trim(), "2000");
    // The mutation sequence must be byte-identical across tiers.
    assert_eq!(jit_log, vm_log, "JIT vs VM op log");
    assert_eq!(vm_log, tree_log, "VM vs tree op log");
    // create + set + append per iteration.
    assert_eq!(jit_log.len(), 2000 * 3, "op count");
    assert_eq!(jit_log[0], "create([\"div\"])->#2");
    assert_eq!(jit_log[1], "set #2.textContent=\"item 0\"");
    assert_eq!(jit_log[2], "append #1 [{\"__ref__\":2}]");
}

