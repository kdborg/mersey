//! Module graph: specifier scanning, topological ordering, and whole-graph
//! type checking (spec §4.5 — the module graph is closed before execution).

use std::collections::{HashMap, HashSet};

use crate::ast::{Item, Module};
use crate::diag::{Code, Diagnostic, Pos};

/// Specifiers a module imports, in source order.
pub fn imports(module: &Module) -> Vec<String> {
    module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Import(im) => Some(im.from.clone()),
            _ => None,
        })
        .collect()
}

/// Relative specifiers only (`./x.mersey`, `../y.mersey`); `std:`/`browser:`
/// are built in and never fetched.
pub fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

/// A remote dependency: an ordinary URL, which any static host can serve.
///
/// The engine never fetches one. `mersey fetch` downloads it, pins its hash in
/// mersey.lock, and caches it on disk; `mersey run` resolves it from that cache
/// or fails. Code that is running has no authority to reach the network (§5.4),
/// and a build that has already fetched is reproducible and offline.
pub fn is_remote(spec: &str) -> bool {
    spec.starts_with("https://") || spec.starts_with("http://")
}

/// Specifiers that are modules in the graph: relative files, remote URLs, plus
/// the `std:` modules that are written in Mersey (`crate::stdlib`).
pub fn is_module(spec: &str) -> bool {
    is_relative(spec) || is_remote(spec) || crate::stdlib::is_source_module(spec)
}

/// Resolve for the graph: `std:` modules resolve to themselves, and a relative
/// import *inside* a remote module stays remote — a package's own files come
/// from the package, not from the importing project's disk.
pub fn resolve_module(referrer: &str, spec: &str) -> String {
    if crate::stdlib::is_source_module(spec) || is_remote(spec) {
        return spec.to_string();
    }
    if is_remote(referrer) && is_relative(spec) {
        return resolve_url(referrer, spec);
    }
    resolve(referrer, spec)
}

/// Resolve `spec` against a remote `referrer`, keeping scheme and host.
pub fn resolve_url(referrer: &str, spec: &str) -> String {
    let Some((scheme, rest)) = referrer.split_once("://") else {
        return spec.to_string();
    };
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, p),
        None => (rest, ""),
    };
    let resolved = resolve(path, spec);
    format!("{scheme}://{host}/{resolved}")
}

/// Resolve `spec` against the directory of `referrer` (POSIX-style, as URLs
/// and file paths both behave here).
pub fn resolve(referrer: &str, spec: &str) -> String {
    let base: Vec<&str> = referrer
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or("")
        .split('/')
        .collect();
    let mut parts: Vec<String> = base
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    for seg in spec.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}

/// Dependency-first ordering. `deps(spec) -> its relative imports`.
/// Returns `Err` on an import cycle (spec §4.5: the graph is static).
pub fn topo_order(
    entry: &str,
    deps: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, Diagnostic> {
    let mut out = Vec::new();
    let mut done = HashSet::new();
    let mut path = Vec::new();
    visit(entry, deps, &mut out, &mut done, &mut path)?;
    Ok(out)
}

fn visit(
    spec: &str,
    deps: &HashMap<String, Vec<String>>,
    out: &mut Vec<String>,
    done: &mut HashSet<String>,
    path: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    if done.contains(spec) {
        return Ok(());
    }
    if path.iter().any(|p| p == spec) {
        path.push(spec.to_string());
        return Err(Diagnostic::error(
            Code::UnexpectedToken,
            format!("import cycle: {}", path.join(" → ")),
            Pos { line: 1, col: 1 },
        ));
    }
    path.push(spec.to_string());
    for d in deps.get(spec).map(|v| v.as_slice()).unwrap_or(&[]) {
        visit(d, deps, out, done, path)?;
    }
    path.pop();
    done.insert(spec.to_string());
    out.push(spec.to_string());
    Ok(())
}
