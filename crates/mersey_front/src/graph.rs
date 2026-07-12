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

/// Resolve `spec` against the directory of `referrer` (POSIX-style, as URLs
/// and file paths both behave here).
pub fn resolve(referrer: &str, spec: &str) -> String {
    let base: Vec<&str> = referrer.rsplit_once('/').map(|(d, _)| d).unwrap_or("").split('/').collect();
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
