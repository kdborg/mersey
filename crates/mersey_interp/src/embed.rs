//! The embedding boundary, shared by every host ABI.
//!
//! Two hosts embed the engine today — the browser loader through WASM
//! (`mersey_wasm`) and native embedders through the C ABI (`mersey_capi`,
//! which is what the Chromium fork's `//components/mersey` wraps). Both must
//! load, check and run a module graph *identically*: same `std:` embedding,
//! same leak-before-check discipline, same diagnostics. That used to live in
//! the WASM crate and was about to be duplicated into the C one, and two
//! copies of the loader is how two hosts drift apart. This is the one copy.
//!
//! The payload format is the loader contract (`web/mersey-loader.js` and the
//! fork's `MerseyScript` both produce it):
//!
//! ```json
//! {"entry": "a.mersey",
//!  "modules": [{"spec": "b.mersey", "source": "…"}, …],
//!  "lazy": ["c.mersey"]}
//! ```
//!
//! `modules` is dependency-first; `lazy` marks the targets of dynamic
//! `import(…)` — loaded and checked with everything else (§4.5: the graph is
//! closed), but not run until imported.

use std::rc::Rc;

use mersey_front::{ast, bind, check, parser, source};

use crate::{webjson, Interp};

/// Scan one module's source for its imports, without running anything: the
/// *host* owns fetching (CORS, CSP, integrity are its jurisdiction), so the
/// host needs the list. Returns JSON: `{"static": […], "dynamic": […]}`.
pub fn scan_imports_json(bytes: &[u8]) -> String {
    let Ok(src) = source::decode("<scan>", bytes) else {
        return "{\"static\":[],\"dynamic\":[]}".to_string();
    };
    let parsed = parser::parse(&src);
    let statics = mersey_front::graph::imports(&parsed.module);
    let dynamics = mersey_front::graph::dynamic_imports(&parsed.module);
    let arr = |specs: &[String]| {
        let mut out = String::from("[");
        for (i, s) in specs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&s.replace('\\', "\\\\").replace('"', "\\\""));
            out.push('"');
        }
        out.push(']');
        out
    };
    format!(
        "{{\"static\":{},\"dynamic\":{}}}",
        arr(&statics),
        arr(&dynamics)
    )
}

/// Run a whole module graph from a loader payload. Diagnostics and runtime
/// errors go to `report`, one message per call; the return code is the ABI's:
/// 0 = ran, 1 = did not start (diagnostics), 2 = threw.
pub fn run_graph_json(interp: &mut Interp, payload: &str, report: &mut dyn FnMut(&str)) -> u32 {
    let Some(payload) = webjson::parse(payload) else {
        report("bad module-graph payload");
        return 1;
    };
    let Some(webjson::Json::Arr(items)) = payload.get("modules") else {
        report("module-graph payload has no modules");
        return 1;
    };

    let mut parsed_modules: Vec<(String, &'static ast::Module)> = Vec::new();
    let mut failed = false;

    // The `std:` modules written in Mersey (`std:result`, `std:url`, …) are
    // embedded in the engine, not fetched — the loader has nothing to fetch
    // them *from*. They have to be in the graph before anything that imports
    // them is checked, or their exports are unknown and every use of one is a
    // type error in a file the author never wrote. They come first: nothing
    // they import can depend on the page's own modules.
    for spec in mersey_front::stdlib::source_modules() {
        let Some(text) = mersey_front::stdlib::source(spec) else {
            continue;
        };
        let Ok(decoded) = source::decode(spec, text.as_bytes()) else {
            continue;
        };
        let parsed = parser::parse(&decoded);
        if !parsed.diagnostics.is_empty() {
            continue; // the conformance suite would have caught this
        }
        let module: &'static _ = Box::leak(Box::new(parsed.module));
        parsed_modules.push(((*spec).to_string(), module));
    }

    for item in items {
        let (Some(spec), Some(src_text)) = (
            item.get("spec").and_then(|v| v.as_str()),
            item.get("source").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let decoded = match source::decode(spec, src_text.as_bytes()) {
            Ok(d) => d,
            Err(d) => {
                report(&d.to_string());
                failed = true;
                continue;
            }
        };
        let parsed = parser::parse(&decoded);
        for d in &parsed.diagnostics {
            report(&format!("{spec}: {d}"));
            failed = true;
        }
        // Leaked *before* checking: the checker's side tables are keyed by AST
        // node address, so the AST that is checked must be the AST that runs.
        let module: &'static _ = Box::leak(Box::new(parsed.module));
        for d in &bind::bind(module).diagnostics {
            report(&format!("{spec}: {d}"));
            failed = true;
        }
        parsed_modules.push((spec.to_string(), module));
    }
    if failed {
        return 1;
    }
    let refs: Vec<(String, &ast::Module)> = parsed_modules
        .iter()
        .map(|(s, m)| (s.clone(), *m))
        .collect();
    for (spec, out) in check::check_graph(&refs) {
        for d in &out.diagnostics {
            report(&format!("{spec}: {d}"));
            failed = true;
        }
    }
    if failed {
        return 1;
    }

    // Modules the payload marks lazy are the targets of a dynamic `import(…)`:
    // loaded and checked with the rest, but not run until someone imports them.
    let lazy: Vec<String> = match payload.get("lazy") {
        Some(webjson::Json::Arr(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    let (eager, deferred): (Vec<_>, Vec<_>) = parsed_modules
        .into_iter()
        .partition(|(spec, _)| !lazy.contains(spec));

    for (spec, module) in deferred {
        interp.register_lazy(spec, module);
    }
    match interp.run_graph(eager) {
        Ok(()) => 0,
        Err(t) => {
            let msg = interp.describe_thrown(&t);
            report(&msg);
            2
        }
    }
}

/// Compile and run one module — the single-script path (`msy_run`,
/// `msy_context_run`). Kept beside the graph path so the leak-before-check
/// rule lives in exactly one file.
pub fn run_single(
    interp: &mut Interp,
    name: &str,
    bytes: &[u8],
    report: &mut dyn FnMut(&str),
) -> u32 {
    let src = match source::decode(name, bytes) {
        Ok(s) => s,
        Err(d) => {
            report(&d.to_string());
            return 1;
        }
    };
    let parsed = parser::parse(&src);
    let mut diags = parsed.diagnostics;
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    if diags.is_empty() {
        diags = bind::bind(module).diagnostics;
    }
    if diags.is_empty() {
        diags = check::check(module).diagnostics;
    }
    if !diags.is_empty() {
        for d in &diags {
            report(&d.to_string());
        }
        return 1;
    }
    match interp.run_module(module) {
        Ok(()) => 0,
        Err(t) => {
            let msg = interp.describe_thrown(&t);
            report(&msg);
            2
        }
    }
}

/// Shared JSON escape for hosts building payloads by hand (the C demo does).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// `Rc` is not `Send`; a context is confined to its creating thread. This
/// zero-sized guard makes that a compile error for Rust embedders and a
/// documented rule for C ones.
pub struct ThreadConfined(std::marker::PhantomData<Rc<()>>);

impl Default for ThreadConfined {
    fn default() -> Self {
        ThreadConfined(std::marker::PhantomData)
    }
}
