//! Ambient web-platform declarations, generated from the standardized
//! WebIDL corpus (tools/webidl-gen). The checker collects these before the
//! user's module, so every standardized interface/dictionary/typedef is a
//! known type; `browser:dom` imports resolve against the harvested Window
//! globals. Per spec §5.4 (no ambient authority) the *values* are not in
//! scope until imported — only the type names are ambient.

use std::sync::OnceLock;

use crate::ast::{Item, Module, Pattern, Stmt, TypeExpr};
use crate::parser;
use crate::source::SourceFile;

const SOURCE: &str = include_str!("webapi.gen.mersey");

pub struct WebApi {
    pub module: &'static Module,
    /// `let name: TypeExpr;` ambient globals (importable via `browser:dom`).
    pub globals: Vec<(String, &'static TypeExpr)>,
    /// All ambient type names (interfaces + aliases), for the binder.
    pub type_names: Vec<String>,
}

pub fn webapi() -> &'static WebApi {
    static CELL: OnceLock<WebApi> = OnceLock::new();
    CELL.get_or_init(|| {
        let src = SourceFile {
            name: "<webapi>".into(),
            text: SOURCE.to_string(),
        };
        let out = parser::parse(&src);
        debug_assert!(
            out.diagnostics.is_empty(),
            "generated webapi must parse cleanly: {}",
            out.diagnostics
                .first()
                .map(|d| d.to_string())
                .unwrap_or_default()
        );
        let module: &'static Module = Box::leak(Box::new(out.module));
        let mut globals = Vec::new();
        let mut type_names = Vec::new();
        for item in &module.items {
            match item {
                Item::Stmt(Stmt::Var(v)) => {
                    for b in &v.bindings {
                        if let (Pattern::Name(n), Some(ty)) = (&b.target, &b.ty) {
                            // A few globals are declared twice by the IDL corpus
                            // — `window` is both `JsAny` (from one partial
                            // interface) and `Window` (from another). The precise
                            // one is the true one, and the imprecise one must not
                            // shadow it: `window` was silently untyped for as long
                            // as `any` existed to hide it.
                            let precise =
                                !matches!(ty, TypeExpr::Named { name, .. } if name == "JsAny");
                            match globals.iter().position(|(g, _)| *g == n.text) {
                                Some(i) if precise => globals[i] = (n.text.clone(), ty),
                                Some(_) => {}
                                None => globals.push((n.text.clone(), ty)),
                            }
                        }
                    }
                }
                Item::Decl(d) => {
                    use crate::ast::Decl;
                    match d {
                        Decl::Interface(i) => type_names.push(i.name.text.clone()),
                        Decl::TypeAlias(t) => type_names.push(t.name.text.clone()),
                        Decl::Class(c) => type_names.push(c.name.text.clone()),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        WebApi {
            module,
            globals,
            type_names,
        }
    })
}

/// The declared type of an ambient global (for `browser:dom` imports).
pub fn global_type(name: &str) -> Option<&'static TypeExpr> {
    webapi()
        .globals
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, t)| *t)
}

/// Is `name` an ambient global at all? (Interface objects included, so
/// `import { HTMLElement } from "browser:dom"` binds.)
pub fn is_global(name: &str) -> bool {
    webapi().globals.iter().any(|(n, _)| n == name)
}
