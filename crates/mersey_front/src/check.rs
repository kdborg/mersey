//! TypeExpr checker v1: enforces §3 (strict typing, numeric-only implicit
//! conversion, defined casts) and §4 (access control, readonly, override,
//! abstract, implements) on a bound module.
//!
//! Scope notes for v1 (single-module):
//! - Imported names from unknown modules type as `Any` — precise cross-module
//!   types arrive with the module-graph loader. `std:console`/`browser:dom`
//!   have built-in signatures.
//! - Nullable narrowing is intentionally simple: `x != null` in `if`/`while`
//!   conditions narrows the branch/body; any assignment to `x` un-narrows.
//! - Generic inference at call sites is one-pass unification of parameter
//!   types against argument types; explicit type arguments always win.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{self, *};
use crate::diag::{Code, Diagnostic, Pos};

pub struct CheckOutput {
    pub diagnostics: Vec<Diagnostic>,
    /// Numeric conversions the engine must perform. See [`Coercions`].
    pub coercions: Coercions,
}

/// A runtime numeric conversion the checker decided is needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Num {
    Int(IntKind),
    F32,
    F64,
}

/// The conversions a program needs, keyed by the **address of the AST
/// expression** that produces the value.
///
/// The type system says `let x: float64 = 7` widens the `7` (§3.3, C-style
/// coercion) — but only the *checker* knows that, because only the checker knows
/// what `x` was declared to be. The engine erases types: it sees an `i32` going
/// into a slot, stores an `i32`, and then `x / 2` dispatches on the value it
/// finds and does integer division. The answer was 3.
///
/// So the conversion has to be written down where it is decided. This is that
/// record: the checker fills it in at every place a value crosses into a context
/// with a different numeric type, and the compiler turns each entry into a
/// `Convert` op. It is what makes the bytecode *typed* — not by carrying a type
/// on every value, but by carrying the one thing the untyped form threw away.
///
/// The key is the node's address. The AST is allocated once, leaked, and shared
/// by the checker and the engine — the same nodes, at the same addresses — so it
/// is a stable identity that costs nothing and cannot go stale.
pub type Coercions = std::collections::HashMap<usize, Num>;

/// The identity of an AST expression: where it lives.
pub fn node_id(e: &Expr) -> usize {
    e as *const Expr as usize
}

thread_local! {
    /// Every conversion the checker has decided on, for every program it has
    /// checked on this thread.
    ///
    /// A global, deliberately. The alternative — handing the table to the engine
    /// and asking it to install it — has one failure mode, and it is the worst
    /// one there is: an embedder that forgets produces a program that *runs*,
    /// with silently wrong arithmetic, because the values fall back to whatever
    /// type they happened to have. That is precisely the bug this whole mechanism
    /// exists to remove, and it must not be reintroduced as a way to hold it.
    ///
    /// Checking a program is what makes its conversions available; nothing else
    /// has to be remembered. The table is keyed by AST node address, so entries
    /// from different programs cannot collide, and it only ever grows — which is
    /// what the leaked AST does too.
    static COERCIONS: RefCell<Coercions> = RefCell::new(Coercions::new());
    /// Conversions applied to the *result* of a compound assignment, keyed by
    /// the assignment node rather than by any expression inside it.
    static RESULT_COERCIONS: RefCell<Coercions> = RefCell::new(Coercions::new());
    /// The numeric type both operands of a binary operator have.
    static OP_TYPES: RefCell<Coercions> = RefCell::new(Coercions::new());
    /// The numeric type of each local, keyed by its declaring name.
    static LOCAL_TYPES: RefCell<Coercions> = RefCell::new(Coercions::new());
    /// The default an *uninitialized* binding or field starts with, keyed by the
    /// address of its declared `TypeExpr`. See [`DefaultVal`].
    static DEFAULTS: RefCell<std::collections::HashMap<usize, DefaultVal>> =
        RefCell::new(std::collections::HashMap::new());
    /// Call nodes the checker proved are `<the JSON global>.stringify(...)` —
    /// the receiver's static type is [`Type::Namespace(Ns::Json)`], so this is
    /// the real `JSON`, not a value that happens to be spelled `JSON`. Keyed by
    /// the address of the callee (the `JSON.stringify` member expression). The
    /// bytecode compiler reads this to fuse a stringify of an object literal into
    /// a template, soundly: a shadowed `JSON` never lands here.
    static JSON_STRINGIFY: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
    /// Record-field value expressions the checker proved are `int32`/`int64`,
    /// inside a fusable `JSON.stringify({literal})`. Their decimal template
    /// rendering is byte-identical to `JSON`'s, so the compiler may emit them as
    /// dynamic template parts. Keyed by the value expression's address.
    static JSON_DYN_INT: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Record that this callee is a genuine `JSON.stringify` (receiver typed
/// `Namespace(Ns::Json)`) whose sole object-literal argument is fully fusable,
/// keyed by the callee expression's address.
fn note_json_stringify(callee: &Expr) {
    JSON_STRINGIFY.with(|m| m.borrow_mut().insert(node_id(callee)));
}

/// Whether this callee is a genuine, fusable `JSON.stringify` the checker
/// resolved against the real JSON global. Sound: a local shadowing `JSON` has a
/// different static type and is never recorded here.
pub fn is_json_stringify(callee: &Expr) -> bool {
    JSON_STRINGIFY.with(|m| m.borrow().contains(&node_id(callee)))
}

/// Whether this record-field value is an `int32`/`int64` the checker authorized
/// as a dynamic template part in a fused `JSON.stringify`.
pub fn is_json_dyn_int(value: &Expr) -> bool {
    JSON_DYN_INT.with(|m| m.borrow().contains(&node_id(value)))
}

/// What an uninitialized binding holds the moment it exists.
///
/// `let x: int32;` and `public x: float64;` used to hold `null` — from a binding
/// the type system said was a number. That is the type system lying, and it was
/// not hypothetical: it produced a real Tier 0/Tier 1 divergence, because
/// compiled code believed the declared type and there is no `null` in an `f64`.
///
/// Now a declaration without an initializer starts at its type's zero: numbers
/// at 0, `string` at `""`, `char` at `'\0'`, `bool` at `false`, containers
/// empty. The *checker* decides which, because only the checker can see through
/// a type alias — `type Meters = float64` must default the same way `float64`
/// does — and the engine reads the answer from this table.
///
/// A type with no constructible default — a class, an interface, a function — is
/// absent from the table and still starts as `null`. For a nullable type that is
/// the honest answer; for a non-nullable class it is the one remaining place the
/// declared type can disagree with the value, tracked in the ROADMAP under
/// definite assignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DefaultVal {
    Num(Num),
    BigInt,
    BigDec,
    Str,
    Char,
    Bool,
    Array,
    Map,
    Set,
    Bytes,
}

/// The default for the binding or field declared with this type, if its type
/// has one.
pub fn default_for_ty(t: &TypeExpr) -> Option<DefaultVal> {
    let id = t as *const TypeExpr as usize;
    DEFAULTS.with(|m| m.borrow().get(&id).copied())
}

/// The conversion this expression's value needs, if any (§3.3).
pub fn coercion_for(e: &Expr) -> Option<Num> {
    let id = node_id(e);
    COERCIONS.with(|m| m.borrow().get(&id).copied())
}

/// The conversion the `{ x }` record shorthand's value needs (§3.3).
///
/// The shorthand has no expression of its own to hang a conversion on — the
/// value comes straight from the binding `x` names — so the checker keys it on
/// the field name instead.
pub fn coercion_for_name(n: &Name) -> Option<Num> {
    let id = n as *const Name as usize;
    COERCIONS.with(|m| m.borrow().get(&id).copied())
}

/// For a compound assignment (`a op= b`): the type its result converts back to
/// before it is stored (§3.3 rule 6).
pub fn result_coercion_for(e: &Expr) -> Option<Num> {
    let id = node_id(e);
    RESULT_COERCIONS.with(|m| m.borrow().get(&id).copied())
}

/// The numeric type both operands of this binary operator have, if it is a
/// numeric operator. The engine emits a typed instruction for it, instead of
/// working the types out from the values it happens to find.
pub fn op_type_for(e: &Expr) -> Option<Num> {
    let id = node_id(e);
    OP_TYPES.with(|m| m.borrow().get(&id).copied())
}

/// The numeric type of the local this name declares. A frame slot with a type is
/// a *register*: the JIT can hold it in a machine register of the right width,
/// and a function mixing `int32` and `float64` is no longer a mixed-up one.
pub fn local_type_for(n: &Name) -> Option<Num> {
    let id = n as *const Name as usize;
    LOCAL_TYPES.with(|m| m.borrow().get(&id).copied())
}

fn publish(
    c: &Coercions,
    results: &Coercions,
    ops: &Coercions,
    locals: &Coercions,
    defaults: &std::collections::HashMap<usize, DefaultVal>,
) {
    COERCIONS.with(|m| m.borrow_mut().extend(c.iter().map(|(k, v)| (*k, *v))));
    RESULT_COERCIONS.with(|m| m.borrow_mut().extend(results.iter().map(|(k, v)| (*k, *v))));
    OP_TYPES.with(|m| m.borrow_mut().extend(ops.iter().map(|(k, v)| (*k, *v))));
    LOCAL_TYPES.with(|m| m.borrow_mut().extend(locals.iter().map(|(k, v)| (*k, *v))));
    DEFAULTS.with(|m| m.borrow_mut().extend(defaults.iter().map(|(k, v)| (*k, *v))));
}

/// The numeric kind of a type, if it has one. `bigint`/`bigdec` are absent on
/// purpose: they never mix implicitly with the fixed-width numerics (§3.7), so
/// there is no conversion to record.
fn num_of(t: &Type) -> Option<Num> {
    match t {
        Type::Int(k) => Some(Num::Int(*k)),
        Type::F32 => Some(Num::F32),
        Type::F64 => Some(Num::F64),
        _ => None,
    }
}

/// What an editor asks for: what is this, where is it declared, what can I
/// type here. The checker already computes all of it — this just records it
/// instead of throwing it away.
#[derive(Default)]
pub struct IndexData {
    /// TypeExpr of every expression, keyed by its start position. Nested
    /// expressions share a start (`a`, `a.b`, `a.b.c` all start at `a`), and
    /// the checker visits inner ones first, so the *last* entry at a position
    /// is the outermost — the one worth hovering.
    types: Vec<(Pos, String)>,
    /// Use → declaration.
    /// (use, declaration, name). The name is kept so a rename knows how many
    /// characters each occurrence covers.
    uses: Vec<(Pos, Pos, String)>,
    /// Everything declared, for scope completion.
    syms: Vec<Sym>,
}

struct Sym {
    name: String,
    detail: String,
    pos: Pos,
}

/// One completion item.
pub struct Completion {
    pub label: String,
    pub detail: String,
    /// LSP CompletionItemKind.
    pub kind: u32,
}

const KIND_FIELD: u32 = 5;
const KIND_METHOD: u32 = 2;
const KIND_VARIABLE: u32 = 6;

/// A checked module, kept alive so an editor can ask questions of it.
pub struct Analysis {
    checker: Checker,
    index: IndexData,
    pub diagnostics: Vec<Diagnostic>,
}

/// Check a module and keep everything the checker learned.
pub fn analyze(module: &Module) -> Analysis {
    analyze_graph(&[("<main>".to_string(), module)])
}

/// Analyse a whole graph, dependency-first; the **last** module is the one the
/// editor is asking about. Its imports therefore have real types, so hover,
/// go-to-definition and completion work across files instead of stopping at the
/// module boundary.
pub fn analyze_graph(modules: &[(String, &Module)]) -> Analysis {
    let (mut results, checker, index) = check_graph_indexed(modules, true);
    let diagnostics = results
        .pop()
        .map(|(_, o)| o.diagnostics)
        .unwrap_or_default();
    Analysis {
        checker,
        index,
        diagnostics,
    }
}

impl Analysis {
    /// The type of the thing at `pos` — the outermost expression starting
    /// there, which is what a cursor on `a.b.c` should report.
    pub fn hover(&self, pos: Pos) -> Option<String> {
        self.index
            .types
            .iter()
            .filter(|(p, _)| p.line == pos.line && p.col == pos.col)
            .next_back()
            .map(|(_, t)| t.clone())
    }

    /// Where the name at `pos` was declared.
    pub fn definition(&self, pos: Pos) -> Option<Pos> {
        self.index
            .uses
            .iter()
            .find(|(u, _, _)| u.line == pos.line && u.col == pos.col)
            .map(|(_, d, _)| *d)
    }

    /// Every occurrence of the name at `pos` — its declaration and all its
    /// uses.
    ///
    /// This is the checker's own resolution, not a text search: two variables
    /// with the same name in different scopes are different names, and a rename
    /// that treated them as one would silently change what the program means.
    pub fn references(&self, pos: Pos) -> Option<Vec<(Pos, String)>> {
        // The cursor may be on a use or on the declaration itself.
        let def = self.definition(pos).or_else(|| {
            self.index
                .syms
                .iter()
                .find(|s| s.pos.line == pos.line && s.pos.col == pos.col)
                .map(|s| s.pos)
        })?;
        let name = self
            .index
            .syms
            .iter()
            .find(|s| s.pos == def)
            .map(|s| s.name.clone())
            .or_else(|| {
                self.index
                    .uses
                    .iter()
                    .find(|(_, d, _)| *d == def)
                    .map(|(_, _, n)| n.clone())
            })?;

        let mut out = vec![(def, name.clone())];
        for (u, d, n) in &self.index.uses {
            if *d == def && !out.iter().any(|(p, _)| p == u) {
                out.push((*u, n.clone()));
            }
        }
        out.sort_by_key(|(p, _)| (p.line, p.col));
        Some(out)
    }

    /// Everything this file declares, for an editor's outline.
    pub fn symbols(&self) -> Vec<(String, String, Pos)> {
        let mut out: Vec<(String, String, Pos)> = Vec::new();
        for sym in &self.index.syms {
            // `this` and compiler temporaries are not part of anyone's outline.
            if sym.name == "this" || sym.name.starts_with('#') {
                continue;
            }
            if out.iter().any(|(n, _, p)| n == &sym.name && *p == sym.pos) {
                continue;
            }
            out.push((sym.name.clone(), sym.detail.clone(), sym.pos));
        }
        out.sort_by_key(|(_, _, p)| (p.line, p.col));
        out
    }

    /// The signature of the function named at `pos`, for signature help.
    ///
    /// Not `hover`: a call expression starts at its callee, so the *last* type
    /// recorded there is the call's result (`int32`), not the function
    /// (`(int32, int32) => int32`). The one we want is the innermost — the
    /// callee itself — which is the first function type recorded at that
    /// position.
    pub fn signature(&self, pos: Pos) -> Option<String> {
        self.index
            .types
            .iter()
            .filter(|(p, _)| p.line == pos.line && p.col == pos.col)
            .map(|(_, t)| t)
            .find(|t| t.starts_with('(') && t.contains("=>"))
            .cloned()
    }

    /// Names in scope at `pos`: everything declared at the top level, plus
    /// locals declared before the cursor.
    pub fn scope_completions(&self, pos: Pos) -> Vec<Completion> {
        let mut out: Vec<Completion> = Vec::new();
        for sym in &self.index.syms {
            let before = (sym.pos.line, sym.pos.col) <= (pos.line, pos.col);
            if !before {
                continue;
            }
            if out.iter().any(|c| c.label == sym.name) {
                continue;
            }
            out.push(Completion {
                label: sym.name.clone(),
                detail: sym.detail.clone(),
                kind: KIND_VARIABLE,
            });
        }
        out
    }
}

/// The sentinel an editor's completion request stands in for: the client
/// rewrites `foo.<cursor>` to `foo.MERSEY_COMPLETION_MARKER`, and the checker
/// records what `foo` turned out to be. Positions would not do — a member name
/// in the AST does not carry one — and a marker also survives the parser.
pub const COMPLETION_MARKER: &str = "MERSEY__COMPLETE";

fn to_api(m: Completion) -> ApiMember {
    ApiMember {
        name: m.label,
        signature: m.detail,
        is_fn: m.kind == KIND_METHOD,
    }
}

/// One documented item: a member, with the type the checker gives it.
pub struct ApiMember {
    pub name: String,
    pub signature: String,
    pub is_fn: bool,
}

/// Depth-first post-order over the alias dependency graph: an alias lands in
/// `order` only after everything it names. A back edge (a cycle) is left alone —
/// there is no order that satisfies it, and `resolve_type` reports it.
fn alias_visit(
    i: usize,
    aliases: &[&crate::ast::TypeAliasDecl],
    by_name: &HashMap<&str, usize>,
    state: &mut [u8],
    order: &mut Vec<usize>,
    circular: &mut Vec<usize>,
) {
    if state[i] != 0 {
        return;
    }
    state[i] = 1;
    let mut deps: Vec<usize> = Vec::new();
    let mut self_ref = false;
    type_names(&aliases[i].ty, &mut |name| {
        if let Some(&j) = by_name.get(name) {
            if j == i || state[j] == 1 {
                // Itself, or something still on the stack: a back edge.
                self_ref = true;
            } else if state[j] == 0 {
                deps.push(j);
            }
        }
    });
    if self_ref {
        circular.push(i);
    }
    for j in deps {
        alias_visit(j, aliases, by_name, state, order, circular);
    }
    state[i] = 2;
    order.push(i);
}

/// Every type name written inside a `TypeExpr`.
fn type_names(t: &TypeExpr, f: &mut impl FnMut(&str)) {
    match t {
        TypeExpr::Named { name, args, .. } => {
            f(name);
            for a in args {
                type_names(a, f);
            }
        }
        TypeExpr::Nullable(inner) | TypeExpr::ArrayOf(inner) => type_names(inner, f),
        TypeExpr::Union(parts) | TypeExpr::Tuple(parts) => {
            for p in parts {
                type_names(p, f);
            }
        }
        TypeExpr::Record(members) => {
            for m in members {
                type_names(&m.ty, f);
            }
        }
        TypeExpr::Function { params, ret, .. } => {
            for p in params {
                type_names(&p.ty, f);
            }
            type_names(ret, f);
        }
    }
}

/// A documented group: a `std:` module, or a builtin type.
pub struct ApiGroup {
    pub title: String,
    /// The import that brings it into scope, if any.
    pub import: String,
    /// Stable name for this group's example file (`docs/examples/<key>.mersey`).
    pub key: String,
    /// For a class exported by a `std:` module: that module's key. A class does
    /// not need its own example when the module's example is what shows it being
    /// used — and writing a second one would only be the first one again.
    pub parent: String,
    pub members: Vec<ApiMember>,
}

/// The whole standard library, **as the checker sees it**.
///
/// Not a second description of the API that could drift from the first: every
/// signature here is produced by the same `member_access` that typechecks the
/// call. If a member is added and this list is not updated, the test notices; if
/// this list names something that does not exist, the test notices that too.
pub fn api_reference() -> Vec<ApiGroup> {
    let mut c = Checker::new();
    let n = c.diags.len();
    c.collect(crate::webapi::webapi().module);
    c.diags.truncate(n);

    let mut out = Vec::new();

    let namespaces = [
        Ns::Console,
        Ns::Math,
        Ns::Format,
        Ns::Parse,
        Ns::Json,
        Ns::Time,
        Ns::Random,
        Ns::Regex,
        Ns::Bytes,
        Ns::PromiseNs,
        Ns::Fs,
        Ns::Env,
        Ns::Caps,
        Ns::Gc,
    ];
    for ns in namespaces {
        let members = c
            .members_of(&Type::Namespace(ns))
            .into_iter()
            .map(|m| ApiMember {
                name: m.label,
                signature: m.detail,
                is_fn: m.kind == KIND_METHOD,
            })
            .collect();
        let title = namespace_module(ns).trim_start_matches("std:").to_string();
        out.push(ApiGroup {
            key: title.clone(),
            title,
            parent: String::new(),
            import: namespace_module(ns).to_string(),
            members,
        });
    }

    // The `std:` modules written in Mersey. They are checked here and their
    // exports listed — the same principle as everything else on the page: the
    // reference cannot name an export that does not exist, because it is asking
    // the checker rather than keeping a list.
    for spec in crate::stdlib::source_modules() {
        let Some(src_text) = crate::stdlib::source(spec) else {
            continue;
        };
        let file = crate::source::SourceFile {
            name: (*spec).to_string(),
            text: src_text.to_string(),
        };
        let parsed = crate::parser::parse(&file);
        if !parsed.diagnostics.is_empty() {
            continue;
        }
        let module = parsed.module;
        let (_, mut mc, _) = check_graph_indexed(&[((*spec).to_string(), &module)], false);

        let exports = mc.module_exports.get(*spec).cloned().unwrap_or_default();

        // Exported functions and constants: the module's own surface.
        let mut members: Vec<ApiMember> = exports
            .values
            .iter()
            .filter(|(name, _)| !exports.types.contains_key(*name))
            .map(|(name, ty)| ApiMember {
                name: name.clone(),
                signature: mc.show(ty),
                is_fn: matches!(ty, Type::Fn(_)),
            })
            .collect();
        members.sort_by(|a, b| a.name.cmp(&b.name));

        // Exported classes get a group each, with their public members.
        let mut classes: Vec<(String, Vec<ApiMember>)> = Vec::new();
        for (name, def) in exports.types.iter() {
            let TypeDef::Class(id) = def else { continue };
            let args: Vec<Type> = mc.classes[*id]
                .tparams
                .iter()
                .map(|_| Type::Unknown)
                .collect();
            let ty = Type::Class(*id, Rc::new(args));
            let ms: Vec<ApiMember> = mc.members_of(&ty).into_iter().map(to_api).collect();
            classes.push((name.clone(), ms));
            members.push(ApiMember {
                name: name.clone(),
                signature: "class".to_string(),
                is_fn: false,
            });
        }
        members.sort_by(|a, b| a.name.cmp(&b.name));

        let module_key = spec.trim_start_matches("std:").to_string();
        out.push(ApiGroup {
            key: module_key.clone(),
            title: module_key.clone(),
            parent: String::new(),
            import: (*spec).to_string(),
            members,
        });

        classes.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, ms) in classes {
            if ms.is_empty() {
                continue;
            }
            let mut key = name.to_lowercase();
            if out.iter().any(|g| g.key == key) {
                key.push_str("-class");
            }
            out.push(ApiGroup {
                title: name,
                key,
                parent: module_key.clone(),
                import: (*spec).to_string(),
                members: ms,
            });
        }
    }

    // The web platform (`browser:dom`). The generated surface declares 1355
    // globals and 1103 interfaces — a list of all of them would be a copy of the
    // IDL, not documentation. What the page carries instead is the surface people
    // actually reach for, typed by the same checker as everything else; the rest
    // is still importable, and the editor knows it.
    const DOM_GLOBALS: &[&str] = &[
        "window",
        "document",
        "navigator",
        "location",
        "localStorage",
        "sessionStorage",
        "fetch",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "requestAnimationFrame",
        "crypto",
        "JSON",
        "Intl",
        "URL",
        "URLSearchParams",
        "WebSocket",
        "AbortController",
        "Headers",
        "Request",
        "Response",
        "FormData",
        "Blob",
        "Node",
        "Element",
        "HTMLElement",
        "Event",
        "CustomEvent",
        "EventTarget",
    ];
    let mut dom_members: Vec<ApiMember> = Vec::new();
    for name in DOM_GLOBALS {
        // An interface name imported as a value is the interface object
        // (`Node.ELEMENT_NODE`, `x is Element`) — the same rule the importer uses.
        let ty = if let Some(TypeDef::Iface(iid)) = c.type_defs.get(*name).cloned() {
            Type::IfaceMeta(iid)
        } else {
            match crate::webapi::global_type(name) {
                Some(ast_ty) => c.resolve_type(ast_ty),
                None => continue,
            }
        };
        let signature = c.show(&ty);
        dom_members.push(ApiMember {
            is_fn: matches!(ty, Type::Fn(..)),
            name: (*name).to_string(),
            signature,
        });
    }
    dom_members.sort_by(|a, b| a.name.cmp(&b.name));
    out.push(ApiGroup {
        title: "browser:dom".to_string(),
        key: "dom".to_string(),
        parent: String::new(),
        import: "browser:dom".to_string(),
        members: dom_members,
    });

    // The interfaces those globals hand you. Listed as children of `browser:dom`:
    // one example shows the platform being used, and nine copies of it would not
    // show anything more.
    for name in [
        "Window",
        "Document",
        "Element",
        "Node",
        "Event",
        "Navigator",
        "Storage",
        "Location",
        "WebSocket",
    ] {
        let Some(TypeDef::Iface(iid)) = c.type_defs.get(name).cloned() else {
            continue;
        };
        let mut members: Vec<ApiMember> = c
            .members_of(&Type::Iface(iid, Rc::new(Vec::new())))
            .into_iter()
            .map(to_api)
            .collect();
        members.sort_by(|a, b| a.name.cmp(&b.name));
        out.push(ApiGroup {
            title: name.to_string(),
            key: format!("dom-{}", name.to_lowercase()),
            parent: "dom".to_string(),
            import: "browser:dom".to_string(),
            members,
        });
    }

    // Builtin types: the members you get on a value, not on a module.
    let i32t = Type::Int(IntKind::I32);
    let strings = c.members_of(&Type::Str);
    out.push(ApiGroup {
        title: "string".to_string(),
        key: "string".to_string(),
        parent: String::new(),
        import: String::new(),
        members: strings.into_iter().map(to_api).collect(),
    });

    // `flat` exists only on an array *of arrays* (`T[][] -> T[]`), so asking a
    // plain `int32[]` about it gets an honest "no such member" — and the
    // reference would then be missing a method that works. Ask both.
    let mut array_members: Vec<ApiMember> = c
        .members_of(&Type::Array(Rc::new(i32t.clone())))
        .into_iter()
        .map(to_api)
        .collect();
    let nested = Type::Array(Rc::new(Type::Array(Rc::new(i32t))));
    for m in c.members_of(&nested).into_iter().map(to_api) {
        if !array_members.iter().any(|x| x.name == m.name) {
            array_members.push(m);
        }
    }
    array_members.sort_by(|a, b| a.name.cmp(&b.name));
    out.push(ApiGroup {
        title: "T[] (array)".to_string(),
        key: "array".to_string(),
        parent: String::new(),
        import: String::new(),
        members: array_members,
    });

    // Generic builtin classes (Map, Set, Iter, Regex, bytes, …).
    for name in ["Map", "Set", "Iter", "AsyncIter", "Regex", "bytes"] {
        if let Some(TypeDef::Class(id)) = c.type_defs.get(name).cloned() {
            let args: Vec<Type> = c.classes[id]
                .tparams
                .iter()
                .map(|_| Type::Unknown)
                .collect();
            let ty = Type::Class(id, Rc::new(args));
            let members = c
                .members_of(&ty)
                .into_iter()
                .map(|m| ApiMember {
                    name: m.label,
                    signature: m.detail,
                    is_fn: m.kind == KIND_METHOD,
                })
                .collect();
            // `std:regex` (the module) and `Regex` (the compiled pattern) are
            // different things with the same name; their example files cannot be.
            let mut key = name.to_lowercase();
            if out.iter().any(|g| g.key == key) {
                key.push_str("-class");
            }
            out.push(ApiGroup {
                title: name.to_string(),
                key,
                parent: String::new(),
                import: String::new(),
                members,
            });
        }
    }
    out
}

/// Every member of every `std:` namespace, in one place.
///
/// The checker's `member_type` dispatches on these names; documentation and
/// editor completion *enumerate* them. One list, so a member cannot exist
/// without being documented, or be documented without existing — the test
/// `every_documented_member_exists` checks both directions.
pub fn namespace_members(ns: Ns) -> &'static [&'static str] {
    match ns {
        Ns::Console => &["log", "warn", "error", "info", "debug"],
        Ns::Math => &[
            "abs", "min", "max", "floor", "ceil", "round", "trunc", "sign", "clamp", "sqrt",
            "cbrt", "pow", "exp", "log", "log2", "log10", "hypot", "sin", "cos", "tan", "asin",
            "acos", "atan", "atan2", "isNaN", "isFinite", "PI", "E",
        ],
        Ns::Format => &["pad", "fixed"],
        Ns::Fs => &["readText"],
        Ns::Env => &["get"],
        Ns::Caps => &["has", "list", "drop"],
        Ns::Json => &["stringify", "parse"],
        Ns::Random => &["float", "int", "bytes"],
        Ns::PromiseNs => &["resolve", "reject", "all"],
        Ns::Time => &["now", "monotonic", "parts", "fromParts", "format", "parse"],
        Ns::Gc => &["collect", "stats"],
        Ns::Regex => &["compile"],
        Ns::Parse => &["int32", "int64", "float64", "bigint", "bigdec", "bool"],
        Ns::Bytes => &[
            "alloc",
            "fill",
            "fromHost",
            "toHost",
            "encodeUtf8",
            "decodeUtf8",
        ],
        Ns::Document => &["getElementById", "createElement"],
        Ns::Opaque => &[],
    }
}

/// The `std:` module each namespace is imported from.
pub fn namespace_module(ns: Ns) -> &'static str {
    match ns {
        Ns::Console => "std:console",
        Ns::Math => "std:math",
        Ns::Format => "std:format",
        Ns::Fs => "std:fs",
        Ns::Env => "std:env",
        Ns::Caps => "std:caps",
        Ns::Json => "std:json",
        Ns::Random => "std:random",
        Ns::PromiseNs => "std:async",
        Ns::Time => "std:time",
        Ns::Gc => "std:gc",
        Ns::Regex => "std:regex",
        Ns::Parse => "std:parse",
        Ns::Bytes => "std:bytes",
        Ns::Document => "browser:dom",
        Ns::Opaque => "",
    }
}

/// Member names probed against the builtin types. The checker owns the truth
/// about what an array or a string has; this is just the list of questions.
/// Every member name the builtin *types* have — strings, arrays, Map, Set,
/// Iter, Regex, bytes.
///
/// The checker's `member_access` is the authority on what each one *means*; this
/// is the list of what to ask it about, for documentation and for editor
/// completion. `builtin_members_are_complete` fails if a member is added to the
/// checker and not to this list — a list that quietly goes stale is how a method
/// ends up undocumented and unsuggestable while working perfectly.
const BUILTIN_MEMBERS: &[&str] = &[
    // shared
    "length",
    "size",
    "toString",
    "at",
    "indexOf",
    "lastIndexOf",
    "contains",
    "slice",
    "concat",
    "keys",
    "values",
    "entries",
    "clear",
    // array
    "push",
    "pop",
    "insertAt",
    "removeAt",
    "fillInPlace",
    "flat",
    "join",
    "map",
    "reduce",
    "filter",
    "find",
    "findIndex",
    "some",
    "every",
    "forEach",
    "sortInPlace",
    "reverseInPlace",
    "toSorted",
    "toReversed",
    // string
    "startsWith",
    "endsWith",
    "substring",
    "charAt",
    "codePointAt",
    "split",
    "toUpperCase",
    "toLowerCase",
    "trim",
    "trimStart",
    "trimEnd",
    "replace",
    "replaceAll",
    "repeat",
    "padStart",
    "padEnd",
    // Map / Set
    "get",
    "set",
    "has",
    "add",
    "remove",
    // Iter / generators
    "next",
    "toArray",
    "take",
    // Regex
    "test",
    "exec",
    "findAll",
    // bytes
    "alloc",
    "fill",
    // promises
    "then",
    "catch",
    "finally",
];

fn add_all(out: &mut Vec<Completion>, items: Vec<Completion>) {
    for c in items {
        if !out.iter().any(|e| e.label == c.label) {
            out.push(c);
        }
    }
}

/// Members available on the receiver of `x.MERSEY__COMPLETE` in `module`.
pub fn member_completions(module: &Module) -> Vec<Completion> {
    member_completions_graph(&[("<main>".to_string(), module)])
}

/// The same, over a whole graph — so the receiver may be a type that came from
/// another file, which in a real project it usually is.
pub fn member_completions_graph(modules: &[(String, &Module)]) -> Vec<Completion> {
    let (_, mut c, _) = check_graph_indexed_with(modules, false, true);
    let Some(t) = c.marker_recv.clone() else {
        return Vec::new();
    };
    c.members_of(&t)
}

/// Typecheck a module.
///
/// **`&'static` is load-bearing, not decoration.** The conversions the checker
/// records are keyed by the address of the AST node they belong to, so an AST
/// that is checked and then *freed* would leave entries describing addresses the
/// allocator is free to hand to something else — and the next program parsed
/// into that memory would inherit conversions that were never about it. Nothing
/// catches that: the program runs, with arithmetic quietly done at the wrong
/// width. (The differential fuzzer caught exactly this, which is what it is for.)
///
/// Requiring the module to live forever makes the hazard unrepresentable: an
/// address that is never freed is never reused. Every engine leaks its AST
/// already — it has to, since closures outlive the call that made them — so this
/// costs nothing. The editor, which re-parses on every keystroke and *does* free,
/// goes through `analyze_graph` instead, and publishes nothing.
pub fn check(module: &'static Module) -> CheckOutput {
    let mut out = check_graph(&[("<main>".to_string(), module)]);
    out.pop().map(|(_, o)| o).unwrap_or(CheckOutput {
        diagnostics: Vec::new(),
        coercions: Coercions::new(),
    })
}

/// Check a whole module graph (dependency-first). One `Checker` spans the
/// graph so a class declared in one module is the *same* type when imported
/// into another; scopes and type namespaces are per-module.
pub fn check_graph(modules: &[(String, &'static Module)]) -> Vec<(String, CheckOutput)> {
    let refs: Vec<(String, &Module)> = modules.iter().map(|(s, m)| (s.clone(), *m)).collect();
    check_graph_indexed(&refs, false).0
}

/// The one checking pass, optionally recording an editor index for the *last*
/// module in the graph.
///
/// Both callers go through here on purpose. An editor that typechecks a file on
/// its own sees a different program than the compiler does — imported names
/// have no types, so hover says `<error>` and completion offers nothing — and
/// the two must not be allowed to disagree about what the code means.
fn check_graph_indexed(
    modules: &[(String, &Module)],
    index_last: bool,
) -> (Vec<(String, CheckOutput)>, Checker, IndexData) {
    check_graph_indexed_with(modules, index_last, false)
}

fn check_graph_indexed_with(
    modules: &[(String, &Module)],
    index_last: bool,
    want_marker: bool,
) -> (Vec<(String, CheckOutput)>, Checker, IndexData) {
    let mut c = Checker::new();
    c.want_marker = want_marker;
    // Ambient web platform (generated from WebIDL); its own collection
    // diagnostics are suppressed — the generator is validated separately.
    // Collected only when some module in the graph actually reaches the web
    // surface: otherwise the ~700 KB WebIDL would parse on every run for
    // nothing. `touches_web` reuses the binder's own complete type walk. The
    // marker/index path (LSP) always collects, so completion still offers web
    // types before the file names one.
    if want_marker || modules.iter().any(|(_, m)| crate::bind::touches_web(m)) {
        let n = c.diags.len();
        c.collect(crate::webapi::webapi().module);
        c.diags.truncate(n);
    }

    let base_types = c.type_defs.clone();
    let base_scope = c.scopes[0].clone();
    let mut exports: HashMap<String, ModuleExports> = HashMap::new();
    let mut results = Vec::new();
    let mut index = IndexData::default();

    let last = modules.len().saturating_sub(1);
    for (i, (spec, module)) in modules.iter().enumerate() {
        c.diags.clear();
        c.type_defs = base_types.clone();
        c.scopes = vec![base_scope.clone(), HashMap::new()];
        c.module_spec = spec.clone();
        c.imported.clear();
        // Only the file the editor is asking about is indexed; its
        // dependencies are checked purely to give its imports real types.
        c.index = if index_last && i == last {
            Some(IndexData::default())
        } else {
            None
        };

        // Bind this module's relative imports from already-checked modules.
        for item in &module.items {
            let Item::Import(im) = item else { continue };
            if !crate::graph::is_module(&im.from) {
                continue;
            }
            let target = crate::graph::resolve_module(spec, &im.from);
            let Some(exp) = exports.get(&target) else {
                continue; // missing module: the loader reports it
            };
            match &im.clause {
                Some(ImportClause::Named(specs)) => {
                    for s in specs {
                        let local = s.alias.as_ref().unwrap_or(&s.name);
                        let mut found = false;
                        if let Some(v) = exp.values.get(&s.name.text) {
                            c.define(&local.text, v.clone(), true);
                            found = true;
                        }
                        if let Some(t) = exp.types.get(&s.name.text) {
                            c.type_defs.insert(local.text.clone(), t.clone());
                            c.imported.insert(local.text.clone());
                            found = true;
                        }
                        if !found {
                            let msg = format!("`{}` is not exported by `{}`", s.name.text, im.from);
                            c.error(Code::UndefinedName, msg, s.name.pos);
                        }
                    }
                }
                Some(ImportClause::Namespace(n)) => {
                    // `import * as m from "./x.mersey"` — a record of the
                    // module's exported values, precisely typed.
                    let mut fields: Vec<RecField> = exp
                        .values
                        .iter()
                        .map(|(name, ty)| RecField {
                            name: name.clone(),
                            ty: ty.clone(),
                            optional: false,
                        })
                        .collect();
                    fields.sort_by(|a, b| a.name.cmp(&b.name));
                    c.define(&n.text, Type::Record(Rc::new(fields)), true);
                }
                None => {}
            }
        }

        c.collect(module);
        c.check_module(module);

        // Publish this module's exports for its dependents.
        let mut e = ModuleExports::default();
        for item in &module.items {
            let Item::Export(ex) = item else { continue };
            let mut publish = |name: &str, c: &Checker| {
                if let Some(v) = c.lookup_scope(name) {
                    e.values.insert(name.to_string(), v.ty.clone());
                }
                if let Some(t) = c.type_defs.get(name) {
                    e.types.insert(name.to_string(), t.clone());
                }
            };
            match &ex.kind {
                ExportKind::Decl(d) => {
                    let name = match d {
                        Decl::Function(f) => &f.name.text,
                        Decl::Class(cl) => &cl.name.text,
                        Decl::Interface(i) => &i.name.text,
                        Decl::Enum(en) => &en.name.text,
                        Decl::TypeAlias(t) => &t.name.text,
                    };
                    publish(name, &c);
                }
                ExportKind::Var(v) => {
                    for b in &v.bindings {
                        let mut names = Vec::new();
                        pattern_names(&b.target, &mut names);
                        for n in names {
                            publish(n, &c);
                        }
                    }
                }
                ExportKind::Named { specs, .. } => {
                    for s in specs {
                        let exported = s.alias.as_ref().unwrap_or(&s.name);
                        if let Some(v) = c.lookup_scope(&s.name.text) {
                            e.values.insert(exported.text.clone(), v.ty.clone());
                        }
                        if let Some(t) = c.type_defs.get(&s.name.text) {
                            e.types.insert(exported.text.clone(), t.clone());
                        }
                    }
                }
            }
        }
        exports.insert(spec.clone(), e.clone());
        c.module_exports.insert(spec.clone(), e);

        let mut diagnostics = std::mem::take(&mut c.diags);
        diagnostics.sort_by_key(|d| (d.pos.line, d.pos.col));
        // The coercions are keyed by node address, so they are unique across the
        // whole graph and each module's output can carry the whole table so far.
        let coercions = std::mem::take(&mut c.coercions);
        let result_coercions = std::mem::take(&mut c.result_coercions);
        let op_types = std::mem::take(&mut c.op_types);
        let local_types = std::mem::take(&mut c.local_types);
        let defaults = std::mem::take(&mut c.defaults);
        // The editor does not publish. It re-checks on every keystroke, dropping
        // the AST each time, and an address that has been freed can be handed to
        // the *next* allocation — so its entries would both pile up forever and
        // come to describe nodes they were never about. Nothing the editor checks
        // is ever executed, so it has no conversions to contribute.
        if !index_last {
            publish(
                &coercions,
                &result_coercions,
                &op_types,
                &local_types,
                &defaults,
            );
        }
        results.push((
            spec.clone(),
            CheckOutput {
                diagnostics,
                coercions,
            },
        ));
        if index_last && i == last {
            index = c.index.take().unwrap_or_default();
        }
    }
    (results, c, index)
}

#[derive(Clone, Default)]
struct ModuleExports {
    values: HashMap<String, Type>,
    types: HashMap<String, TypeDef>,
}

/// The value of a string literal, if `e` is one.
fn string_literal(e: &Expr) -> Option<String> {
    match e {
        Expr::Paren(inner) => string_literal(inner),
        Expr::Lit {
            kind: LitKind::Str,
            text,
            ..
        } => Some(crate::ast::string_value(text)),
        _ => None,
    }
}

fn pattern_names<'a>(p: &'a Pattern, out: &mut Vec<&'a str>) {
    match p {
        Pattern::Name(n) => out.push(&n.text),
        Pattern::Array { elems, rest } => {
            for e in elems {
                pattern_names(&e.target, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Record(fields) => {
            for f in fields {
                match &f.target {
                    Some(t) => pattern_names(t, out),
                    None => out.push(&f.name.text),
                }
            }
        }
    }
}

// ---- types ----------------------------------------------------------------

pub type ClassId = usize;
pub type IfaceId = usize;
pub type EnumId = usize;
pub type TvId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IntKind {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntKind {
    fn name(self) -> &'static str {
        use IntKind::*;
        match self {
            I8 => "int8",
            I16 => "int16",
            I32 => "int32",
            I64 => "int64",
            U8 => "uint8",
            U16 => "uint16",
            U32 => "uint32",
            U64 => "uint64",
        }
    }
    fn signed(self) -> bool {
        matches!(
            self,
            IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64
        )
    }
    fn bits(self) -> u32 {
        use IntKind::*;
        match self {
            I8 | U8 => 8,
            I16 | U16 => 16,
            I32 | U32 => 32,
            I64 | U64 => 64,
        }
    }
}

#[derive(Clone)]
/// A type as it *means*: resolved, canonical, comparable.
///
/// The counterpart of `ast::TypeExpr`, which is a type as *written*. Two
/// different `TypeExpr`s (`int32[]`, and an alias for it) resolve to one
/// `Type` — which is what makes a type comparable at all.
pub enum Type {
    /// The sound top type: what a value has when it crosses into the program
    /// from outside the type system — a JSON document, a JS host object.
    ///
    /// Anything is assignable *to* it; it is assignable *from* nothing. You
    /// cannot read a member of it, call it, or index it. To use one you must
    /// narrow it — `as T` (checked at runtime) or `instanceof`.
    ///
    /// It replaces `any`, which was assignable in *both* directions and
    /// permitted *any* member: not a type, but a hole in the checker that
    /// spread to everything it touched.
    Unknown,
    /// Poison type: an error was already reported; stay quiet downstream.
    Err,
    Void,
    Null,
    Bool,
    Char,
    Str,
    BigInt,
    BigDec,
    Int(IntKind),
    F32,
    F64,
    Nullable(Rc<Type>),
    Array(Rc<Type>),
    Tuple(Rc<Vec<Type>>),
    Record(Rc<Vec<RecField>>),
    Fn(Rc<FnType>),
    Class(ClassId, Rc<Vec<Type>>),
    Iface(IfaceId, Rc<Vec<Type>>),
    Enum(EnumId),
    /// The class object itself (statics, `instanceof` RHS).
    ClassMeta(ClassId),
    /// The host *interface object* (`window.HTMLElement`) — the right-hand
    /// side of `x instanceof HTMLElement`, plus its statics/constants.
    IfaceMeta(IfaceId),
    /// The enum object itself (`Color.RED`).
    EnumMeta(EnumId),
    /// Built-in namespaces (`console`, `document`); Any-typed namespace for
    /// unknown imports.
    Namespace(Ns),
    Var(TvId),
    Union(Rc<Vec<Type>>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Ns {
    Console,
    Document,
    Bytes,
    Time,
    Gc,
    Regex,
    Parse,
    Math,
    Format,
    Fs,
    Env,
    Caps,
    Json,
    Random,
    /// The `Promise` value from `std:async` (`resolve`/`reject`/`all`), as
    /// distinct from the `Promise<T>` *type*.
    PromiseNs,
    Opaque,
}

#[derive(Clone)]
pub struct RecField {
    pub name: String,
    pub ty: Type,
    pub optional: bool,
}

#[derive(Clone)]
pub struct FnType {
    pub tparams: Vec<TvId>,
    pub params: Vec<ParamType>,
    pub ret: Type,
}

#[derive(Clone)]
pub struct ParamType {
    pub ty: Type,
    pub optional: bool,
    pub rest: bool,
}

// ---- declaration tables ------------------------------------------------------

struct FieldInfo {
    name: String,
    ty: Type,
    access: Access,
    is_static: bool,
    readonly: bool,
}

struct MethodInfo {
    name: String,
    sig: FnType,
    access: Access,
    is_static: bool,
    is_abstract: bool,
    is_final: bool,
    has_override: bool,
}

struct AccessorInfo {
    name: String,
    ty: Type, // getter return / setter param
    access: Access,
}

struct ClassInfo {
    name: String,
    tparams: Vec<TvId>,
    parent: Option<(ClassId, Vec<Type>)>,
    /// `class X extends HTMLElement` — instances ARE host objects: members
    /// not declared in Mersey resolve against this interface, and the class
    /// is assignable wherever the interface is expected.
    host_parent: Option<(IfaceId, Vec<Type>)>,
    ifaces: Vec<(IfaceId, Vec<Type>)>,
    fields: Vec<FieldInfo>,
    methods: Vec<MethodInfo>,
    getters: Vec<AccessorInfo>,
    setters: Vec<AccessorInfo>,
    ctor: Option<(Vec<ParamType>, Access)>,
    is_abstract: bool,
    is_final: bool,
}

struct IfaceMember {
    name: String,
    ty: Type, // property type or Fn
    optional: bool,
    /// A `readonly` property, or one declared with only a `get` accessor: it can
    /// be read through the interface and not written.
    readonly: bool,
}

struct IfaceInfo {
    name: String,
    tparams: Vec<TvId>,
    extends: Vec<(IfaceId, Vec<Type>)>,
    members: Vec<IfaceMember>,
}

struct EnumInfo {
    name: String,
    members: Vec<String>,
}

struct AliasInfo {
    tparams: Vec<TvId>,
    target: Type,
}

#[derive(Clone)]
enum TypeDef {
    Class(ClassId),
    Iface(IfaceId),
    Enum(EnumId),
    Alias(usize),
    Imported,
}

// ---- checker --------------------------------------------------------------------

#[derive(Clone)]
struct VarInfo {
    ty: Type,
    is_const: bool,
    /// Where this name was declared — what "go to definition" jumps to.
    def: Option<Pos>,
}

struct Checker {
    diags: Vec<Diagnostic>,
    /// Editor index; `None` during ordinary compilation, so it costs nothing.
    index: Option<IndexData>,
    /// Completion: capture the receiver type of `x.MERSEY__COMPLETE`.
    want_marker: bool,
    marker_recv: Option<Type>,
    /// Exports of the modules checked so far, so a dynamic `import("./x")` can
    /// be given the precise type of what it will produce.
    module_exports: HashMap<String, ModuleExports>,
    /// Declared bound for each type parameter (`<T extends Comparable<T>>`).
    tv_bounds: HashMap<TvId, Type>,
    /// Numeric conversions the engine must perform. See [`Coercions`].
    coercions: Coercions,
    /// Conversions applied to the *result* of a compound assignment, keyed by
    /// the assignment node. See [`result_coercion_for`].
    result_coercions: Coercions,
    /// The numeric type both operands of a binary operator have, keyed by the
    /// operator node. See [`op_type_for`].
    op_types: Coercions,
    /// The numeric type of a local, keyed by the name that declares it. The
    /// engine gives every local a frame slot; this is what that slot holds.
    local_types: Coercions,
    /// Zero-defaults for uninitialized bindings and fields, keyed by the address
    /// of the declared `TypeExpr`. See [`DefaultVal`].
    defaults: std::collections::HashMap<usize, DefaultVal>,
    classes: Vec<ClassInfo>,
    ifaces: Vec<IfaceInfo>,
    enums: Vec<EnumInfo>,
    aliases: Vec<AliasInfo>,
    type_defs: HashMap<String, TypeDef>,
    tv_count: usize,
    tv_names: Vec<String>,
    /// Value scopes (innermost last).
    scopes: Vec<HashMap<String, VarInfo>>,
    /// Narrowing overlays (innermost last); consulted before scopes.
    narrows: Vec<HashMap<String, Type>>,
    /// TypeExpr-parameter scopes for resolution.
    tp_scopes: Vec<HashMap<String, TvId>>,
    current_class: Option<ClassId>,
    in_ctor: bool,
    in_static: bool,
    ret_ty: Option<Type>,
    // Built-in class ids
    error_id: ClassId,
    element_id: ClassId,
    /// The generated `interface Promise<T>` (webapi), resolved lazily.
    promise_id: Option<IfaceId>,
    bytes_id: Option<ClassId>,
    regex_id: Option<ClassId>,
    iter_id: Option<ClassId>,
    numeric_id: Option<IfaceId>,
    iterable_id: Option<IfaceId>,
    async_iterable_id: Option<IfaceId>,
    display_id: Option<IfaceId>,
    async_iter_id: Option<ClassId>,
    /// Element type of the generator currently being checked.
    yield_ty: Option<Type>,
    /// The module being checked (diagnostics/context).
    module_spec: String,
    /// TypeExpr names pulled in from other modules (not declared here).
    imported: std::collections::HashSet<String>,
}

const PREDEFINED: &[(&str, Type)] = &[
    // The top type is writable: a function that means to accept anything says
    // so (`f(v: unknown)`), rather than being handed one only at a boundary.
    ("unknown", Type::Unknown),
    ("bool", Type::Bool),
    ("char", Type::Char),
    ("string", Type::Str),
    ("bigint", Type::BigInt),
    ("bigdec", Type::BigDec),
    ("void", Type::Void),
    ("int", Type::Int(IntKind::I32)),
    ("int8", Type::Int(IntKind::I8)),
    ("int16", Type::Int(IntKind::I16)),
    ("int32", Type::Int(IntKind::I32)),
    ("int64", Type::Int(IntKind::I64)),
    ("uint", Type::Int(IntKind::U32)),
    ("uint8", Type::Int(IntKind::U8)),
    ("uint16", Type::Int(IntKind::U16)),
    ("uint32", Type::Int(IntKind::U32)),
    ("uint64", Type::Int(IntKind::U64)),
    ("float", Type::F64),
    ("float32", Type::F32),
    ("float64", Type::F64),
];

fn predefined(name: &str) -> Option<Type> {
    PREDEFINED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.clone())
}

impl Checker {
    fn new() -> Checker {
        let mut c = Checker {
            diags: Vec::new(),
            index: None,
            want_marker: false,
            marker_recv: None,
            module_exports: HashMap::new(),
            tv_bounds: HashMap::new(),
            coercions: Coercions::new(),
            result_coercions: Coercions::new(),
            op_types: Coercions::new(),
            local_types: Coercions::new(),
            defaults: std::collections::HashMap::new(),
            classes: Vec::new(),
            ifaces: Vec::new(),
            enums: Vec::new(),
            aliases: Vec::new(),
            type_defs: HashMap::new(),
            tv_count: 0,
            tv_names: Vec::new(),
            scopes: vec![HashMap::new()],
            narrows: Vec::new(),
            tp_scopes: Vec::new(),
            current_class: None,
            in_ctor: false,
            in_static: false,
            ret_ty: None,
            error_id: 0,
            element_id: 0,
            promise_id: None,
            bytes_id: None,
            regex_id: None,
            iter_id: None,
            numeric_id: None,
            iterable_id: None,
            async_iterable_id: None,
            display_id: None,
            async_iter_id: None,
            yield_ty: None,
            module_spec: String::new(),
            imported: std::collections::HashSet::new(),
        };
        c.install_builtins();
        c
    }

    fn install_builtins(&mut self) {
        let str_param = ParamType {
            ty: Type::Str,
            optional: true,
            rest: false,
        };
        // Error hierarchy (spec §4.6).
        let error_id = self.classes.len();
        self.error_id = error_id;
        self.classes.push(ClassInfo {
            name: "Error".into(),
            tparams: vec![],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![
                FieldInfo {
                    name: "message".into(),
                    ty: Type::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: false,
                },
                FieldInfo {
                    name: "stack".into(),
                    ty: Type::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: true,
                },
            ],
            methods: vec![],
            getters: vec![],
            setters: vec![],
            ctor: Some((vec![str_param.clone()], Access::Public)),
            is_abstract: false,
            is_final: false,
        });
        self.type_defs
            .insert("Error".into(), TypeDef::Class(error_id));
        for name in ["RangeError", "TypeError"] {
            let id = self.classes.len();
            self.classes.push(ClassInfo {
                name: name.into(),
                tparams: vec![],
                parent: Some((error_id, vec![])),
                host_parent: None,
                ifaces: vec![],
                fields: vec![],
                methods: vec![],
                getters: vec![],
                setters: vec![],
                ctor: Some((vec![str_param.clone()], Access::Public)),
                is_abstract: false,
                is_final: false,
            });
            self.type_defs.insert(name.into(), TypeDef::Class(id));
        }
        // Element (Stage A DOM handle; lazily bound, hence non-null).
        let element_id = self.classes.len();
        self.element_id = element_id;
        self.classes.push(ClassInfo {
            name: "Element".into(),
            tparams: vec![],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![
                FieldInfo {
                    name: "textContent".into(),
                    ty: Type::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: false,
                },
                FieldInfo {
                    name: "value".into(),
                    ty: Type::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: false,
                },
            ],
            methods: vec![MethodInfo {
                name: "addEventListener".into(),
                sig: FnType {
                    tparams: vec![],
                    params: vec![
                        ParamType {
                            ty: Type::Str,
                            optional: false,
                            rest: false,
                        },
                        ParamType {
                            ty: Type::Fn(Rc::new(FnType {
                                tparams: vec![],
                                params: vec![],
                                ret: Type::Void,
                            })),
                            optional: false,
                            rest: false,
                        },
                    ],
                    ret: Type::Void,
                },
                access: Access::Public,
                is_static: false,
                is_abstract: false,
                is_final: false,
                has_override: false,
            }],
            getters: vec![],
            setters: vec![],
            ctor: None,
            is_abstract: true,
            is_final: false,
        });
        // appendChild / remove
        let elem_ty = Type::Class(element_id, Rc::new(vec![]));
        for (name, params, ret) in [
            (
                "appendChild",
                vec![ParamType {
                    ty: elem_ty.clone(),
                    optional: false,
                    rest: false,
                }],
                Type::Void,
            ),
            ("remove", vec![], Type::Void),
        ] {
            self.classes[element_id].methods.push(MethodInfo {
                name: name.into(),
                sig: FnType {
                    tparams: vec![],
                    params,
                    ret,
                },
                access: Access::Public,
                is_static: false,
                is_abstract: false,
                is_final: false,
                has_override: false,
            });
        }
        self.type_defs
            .insert("Element".into(), TypeDef::Class(element_id));

        for name in ["Error", "RangeError", "TypeError"] {
            let TypeDef::Class(id) = self.type_defs[name] else {
                unreachable!()
            };
            self.scopes[0].insert(
                name.to_string(),
                VarInfo {
                    ty: Type::ClassMeta(id),
                    is_const: true,
                    def: None,
                },
            );
        }
        self.install_collections();
        self.install_bytes();
        self.install_regex();
        self.install_numeric();
        self.install_iter();
        self.install_async_iter();
        self.install_protocols();
    }

    /// `Iter<T>` — what a generator returns and what `for … of` consumes.
    /// `Numeric`: the bound that lets a width-preserving function be *generic*
    /// instead of untyped.
    ///
    /// `math.abs` used to be `(any) => any` purely so that `abs(-3)` could give
    /// back an `int32` and `abs(-3.5)` a `float64` — which meant `math.abs("hi")`
    /// typechecked. It is properly `abs<T: Numeric>(x: T): T`: one signature,
    /// every numeric width, and nothing else.
    ///
    /// It is a marker with no members: nothing is *called* through it, it only
    /// says which types may stand in for `T`.
    fn install_numeric(&mut self) {
        let id = self.ifaces.len();
        self.ifaces.push(IfaceInfo {
            name: "Numeric".into(),
            tparams: vec![],
            extends: vec![],
            members: vec![],
        });
        self.type_defs.insert("Numeric".into(), TypeDef::Iface(id));
        self.numeric_id = Some(id);
    }

    /// `Iterable<T>`, `AsyncIterable<T>`, `Display`: the protocols a class can opt
    /// into.
    ///
    /// JavaScript spells these with well-known symbols — `Symbol.iterator`,
    /// `Symbol.toPrimitive`. A symbol-keyed method is a *runtime convention the
    /// type system cannot see*: nothing tells you that you forgot it, nothing
    /// checks its signature, and no editor can suggest it. An interface is the
    /// same extension point with none of the invisibility — declared, checked at
    /// the class rather than discovered at the call site, and visible in the type.
    fn install_protocols(&mut self) {
        // Iterable<T> { iter(): Iter<T> }
        let t = self.fresh_tv("T");
        let elem = Type::Var(t);
        let iter_ret = self.iter_of(elem.clone());
        let id = self.ifaces.len();
        self.ifaces.push(IfaceInfo {
            name: "Iterable".into(),
            tparams: vec![t],
            extends: vec![],
            members: vec![IfaceMember {
                name: "iter".into(),
                ty: Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: iter_ret,
                })),
                optional: false,
                readonly: false,
            }],
        });
        self.type_defs.insert("Iterable".into(), TypeDef::Iface(id));
        self.iterable_id = Some(id);

        // AsyncIterable<T> { iter(): AsyncIter<T> }
        let t = self.fresh_tv("T");
        let elem = Type::Var(t);
        let aiter_ret = self.async_iter_of(elem);
        let id = self.ifaces.len();
        self.ifaces.push(IfaceInfo {
            name: "AsyncIterable".into(),
            tparams: vec![t],
            extends: vec![],
            members: vec![IfaceMember {
                name: "iter".into(),
                ty: Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: aiter_ret,
                })),
                optional: false,
                readonly: false,
            }],
        });
        self.type_defs
            .insert("AsyncIterable".into(), TypeDef::Iface(id));
        self.async_iterable_id = Some(id);

        // Display { toString(): string }
        let id = self.ifaces.len();
        self.ifaces.push(IfaceInfo {
            name: "Display".into(),
            tparams: vec![],
            extends: vec![],
            members: vec![IfaceMember {
                name: "toString".into(),
                ty: Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: Type::Str,
                })),
                optional: false,
                readonly: false,
            }],
        });
        self.type_defs.insert("Display".into(), TypeDef::Iface(id));
        self.display_id = Some(id);
    }

    /// The type arguments this class supplies to `iface`, if it implements it —
    /// including through a base class.
    fn implemented_args(&self, class: ClassId, iface: IfaceId) -> Option<Vec<Type>> {
        let mut cur = Some(class);
        while let Some(id) = cur {
            let info = &self.classes[id];
            if let Some((_, args)) = info.ifaces.iter().find(|(i, _)| *i == iface) {
                return Some(args.clone());
            }
            cur = info.parent.as_ref().map(|(p, _)| *p);
        }
        None
    }

    /// A type parameter bounded by `Numeric`, ready to use in a signature.
    fn numeric_tv(&mut self) -> TvId {
        let tv = self.fresh_tv("T");
        if let Some(id) = self.numeric_id {
            self.tv_bounds.insert(tv, Type::Iface(id, Rc::new(vec![])));
        }
        tv
    }

    fn install_iter(&mut self) {
        let t = self.fresh_tv("T");
        let tv = Type::Var(t);
        let id = self.classes.len();
        // `map`/`filter`/`take` are lazy: each returns a new `Iter` that pulls
        // from this one, so `it.map(f).take(3)` runs the generator three times
        // rather than to exhaustion.
        let u = self.fresh_tv("U");
        let self_ty = Type::Class(id, Rc::new(vec![tv.clone()]));
        let p = |ty: Type| ParamType {
            ty,
            optional: false,
            rest: false,
        };
        let meth = |name: &str, tparams: Vec<TvId>, params: Vec<ParamType>, ret: Type| MethodInfo {
            name: name.to_string(),
            sig: FnType {
                tparams,
                params,
                ret,
            },
            access: Access::Public,
            is_static: false,
            is_abstract: false,
            is_final: false,
            has_override: false,
        };
        let mapper = Type::Fn(Rc::new(FnType {
            tparams: vec![],
            params: vec![p(tv.clone())],
            ret: Type::Var(u),
        }));
        let pred = Type::Fn(Rc::new(FnType {
            tparams: vec![],
            params: vec![p(tv.clone())],
            ret: Type::Bool,
        }));
        let adapters = vec![
            meth(
                "map",
                vec![u],
                vec![p(mapper)],
                Type::Class(id, Rc::new(vec![Type::Var(u)])),
            ),
            meth("filter", vec![], vec![p(pred)], self_ty.clone()),
            meth("take", vec![], vec![p(Type::Int(IntKind::I32))], self_ty),
        ];
        self.classes.push(ClassInfo {
            name: "Iter".into(),
            tparams: vec![t],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![],
            methods: vec![
                MethodInfo {
                    name: "next".into(),
                    sig: FnType {
                        tparams: vec![],
                        params: vec![],
                        ret: nullable(tv.clone()),
                    },
                    access: Access::Public,
                    is_static: false,
                    is_abstract: false,
                    is_final: false,
                    has_override: false,
                },
                MethodInfo {
                    name: "toArray".into(),
                    sig: FnType {
                        tparams: vec![],
                        params: vec![],
                        ret: Type::Array(Rc::new(tv)),
                    },
                    access: Access::Public,
                    is_static: false,
                    is_abstract: false,
                    is_final: false,
                    has_override: false,
                },
            ]
            .into_iter()
            .chain(adapters)
            .collect(),
            getters: vec![],
            setters: vec![],
            ctor: None,
            is_abstract: false,
            is_final: true,
        });
        self.type_defs.insert("Iter".into(), TypeDef::Class(id));
        self.iter_id = Some(id);
    }

    /// `Iter<T>` for a given element type.
    fn iter_of(&mut self, t: Type) -> Type {
        match self.iter_id {
            Some(id) => Type::Class(id, Rc::new(vec![t])),
            None => Type::Err,
        }
    }

    fn unwrap_iter(&self, t: &Type) -> Option<Type> {
        let id = self.iter_id?;
        match strip_null(t) {
            Type::Class(cid, args) if cid == id => Some(args.first().cloned().unwrap_or(Type::Err)),
            _ => None,
        }
    }

    /// `AsyncIter<T>`: what an `async` function containing `yield` returns.
    ///
    /// Mersey has no `function*`: a function that yields *is* a generator, so an
    /// async one needs no new syntax either. Its `next()` returns a promise of
    /// the next element — which is exactly what `for await` consumes.
    fn install_async_iter(&mut self) {
        let t = self.fresh_tv("T");
        let tv = Type::Var(t);
        let next_ret = self.promise_of(nullable(tv.clone()));
        let id = self.classes.len();
        self.classes.push(ClassInfo {
            name: "AsyncIter".into(),
            tparams: vec![t],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![],
            methods: vec![MethodInfo {
                name: "next".into(),
                sig: FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: next_ret,
                },
                access: Access::Public,
                is_static: false,
                is_abstract: false,
                is_final: false,
                has_override: false,
            }],
            getters: vec![],
            setters: vec![],
            ctor: None,
            is_abstract: false,
            is_final: true,
        });
        self.type_defs
            .insert("AsyncIter".into(), TypeDef::Class(id));
        self.async_iter_id = Some(id);
    }

    /// Give `AsyncIter<T>.next()` its real return type once `Promise` exists.
    ///
    /// `next()` returns `Promise<T?>`, but `Promise` is declared by the web
    /// surface, which is collected *after* the builtins are installed. Until
    /// then `promise_of` has no interface to name and yields `Err` — and `Err`
    /// is assignable in both directions, so `const s: string = await it.next()`
    /// typechecked against anything at all. An async iterator is one of the few
    /// places where a hole is invisible: the `await` looks like it is doing the
    /// work. Link the signature the moment `Promise` is known.
    fn link_async_iter(&mut self) {
        let Some(id) = self.async_iter_id else {
            return;
        };
        let Some(m) = self.classes[id].methods.first() else {
            return;
        };
        if !matches!(m.sig.ret, Type::Err) {
            return;
        }
        let tv = Type::Var(self.classes[id].tparams[0]);
        let ret = self.promise_of(nullable(tv));
        if !matches!(ret, Type::Err) {
            self.classes[id].methods[0].sig.ret = ret;
        }
    }

    fn async_iter_of(&mut self, t: Type) -> Type {
        match self.async_iter_id {
            Some(id) => Type::Class(id, Rc::new(vec![t])),
            None => Type::Err,
        }
    }

    fn unwrap_async_iter(&self, t: &Type) -> Option<Type> {
        let id = self.async_iter_id?;
        match strip_null(t) {
            Type::Class(cid, args) if cid == id => Some(args.first().cloned().unwrap_or(Type::Err)),
            _ => None,
        }
    }

    /// `Regex` (from `std:regex`) and the record a match produces.
    fn install_regex(&mut self) {
        let i32t = Type::Int(IntKind::I32);
        let match_ty = Type::Record(Rc::new(vec![
            RecField {
                name: "text".into(),
                ty: Type::Str,
                optional: false,
            },
            RecField {
                name: "start".into(),
                ty: i32t.clone(),
                optional: false,
            },
            RecField {
                name: "end".into(),
                ty: i32t.clone(),
                optional: false,
            },
            RecField {
                name: "groups".into(),
                ty: Type::Array(Rc::new(nullable(Type::Str))),
                optional: false,
            },
        ]));
        let p = |ty: Type| ParamType {
            ty,
            optional: false,
            rest: false,
        };
        let m = |name: &str, params: Vec<ParamType>, ret: Type| MethodInfo {
            name: name.into(),
            sig: FnType {
                tparams: vec![],
                params,
                ret,
            },
            access: Access::Public,
            is_static: false,
            is_abstract: false,
            is_final: false,
            has_override: false,
        };
        let id = self.classes.len();
        self.classes.push(ClassInfo {
            name: "Regex".into(),
            tparams: vec![],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![],
            methods: vec![
                m("test", vec![p(Type::Str)], Type::Bool),
                m("find", vec![p(Type::Str)], nullable(match_ty.clone())),
                m(
                    "findAll",
                    vec![p(Type::Str)],
                    Type::Array(Rc::new(match_ty)),
                ),
                // `replace` does the first match only; `replaceAll` does all of
                // them. Neither name can be mistaken for the other (§1.3).
                m("replace", vec![p(Type::Str), p(Type::Str)], Type::Str),
                m("replaceAll", vec![p(Type::Str), p(Type::Str)], Type::Str),
                m("split", vec![p(Type::Str)], Type::Array(Rc::new(Type::Str))),
            ],
            getters: vec![],
            setters: vec![],
            ctor: None,
            is_abstract: false,
            is_final: true,
        });
        self.type_defs.insert("Regex".into(), TypeDef::Class(id));
        self.regex_id = Some(id);
    }

    /// `Bytes`: packed byte buffer with O(1) element access (spec §3.8-ish;
    /// the engine-side home for pixel/audio/binary data).
    fn install_bytes(&mut self) {
        let id = self.classes.len();
        self.classes.push(ClassInfo {
            name: "Bytes".into(),
            tparams: vec![],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "length".into(),
                ty: Type::Int(IntKind::I32),
                access: Access::Public,
                is_static: false,
                readonly: true,
            }],
            methods: vec![],
            getters: vec![],
            setters: vec![],
            ctor: None,
            is_abstract: false,
            is_final: true,
        });
        self.type_defs.insert("Bytes".into(), TypeDef::Class(id));
        self.bytes_id = Some(id);
    }

    /// Built-in generic collections (spec §3.8): Map<K,V>, Set<T>. Methods
    /// follow the consistent-API rules (§1.3): mutators return void/bool,
    /// views are verbs.
    fn install_collections(&mut self) {
        let kv = (self.fresh_tv("K"), self.fresh_tv("V"));
        let m = |tparams: Vec<TvId>, params: Vec<ParamType>, ret: Type| MethodInfo {
            name: String::new(),
            sig: FnType {
                tparams,
                params,
                ret,
            },
            access: Access::Public,
            is_static: false,
            is_abstract: false,
            is_final: false,
            has_override: false,
        };
        let p = |ty: Type| ParamType {
            ty,
            optional: false,
            rest: false,
        };
        let (k, v) = (Type::Var(kv.0), Type::Var(kv.1));

        let mut map_methods = Vec::new();
        for (name, params, ret) in [
            ("set", vec![p(k.clone()), p(v.clone())], Type::Void),
            ("get", vec![p(k.clone())], nullable(v.clone())),
            ("has", vec![p(k.clone())], Type::Bool),
            ("remove", vec![p(k.clone())], Type::Bool),
            ("keys", vec![], Type::Array(Rc::new(k.clone()))),
            ("values", vec![], Type::Array(Rc::new(v.clone()))),
            (
                "entries",
                vec![],
                Type::Array(Rc::new(Type::Tuple(Rc::new(vec![k.clone(), v.clone()])))),
            ),
            ("clear", vec![], Type::Void),
        ] {
            let mut mi = m(vec![], params, ret);
            mi.name = name.to_string();
            map_methods.push(mi);
        }
        let map_id = self.classes.len();
        self.classes.push(ClassInfo {
            name: "Map".into(),
            tparams: vec![kv.0, kv.1],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "size".into(),
                ty: Type::Int(IntKind::I32),
                access: Access::Public,
                is_static: false,
                readonly: true,
            }],
            methods: map_methods,
            getters: vec![],
            setters: vec![],
            ctor: Some((vec![], Access::Public)),
            is_abstract: false,
            is_final: true,
        });
        self.type_defs.insert("Map".into(), TypeDef::Class(map_id));
        self.scopes[0].insert(
            "Map".into(),
            VarInfo {
                ty: Type::ClassMeta(map_id),
                is_const: true,
                def: None,
            },
        );

        let t = self.fresh_tv("T");
        let tv = Type::Var(t);
        let mut set_methods = Vec::new();
        for (name, params, ret) in [
            ("add", vec![p(tv.clone())], Type::Void),
            ("has", vec![p(tv.clone())], Type::Bool),
            ("remove", vec![p(tv.clone())], Type::Bool),
            ("values", vec![], Type::Array(Rc::new(tv.clone()))),
            ("clear", vec![], Type::Void),
        ] {
            let mut mi = m(vec![], params, ret);
            mi.name = name.to_string();
            set_methods.push(mi);
        }
        let set_id = self.classes.len();
        self.classes.push(ClassInfo {
            name: "Set".into(),
            tparams: vec![t],
            parent: None,
            host_parent: None,
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "size".into(),
                ty: Type::Int(IntKind::I32),
                access: Access::Public,
                is_static: false,
                readonly: true,
            }],
            methods: set_methods,
            getters: vec![],
            setters: vec![],
            ctor: Some((vec![], Access::Public)),
            is_abstract: false,
            is_final: true,
        });
        self.type_defs.insert("Set".into(), TypeDef::Class(set_id));
        self.scopes[0].insert(
            "Set".into(),
            VarInfo {
                ty: Type::ClassMeta(set_id),
                is_const: true,
                def: None,
            },
        );
    }

    fn error(&mut self, code: Code, msg: impl Into<String>, pos: Pos) {
        self.diags.push(Diagnostic::error(code, msg, pos));
    }

    fn fresh_tv(&mut self, name: &str) -> TvId {
        let id = self.tv_count;
        self.tv_count += 1;
        self.tv_names.push(name.to_string());
        id
    }

    // ---- type display ------------------------------------------------------

    fn show(&self, t: &Type) -> String {
        match t {
            Type::Unknown => "unknown".into(),
            Type::Err => "<error>".into(),
            Type::Void => "void".into(),
            Type::Null => "null".into(),
            Type::Bool => "bool".into(),
            Type::Char => "char".into(),
            Type::Str => "string".into(),
            Type::BigInt => "bigint".into(),
            Type::BigDec => "bigdec".into(),
            Type::Int(k) => k.name().into(),
            Type::F32 => "float32".into(),
            Type::F64 => "float64".into(),
            Type::Nullable(t) => format!("{}?", self.show(t)),
            Type::Array(t) => format!("{}[]", self.show(t)),
            Type::Tuple(ts) => {
                format!(
                    "[{}]",
                    ts.iter()
                        .map(|t| self.show(t))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Type::Record(fs) => format!(
                "{{{}}}",
                fs.iter()
                    .map(|f| format!("{}: {}", f.name, self.show(&f.ty)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Type::Fn(f) => format!(
                "({}) => {}",
                f.params
                    .iter()
                    .map(|p| {
                        // A rest parameter and an optional one are *part of the
                        // signature*. Printing `(unknown) => void` for
                        // `console.log` said it takes exactly one argument,
                        // which is not what it does — in the reference, and in
                        // every error message that ever showed a signature.
                        let ty = self.show(&p.ty);
                        if p.rest {
                            format!("...{ty}[]")
                        } else if p.optional {
                            format!("{ty}?")
                        } else {
                            ty
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                self.show(&f.ret)
            ),
            Type::Class(id, args) | Type::Iface(id, args) => {
                let name = match t {
                    Type::Class(..) => &self.classes[*id].name,
                    _ => &self.ifaces[*id].name,
                };
                if args.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{name}<{}>",
                        args.iter()
                            .map(|a| self.show(a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Enum(id) => self.enums[*id].name.clone(),
            Type::ClassMeta(id) => format!("class {}", self.classes[*id].name),
            Type::IfaceMeta(id) => format!("interface {}", self.ifaces[*id].name),
            Type::EnumMeta(id) => format!("enum {}", self.enums[*id].name),
            Type::Namespace(_) => "namespace".into(),
            Type::Var(tv) => self.tv_names[*tv].clone(),
            Type::Union(arms) => arms
                .iter()
                .map(|a| self.show(a))
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }

    // ---- scopes ---------------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, ty: Type, is_const: bool) {
        self.scopes.last_mut().unwrap().insert(
            name.to_string(),
            VarInfo {
                ty,
                is_const,
                def: None,
            },
        );
    }

    /// Define a name and remember where it was written, for the editor.
    fn define_at(&mut self, name: &str, ty: Type, is_const: bool, pos: Pos) {
        if let Some(ix) = &mut self.index {
            ix.syms.push(Sym {
                name: name.to_string(),
                detail: String::new(), // filled in below (needs &self)
                pos,
            });
        }
        let detail = self.show(&ty);
        if let Some(ix) = &mut self.index {
            if let Some(last) = ix.syms.last_mut() {
                last.detail = detail.clone();
            }
            // A declaration is not an expression, so nothing else would record
            // a type here — but hovering the name you are declaring is the
            // most natural thing an editor user does.
            ix.types.push((pos, detail));
        }
        self.scopes.last_mut().unwrap().insert(
            name.to_string(),
            VarInfo {
                ty,
                is_const,
                def: Some(pos),
            },
        );
    }

    fn lookup(&self, name: &str) -> Option<VarInfo> {
        for n in self.narrows.iter().rev() {
            if let Some(t) = n.get(name) {
                // Narrow overlays refine the type; const-ness from scope.
                let base = self.lookup_scope(name);
                let base = base.clone();
                return Some(VarInfo {
                    ty: t.clone(),
                    is_const: base.as_ref().map(|b| b.is_const).unwrap_or(false),
                    def: base.and_then(|b| b.def),
                });
            }
        }
        self.lookup_scope(name)
    }

    fn lookup_scope(&self, name: &str) -> Option<VarInfo> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Some(v.clone());
            }
        }
        None
    }

    fn kill_narrow(&mut self, path: &str) {
        let prefix = format!("{path}.");
        for n in &mut self.narrows {
            n.retain(|k, _| k != path && !k.starts_with(&prefix));
        }
    }

    // ---- collection (headers) ----------------------------------------------------

    fn collect(&mut self, module: &Module) {
        // Phase A: allocate ids so mutual references resolve.
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => d,
                _ => continue,
            };
            match d {
                Decl::Class(c) => {
                    let id = self.classes.len();
                    self.classes.push(ClassInfo {
                        name: c.name.text.clone(),
                        tparams: vec![],
                        parent: None,
                        host_parent: None,
                        ifaces: vec![],
                        fields: vec![],
                        methods: vec![],
                        getters: vec![],
                        setters: vec![],
                        ctor: None,
                        is_abstract: c.is_abstract,
                        is_final: c.is_final,
                    });
                    self.type_defs
                        .insert(c.name.text.clone(), TypeDef::Class(id));
                }
                Decl::Interface(i) => {
                    let id = self.ifaces.len();
                    self.ifaces.push(IfaceInfo {
                        name: i.name.text.clone(),
                        tparams: vec![],
                        extends: vec![],
                        members: vec![],
                    });
                    self.type_defs
                        .insert(i.name.text.clone(), TypeDef::Iface(id));
                }
                Decl::Enum(e) => {
                    let id = self.enums.len();
                    self.enums.push(EnumInfo {
                        name: e.name.text.clone(),
                        members: e.members.iter().map(|(n, _)| n.text.clone()).collect(),
                    });
                    self.type_defs
                        .insert(e.name.text.clone(), TypeDef::Enum(id));
                }
                Decl::TypeAlias(t) => {
                    let id = self.aliases.len();
                    self.aliases.push(AliasInfo {
                        tparams: vec![],
                        target: Type::Err, // placeholder; the header pass fills it in
                    });
                    self.type_defs
                        .insert(t.name.text.clone(), TypeDef::Alias(id));
                }
                Decl::Function(_) => {}
            }
        }
        // Imports: names typed Any unless built-in modules.
        for item in &module.items {
            if let Item::Import(im) = item {
                self.collect_import(im);
            }
        }
        // Phase B0: allocate every declaration's type parameters first,
        // so headers can reference each other regardless of source order.
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => d,
                _ => continue,
            };
            match d {
                Decl::Class(c) => {
                    let tvs: Vec<TvId> = c
                        .type_params
                        .iter()
                        .map(|tp| self.fresh_tv(&tp.name.text))
                        .collect();
                    if let TypeDef::Class(id) = self.type_defs[&c.name.text] {
                        self.classes[id].tparams = tvs;
                    }
                }
                Decl::Interface(i) => {
                    let tvs: Vec<TvId> = i
                        .type_params
                        .iter()
                        .map(|tp| self.fresh_tv(&tp.name.text))
                        .collect();
                    if let TypeDef::Iface(id) = self.type_defs[&i.name.text] {
                        self.ifaces[id].tparams = tvs;
                    }
                }
                Decl::TypeAlias(t) => {
                    let tvs: Vec<TvId> = t
                        .type_params
                        .iter()
                        .map(|tp| self.fresh_tv(&tp.name.text))
                        .collect();
                    if let TypeDef::Alias(id) = self.type_defs[&t.name.text] {
                        self.aliases[id].tparams = tvs;
                    }
                }
                _ => {}
            }
        }
        // Phase B: resolve headers. Aliases first, in dependency order — see
        // `collect_alias_headers`.
        self.collect_alias_headers(module);
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => d,
                _ => continue,
            };
            self.collect_decl_header(d);
        }
        self.link_async_iter();
    }

    /// Resolve type aliases in *dependency* order, not source order.
    ///
    /// An alias is expanded where it is named, so `type A = { p: B }` needs `B`
    /// to be resolved already. Phase A gives every alias an id with an `Err`
    /// placeholder, and if `B` is declared *below* `A`, source order bakes that
    /// placeholder into `A` for good — `A` ends up with a poisoned field that
    /// silently accepts every value, because `Err` is assignable both ways. That
    /// is exactly what happened to `RequestInit.privateToken` in the generated
    /// web surface, where the declarations come out in whatever order the IDL
    /// was in. Nobody writing Mersey should have to order their types.
    ///
    /// A cycle (`type A = { b: B }; type B = { a: A }`) has no valid order at
    /// all: an alias is *expanded* where it is named, and expanding a cycle
    /// never terminates. It is reported (E0414) rather than left to collapse
    /// into the placeholder, which would poison the field and let it accept
    /// every value — the same silent hole, just harder to find. A type that
    /// refers to itself is what a `class` is for.
    fn collect_alias_headers(&mut self, module: &Module) {
        let mut aliases: Vec<&crate::ast::TypeAliasDecl> = Vec::new();
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => d,
                _ => continue,
            };
            if let Decl::TypeAlias(t) = d {
                aliases.push(t);
            }
        }
        let by_name: HashMap<&str, usize> = aliases
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.text.as_str(), i))
            .collect();

        let mut state = vec![0u8; aliases.len()]; // 0 = unvisited, 1 = on stack, 2 = done
        let mut order: Vec<usize> = Vec::new();
        let mut circular: Vec<usize> = Vec::new();
        for i in 0..aliases.len() {
            alias_visit(i, &aliases, &by_name, &mut state, &mut order, &mut circular);
        }
        for i in order {
            self.resolve_alias_header(aliases[i]);
        }
        for i in circular {
            self.error(
                Code::CircularTypeAlias,
                format!(
                    "type alias `{}` refers to itself; use a class for a recursive type",
                    aliases[i].name.text
                ),
                aliases[i].name.pos,
            );
        }
    }

    fn resolve_alias_header(&mut self, t: &crate::ast::TypeAliasDecl) {
        let TypeDef::Alias(id) = self.type_defs[&t.name.text] else {
            return;
        };
        let tvs = self.aliases[id].tparams.clone();
        self.push_tp_scope(&t.type_params, &tvs);
        let target = self.resolve_type(&t.ty);
        self.tp_scopes.pop();
        self.aliases[id].target = target;
    }

    fn collect_import(&mut self, im: &ImportDecl) {
        // Module imports (relative files, and `std:` modules written in
        // Mersey) are bound precisely by `check_graph` — don't clobber those.
        if crate::graph::is_module(&im.from) {
            return;
        }
        let Some(clause) = &im.clause else { return };
        match clause {
            ImportClause::Namespace(n) => {
                self.define(&n.text, Type::Namespace(Ns::Opaque), true);
            }
            ImportClause::Named(specs) => {
                for s in specs {
                    let local = s.alias.as_ref().unwrap_or(&s.name);
                    let ty = match (im.from.as_str(), s.name.text.as_str()) {
                        ("std:console", _) => Type::Namespace(Ns::Console),
                        ("std:bytes", _) => Type::Namespace(Ns::Bytes),
                        ("std:time", _) => Type::Namespace(Ns::Time),
                        ("std:gc", _) => Type::Namespace(Ns::Gc),
                        ("std:regex", _) => Type::Namespace(Ns::Regex),
                        ("std:parse", _) => Type::Namespace(Ns::Parse),
                        ("std:math", _) => Type::Namespace(Ns::Math),
                        ("std:format", _) => Type::Namespace(Ns::Format),
                        ("std:fs", _) => Type::Namespace(Ns::Fs),
                        ("std:env", _) => Type::Namespace(Ns::Env),
                        ("std:caps", _) => Type::Namespace(Ns::Caps),
                        ("std:json", _) => Type::Namespace(Ns::Json),
                        ("std:random", _) => Type::Namespace(Ns::Random),
                        ("std:async", _) => Type::Namespace(Ns::PromiseNs),
                        ("browser:dom", global) => {
                            // An interface NAME imported as a value is the
                            // interface object: `x instanceof HTMLElement`.
                            if let Some(TypeDef::Iface(iid)) = self.type_defs.get(global) {
                                Type::IfaceMeta(*iid)
                            } else {
                                match crate::webapi::global_type(global) {
                                    Some(ast_ty) => self.resolve_type(ast_ty),
                                    None => Type::Unknown, // a host global with no IDL type
                                }
                            }
                        }
                        // A module nobody has heard of. This used to fall
                        // through to `any`, which meant a typo — `std:consoel` —
                        // compiled clean, bound every name it imported to `any`,
                        // and turned type checking off for all of them until it
                        // finally died at runtime. In a language whose whole
                        // premise is that mistakes are compile errors, that is
                        // the one thing the import must not do.
                        (other, _) => {
                            self.error(
                                Code::UnknownTypeName,
                                format!("unknown module `{other}`"),
                                s.name.pos,
                            );
                            Type::Err
                        }
                    };
                    self.define_at(&local.text, ty, true, local.pos);
                    // Importing a *value* must not shadow a *type* of the same
                    // name. `import { Node } from "browser:dom"` brings in the
                    // interface object (for `Node.ELEMENT_NODE` and `instanceof`)
                    // — it does not make the type `Node` mean "imported, who
                    // knows". That mistake was invisible while the fallback was
                    // `any`; with `unknown` it turns every `x as Node` into a
                    // value you cannot use.
                    if !self.type_defs.contains_key(&local.text) {
                        self.type_defs.insert(local.text.clone(), TypeDef::Imported);
                    }
                }
            }
        }
    }

    fn push_tp_scope(&mut self, tps: &[TypeParam], tvs: &[TvId]) {
        let mut scope = HashMap::new();
        for (tp, tv) in tps.iter().zip(tvs) {
            scope.insert(tp.name.text.clone(), *tv);
        }
        self.tp_scopes.push(scope);
        // Bounds may mention the parameters themselves (F-bounded:
        // `<T extends Comparable<T>>`), so resolve them inside the scope.
        for (tp, tv) in tps.iter().zip(tvs) {
            if let Some(c) = &tp.constraint {
                let bound = self.resolve_type(c);
                self.tv_bounds.insert(*tv, bound);
            }
        }
    }

    fn bind_tparams(&mut self, tps: &[TypeParam]) -> Vec<TvId> {
        let mut scope = HashMap::new();
        let mut ids = Vec::new();
        for tp in tps {
            let id = self.fresh_tv(&tp.name.text);
            scope.insert(tp.name.text.clone(), id);
            ids.push(id);
        }
        self.tp_scopes.push(scope);
        for (tp, tv) in tps.iter().zip(&ids) {
            if let Some(c) = &tp.constraint {
                let bound = self.resolve_type(c);
                self.tv_bounds.insert(*tv, bound);
            }
        }
        ids
    }

    fn collect_decl_header(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => {
                let tvs = self.bind_tparams(&f.type_params);
                let params = self.resolve_params(&f.params);
                let ret = match &f.ret {
                    Some(t) => self.resolve_type(t),
                    None => {
                        self.error(
                            Code::BadReturn,
                            format!(
                                "module-level function `{}` must declare its return type",
                                f.name.text
                            ),
                            f.name.pos,
                        );
                        Type::Err
                    }
                };
                self.tp_scopes.pop();
                // `async function f(): T` — the body returns T, callers get
                // Promise<T> (an already-Promise<…> annotation is kept).
                let ret = if f.is_async && body_yields(&f.body) {
                    // An async generator: `async function f(): int32` with
                    // `yield` in the body hands callers an `AsyncIter<int32>`,
                    // which is what `for await` consumes.
                    if self.unwrap_async_iter(&ret).is_none() {
                        self.async_iter_of(ret)
                    } else {
                        ret
                    }
                } else if f.is_async && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
                } else if body_yields(&f.body) && self.unwrap_iter(&ret).is_none() {
                    // A generator: `function f(): int32` with `yield` in the
                    // body hands callers an `Iter<int32>`.
                    self.iter_of(ret)
                } else {
                    ret
                };
                let fnty = Type::Fn(Rc::new(FnType {
                    tparams: tvs,
                    params,
                    ret,
                }));
                self.define_at(&f.name.text, fnty, true, f.name.pos);
            }
            Decl::Class(c) => {
                let TypeDef::Class(id) = self.type_defs[&c.name.text] else {
                    return;
                };
                let tvs = self.classes[id].tparams.clone();
                self.push_tp_scope(&c.type_params, &tvs);

                let mut host_parent = None;
                let parent = c.extends.as_ref().and_then(|t| {
                    let rt = self.resolve_type(t);
                    match rt {
                        Type::Class(pid, args) => {
                            if self.classes[pid].is_final {
                                self.error(
                                    Code::BadOverride,
                                    format!(
                                        "cannot extend `{}`: it is final",
                                        self.classes[pid].name
                                    ),
                                    c.name.pos,
                                );
                            }
                            Some((pid, args.as_ref().clone()))
                        }
                        // Host-backed class: instances ARE host objects.
                        Type::Iface(iid, args) => {
                            host_parent = Some((iid, args.as_ref().clone()));
                            None
                        }
                        Type::Err => None,
                        _ => {
                            self.error(
                                Code::TypeMismatch,
                                "`extends` must name a class or a host interface",
                                c.name.pos,
                            );
                            None
                        }
                    }
                });
                self.classes[id].parent = parent;
                // A Mersey base class may itself be host-backed: inherit that.
                if host_parent.is_none() {
                    if let Some((pid, _)) = &self.classes[id].parent {
                        host_parent = self.classes[*pid].host_parent.clone();
                    }
                }
                self.classes[id].host_parent = host_parent;
                let mut ifaces = Vec::new();
                for t in &c.implements {
                    match self.resolve_type(t) {
                        Type::Iface(iid, args) => ifaces.push((iid, args.as_ref().clone())),
                        Type::Err => {}
                        _ => self.error(
                            Code::TypeMismatch,
                            "`implements` must name an interface",
                            c.name.pos,
                        ),
                    }
                }
                self.classes[id].ifaces = ifaces;

                for m in &c.members {
                    self.collect_member(id, m, c.name.pos);
                }
                self.tp_scopes.pop();
            }
            Decl::Interface(i) => {
                let TypeDef::Iface(id) = self.type_defs[&i.name.text] else {
                    return;
                };
                let tvs = self.ifaces[id].tparams.clone();
                self.push_tp_scope(&i.type_params, &tvs);
                let mut extends = Vec::new();
                for t in &i.extends {
                    match self.resolve_type(t) {
                        Type::Iface(iid, args) => extends.push((iid, args.as_ref().clone())),
                        Type::Err => {}
                        _ => self.error(
                            Code::TypeMismatch,
                            "an interface can only extend interfaces",
                            i.name.pos,
                        ),
                    }
                }
                self.ifaces[id].extends = extends;
                let mut members = Vec::new();
                for m in &i.members {
                    match m {
                        InterfaceMember::Prop {
                            readonly,
                            name,
                            optional,
                            ty,
                        } => {
                            let t = self.resolve_type(ty);
                            members.push(IfaceMember {
                                name: name.clone(),
                                ty: t,
                                optional: *optional,
                                readonly: *readonly,
                            });
                        }
                        InterfaceMember::Method {
                            name,
                            type_params,
                            params,
                            ret,
                        } => {
                            let tvs = self.bind_tparams(type_params);
                            let params = self.resolve_params(params);
                            let ret = self.resolve_type(ret);
                            self.tp_scopes.pop();
                            members.push(IfaceMember {
                                name: name.clone(),
                                ty: Type::Fn(Rc::new(FnType {
                                    tparams: tvs,
                                    params,
                                    ret,
                                })),
                                optional: false,
                                // A method is not a property; you cannot assign
                                // to it either way.
                                readonly: true,
                            });
                        }
                    }
                }
                self.ifaces[id].members = members;
            }
            Decl::Enum(e) => {
                let TypeDef::Enum(id) = self.type_defs[&e.name.text] else {
                    return;
                };
                self.define_at(&e.name.text, Type::EnumMeta(id), true, e.name.pos);
            }
            Decl::TypeAlias(t) => self.resolve_alias_header(t),
        }
        // Class values (constructors as values / statics).
        if let Decl::Class(c) = d {
            if let TypeDef::Class(id) = self.type_defs[&c.name.text] {
                self.define_at(&c.name.text, Type::ClassMeta(id), true, c.name.pos);
            }
        }
    }

    fn collect_member(&mut self, id: ClassId, m: &ClassMember, class_pos: Pos) {
        match m {
            ClassMember::Field {
                mods,
                readonly,
                name,
                ty,
                ..
            } => {
                let t = self.resolve_type(ty);
                self.classes[id].fields.push(FieldInfo {
                    name: name.clone(),
                    ty: t,
                    access: mods.access.unwrap_or(Access::Private),
                    is_static: mods.is_static,
                    readonly: *readonly,
                });
            }
            ClassMember::Method {
                mods,
                is_async,
                name,
                type_params,
                params,
                ret,
                body,
            } => {
                let tvs = self.bind_tparams(type_params);
                let params = self.resolve_params(params);
                let ret = self.resolve_type(ret);
                self.tp_scopes.pop();
                // A method is typed by exactly the same rules as a function —
                // including the one for a generator. An `async` method whose body
                // yields is an *async generator*, so it returns `AsyncIter<T>`,
                // not `Promise<AsyncIter<T>>`. That rule was applied to functions
                // and not to methods, so a class could not implement
                // `AsyncIterable<T>` at all: the signature it was required to
                // have was one the checker would not let it write.
                let yields = body.as_ref().is_some_and(|b| body_yields(b));
                let ret = if *is_async && yields {
                    if self.unwrap_async_iter(&ret).is_none() {
                        self.async_iter_of(ret)
                    } else {
                        ret
                    }
                } else if *is_async && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
                } else if yields && self.unwrap_iter(&ret).is_none() {
                    self.iter_of(ret)
                } else {
                    ret
                };
                let is_abstract = mods.virt == Some(Virt::Abstract) || body.is_none();
                if is_abstract && !self.classes[id].is_abstract {
                    self.error(
                        Code::BadOverride,
                        format!("abstract method `{name}` requires an abstract class"),
                        class_pos,
                    );
                }
                self.classes[id].methods.push(MethodInfo {
                    name: name.clone(),
                    sig: FnType {
                        tparams: tvs,
                        params,
                        ret,
                    },
                    access: mods.access.unwrap_or(Access::Private),
                    is_static: mods.is_static,
                    is_abstract,
                    is_final: mods.virt == Some(Virt::Final),
                    has_override: mods.virt == Some(Virt::Override),
                });
            }
            ClassMember::Getter {
                mods, name, ret, ..
            } => {
                let t = self.resolve_type(ret);
                self.classes[id].getters.push(AccessorInfo {
                    name: name.clone(),
                    ty: t,
                    access: mods.access.unwrap_or(Access::Private),
                });
            }
            ClassMember::Setter {
                mods, name, param, ..
            } => {
                let t = param
                    .ty
                    .as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Type::Err);
                self.classes[id].setters.push(AccessorInfo {
                    name: name.clone(),
                    ty: t,
                    access: mods.access.unwrap_or(Access::Private),
                });
            }
            ClassMember::Ctor { access, params, .. } => {
                let params = self.resolve_params(params);
                self.classes[id].ctor = Some((params, access.unwrap_or(Access::Private)));
            }
        }
    }

    fn resolve_params(&mut self, params: &[Param]) -> Vec<ParamType> {
        params
            .iter()
            .map(|p| {
                let mut ty =
                    p.ty.as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or(Type::Err);
                // A parameter is a local like any other, and it gets slot 0..n.
                if !p.rest {
                    self.note_local(&p.target, &ty);
                }
                if p.rest {
                    // `...xs: int32[]` — the per-argument type is the element.
                    ty = match ty {
                        Type::Array(e) => e.as_ref().clone(),
                        Type::Err => Type::Err,
                        other => {
                            self.error(
                                Code::TypeMismatch,
                                format!(
                                    "rest parameter needs an array type, got `{}`",
                                    self.show(&other)
                                ),
                                pattern_pos(&p.target),
                            );
                            Type::Err
                        }
                    };
                }
                if p.optional {
                    ty = nullable(ty);
                }
                ParamType {
                    ty,
                    optional: p.optional || p.default.is_some(),
                    rest: p.rest,
                }
            })
            .collect()
    }

    // ---- type resolution -----------------------------------------------------------

    fn resolve_type(&mut self, t: &ast::TypeExpr) -> Type {
        match t {
            ast::TypeExpr::Named { name, pos, args } => self.resolve_named(name, *pos, args),
            ast::TypeExpr::Nullable(inner) => nullable(self.resolve_type(inner)),
            ast::TypeExpr::ArrayOf(inner) => Type::Array(Rc::new(self.resolve_type(inner))),
            ast::TypeExpr::Union(arms) => {
                let tys: Vec<Type> = arms.iter().map(|a| self.resolve_type(a)).collect();
                fold_union(tys)
            }
            ast::TypeExpr::Tuple(ts) => {
                Type::Tuple(Rc::new(ts.iter().map(|t| self.resolve_type(t)).collect()))
            }
            ast::TypeExpr::Record(members) => {
                let fs = members
                    .iter()
                    .map(|m| RecField {
                        name: m.name.clone(),
                        ty: self.resolve_type(&m.ty),
                        optional: m.optional,
                    })
                    .collect();
                Type::Record(Rc::new(fs))
            }
            ast::TypeExpr::Function { params, ret, .. } => {
                let params = params
                    .iter()
                    .map(|p| ParamType {
                        ty: self.resolve_type(&p.ty),
                        optional: p.optional,
                        rest: p.rest,
                    })
                    .collect();
                let ret = self.resolve_type(ret);
                Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params,
                    ret,
                }))
            }
        }
    }

    fn resolve_named(&mut self, name: &str, pos: Pos, args: &[ast::TypeExpr]) -> Type {
        let rargs: Vec<Type> = args.iter().map(|a| self.resolve_type(a)).collect();
        // TypeExpr parameters shadow everything.
        for scope in self.tp_scopes.iter().rev() {
            if let Some(tv) = scope.get(name) {
                return Type::Var(*tv);
            }
        }
        // IDL `any`/`object`: a value from JavaScript, where there are no static
        // types to import. The honest type for it is `unknown`.
        if name == "JsAny" {
            return Type::Unknown;
        }
        if let Some(t) = predefined(name) {
            return t;
        }
        // A *namespaced* constructor — `new Intl.NumberFormat(…)`. JavaScript has
        // namespaces holding constructors; Mersey does not. The dotted name is
        // resolved by joining its segments (`Intl.NumberFormat` ->
        // `IntlNumberFormat`), which is how the generated surface declares it —
        // while the engine still hands the *written* name to the host, which
        // walks the path and constructs the real thing.
        let joined: String;
        let name: &str = if name.contains('.') {
            joined = name.split('.').collect();
            if !self.type_defs.contains_key(&joined) {
                return Type::Unknown;
            }
            &joined
        } else {
            name
        };
        match self.type_defs.get(name) {
            Some(TypeDef::Class(id)) => {
                let id = *id;
                self.check_arity(name, self.classes[id].tparams.len(), rargs.len(), pos);
                let tvs = self.classes[id].tparams.clone();
                self.check_bounds(&tvs, &rargs, name, pos);
                Type::Class(id, Rc::new(rargs))
            }
            Some(TypeDef::Iface(id)) => {
                let id = *id;
                if name == "Promise" {
                    self.promise_id = Some(id);
                }
                self.check_arity(name, self.ifaces[id].tparams.len(), rargs.len(), pos);
                Type::Iface(id, Rc::new(rargs))
            }
            Some(TypeDef::Enum(id)) => Type::Enum(*id),
            Some(TypeDef::Alias(id)) => {
                let id = *id;
                let info = &self.aliases[id];
                let (tvs, target) = (info.tparams.clone(), info.target.clone());
                self.check_arity(name, tvs.len(), rargs.len(), pos);
                let map: HashMap<TvId, Type> = tvs.into_iter().zip(rargs).collect();
                subst(&target, &map)
            }
            Some(TypeDef::Imported) => Type::Unknown,
            None => Type::Err, // binder already reported E0308
        }
    }

    /// `Promise<T>` for the generated interface (falls back to `any`).
    fn promise_of(&mut self, t: Type) -> Type {
        let id = match self.promise_id {
            Some(id) => id,
            None => match self.type_defs.get("Promise") {
                Some(TypeDef::Iface(id)) => {
                    self.promise_id = Some(*id);
                    *id
                }
                _ => return Type::Err,
            },
        };
        Type::Iface(id, Rc::new(vec![t]))
    }

    /// `T` from `Promise<T>`; `None` if not a promise.
    fn unwrap_promise(&mut self, t: &Type) -> Option<Type> {
        let pid = self.promise_id.or(match self.type_defs.get("Promise") {
            Some(TypeDef::Iface(id)) => Some(*id),
            _ => None,
        })?;
        match strip_null(t) {
            Type::Iface(id, args) if id == pid => Some(args.first().cloned().unwrap_or(Type::Err)),
            _ => None,
        }
    }

    fn check_arity(&mut self, name: &str, want: usize, got: usize, pos: Pos) {
        if want != got {
            self.error(
                Code::BadCall,
                format!("`{name}` takes {want} type argument(s), got {got}"),
                pos,
            );
        }
    }

    /// `<T extends Comparable<T>>` — a type argument must satisfy its bound.
    /// Can a *value* be tested against this type at run time?
    ///
    /// Primitives and nominal types, yes: the value either holds an int32 or it
    /// does not, and an object either descends from a class or it does not.
    /// Structural types (records) and function types, no — a record type is its
    /// shape, and two different declarations with the same fields are the same
    /// type, so there is nothing at run time that distinguishes them.
    fn testable(&self, t: &Type) -> bool {
        matches!(
            t,
            Type::Bool
                | Type::Char
                | Type::Str
                | Type::BigInt
                | Type::BigDec
                | Type::Int(_)
                | Type::F32
                | Type::F64
                | Type::Null
                | Type::Class(..)
                | Type::Iface(..)
                | Type::Enum(_)
                | Type::Array(_)
                | Type::Err
        )
    }

    /// Check a statement list, carrying a guard clause's narrowing forward.
    ///
    /// `if (x is int32) { return …; }` tells you something about the code
    /// *after* it too: whatever else `x` is, it is not an int32. A guard whose
    /// body always leaves — returns, throws, breaks, continues — makes the rest
    /// of the block its else-branch, which is what lets a program be written as
    /// a series of guards rather than a staircase of nested `else`s.
    fn check_stmts_flowing(&mut self, stmts: &[Stmt]) {
        let mut pushed = 0usize;
        for s in stmts {
            self.check_stmt(s);
            if let Stmt::If {
                cond,
                then,
                els: None,
            } = s
            {
                if always_diverges(then) {
                    let (_, els) = self.narrow_from(cond);
                    if !els.is_empty() {
                        self.narrows.push(els);
                        pushed += 1;
                    }
                }
            }
        }
        for _ in 0..pushed {
            self.narrows.pop();
        }
    }

    fn check_bounds(&mut self, tvs: &[TvId], args: &[Type], what: &str, pos: Pos) {
        let map: HashMap<TvId, Type> = tvs.iter().copied().zip(args.iter().cloned()).collect();
        for (tv, arg) in tvs.iter().zip(args) {
            let Some(bound) = self.tv_bounds.get(tv).cloned() else {
                continue;
            };
            // The bound may itself mention the parameters (F-bounded).
            let bound = subst(&bound, &map);
            if !self.assignable(arg, &bound) {
                self.error(
                    Code::TypeMismatch,
                    format!(
                        "`{}` does not satisfy the bound `{}` on {what}",
                        self.show(arg),
                        self.show(&bound)
                    ),
                    pos,
                );
            }
        }
    }

    // ---- module & statements ------------------------------------------------------

    fn check_module(&mut self, module: &Module) {
        // Module vars first (types available to bodies), then bodies.
        for item in &module.items {
            match item {
                Item::Stmt(Stmt::Var(v))
                | Item::Export(ExportDecl {
                    kind: ExportKind::Var(v),
                    ..
                }) => self.check_var(v),
                Item::Stmt(_) => {}
                _ => {}
            }
        }
        for item in &module.items {
            match item {
                Item::Stmt(Stmt::Var(_)) => {}
                Item::Stmt(s) => {
                    self.check_stmt(s);
                }
                Item::Decl(d)
                | Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => self.check_decl_body(d),
                _ => {}
            }
        }
    }

    fn check_decl_body(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => {
                let Some(VarInfo {
                    ty: Type::Fn(sig), ..
                }) = self.lookup(&f.name.text)
                else {
                    return;
                };
                self.check_fn_body_async(&f.type_params, &f.params, &sig, &f.body, f.is_async);
            }
            Decl::Class(c) => self.check_class_body(c),
            Decl::Enum(e) => {
                for (_, init) in &e.members {
                    if let Some(init) = init {
                        let t = self.check_expr(init, None);
                        if !matches!(t, Type::Int(_) | Type::Err) {
                            self.error(
                                Code::TypeMismatch,
                                "enum member values must be integers",
                                e.name.pos,
                            );
                        }
                    }
                }
            }
            Decl::Interface(_) | Decl::TypeAlias(_) => {}
        }
    }

    /// Bind type params + params, then check the body against `sig`.
    /// Check a body against `sig`. For async functions the *body* returns
    /// `T` while the signature says `Promise<T>` — unwrap for the check.
    fn check_fn_body_async(
        &mut self,
        tps: &[TypeParam],
        params: &[Param],
        sig: &FnType,
        body: &[Stmt],
        is_async: bool,
    ) {
        if is_async {
            if let Some(inner) = self.unwrap_promise(&sig.ret) {
                let unwrapped = FnType {
                    tparams: sig.tparams.clone(),
                    params: sig.params.clone(),
                    ret: inner,
                };
                return self.check_fn_body(tps, params, &unwrapped, body);
            }
        }
        // A generator's body yields elements; its `return` (if any) is bare.
        if body_yields(body) {
            if let Some(elem) = self.unwrap_async_iter(&sig.ret) {
                let saved = self.yield_ty.replace(elem);
                let unwrapped = FnType {
                    tparams: sig.tparams.clone(),
                    params: sig.params.clone(),
                    ret: Type::Void,
                };
                self.check_fn_body(tps, params, &unwrapped, body);
                self.yield_ty = saved;
                return;
            }
            if let Some(elem) = self.unwrap_iter(&sig.ret) {
                let saved = self.yield_ty.replace(elem);
                let unwrapped = FnType {
                    tparams: sig.tparams.clone(),
                    params: sig.params.clone(),
                    ret: Type::Void,
                };
                self.check_fn_body(tps, params, &unwrapped, body);
                self.yield_ty = saved;
                return;
            }
        }
        self.check_fn_body(tps, params, sig, body)
    }

    fn check_fn_body(&mut self, tps: &[TypeParam], params: &[Param], sig: &FnType, body: &[Stmt]) {
        let mut scope = HashMap::new();
        for (tp, tv) in tps.iter().zip(&sig.tparams) {
            scope.insert(tp.name.text.clone(), *tv);
        }
        self.tp_scopes.push(scope);
        self.push_scope();
        for (p, pt) in params.iter().zip(&sig.params) {
            let ty = if p.rest {
                Type::Array(Rc::new(pt.ty.clone()))
            } else {
                pt.ty.clone()
            };
            self.bind_pattern_ty(&p.target, &ty, false);
            if let Some(d) = &p.default {
                let dt = self.check_expr(d, Some(&pt.ty));
                self.require_assignable_at(d, &dt, &pt.ty, "default value");
            }
        }
        let saved_ret = self.ret_ty.replace(sig.ret.clone());
        self.check_stmts_flowing(body);
        self.ret_ty = saved_ret;
        self.pop_scope();
        self.tp_scopes.pop();
    }

    fn class_self_type(&self, id: ClassId) -> Type {
        let args: Vec<Type> = self.classes[id]
            .tparams
            .iter()
            .map(|tv| Type::Var(*tv))
            .collect();
        Type::Class(id, Rc::new(args))
    }

    fn check_class_body(&mut self, c: &ClassDecl) {
        let TypeDef::Class(id) = self.type_defs[&c.name.text] else {
            return;
        };
        let mut scope = HashMap::new();
        let tvs = self.classes[id].tparams.clone();
        for (tp, tv) in c.type_params.iter().zip(&tvs) {
            scope.insert(tp.name.text.clone(), *tv);
        }
        self.tp_scopes.push(scope);
        let saved_class = self.current_class.replace(id);

        self.check_overrides(id, c.name.pos);
        self.check_implements(id, c.name.pos);

        let self_ty = self.class_self_type(id);
        for m in &c.members {
            match m {
                ClassMember::Field {
                    name,
                    init,
                    mods,
                    ty,
                    ..
                } => {
                    if init.is_none() && !mods.is_static {
                        // No initializer: instances are born with the type's zero.
                        let t = self.field_info(id, name).map(|f| f.0).unwrap_or(Type::Err);
                        self.note_default(ty, &t);
                    }
                    if init.is_none() && mods.is_static {
                        let t = self.resolve_type(ty);
                        self.note_default(ty, &t);
                    }
                    if let Some(init) = init {
                        let want = self.field_info(id, name).map(|f| f.0).unwrap_or(Type::Err);
                        self.push_scope();
                        self.in_static = mods.is_static;
                        if !mods.is_static {
                            self.define("this", self_ty.clone(), true);
                        }
                        let t = self.check_expr(init, Some(&want));
                        self.require_assignable_at(init, &t, &want, "field initializer");
                        self.in_static = false;
                        self.pop_scope();
                    }
                }
                ClassMember::Method {
                    name,
                    type_params,
                    params,
                    body,
                    mods,
                    is_async,
                    ..
                } => {
                    if let Some(body) = body {
                        let sig = self
                            .method_sig(id, name, mods.is_static)
                            .expect("collected method");
                        self.push_scope();
                        self.in_static = mods.is_static;
                        if !mods.is_static {
                            self.define("this", self_ty.clone(), true);
                        }
                        self.check_fn_body_async(type_params, params, &sig, body, *is_async);
                        self.in_static = false;
                        self.pop_scope();
                    }
                }
                ClassMember::Getter { name, body, .. } => {
                    let ret = self.classes[id]
                        .getters
                        .iter()
                        .find(|g| g.name == *name)
                        .map(|g| g.ty.clone())
                        .unwrap_or(Type::Err);
                    let sig = FnType {
                        tparams: vec![],
                        params: vec![],
                        ret,
                    };
                    self.push_scope();
                    self.define("this", self_ty.clone(), true);
                    self.check_fn_body(&[], &[], &sig, body);
                    self.pop_scope();
                }
                ClassMember::Setter {
                    name, param, body, ..
                } => {
                    let pt = self.classes[id]
                        .setters
                        .iter()
                        .find(|s| s.name == *name)
                        .map(|s| s.ty.clone())
                        .unwrap_or(Type::Err);
                    let sig = FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: pt,
                            optional: false,
                            rest: false,
                        }],
                        ret: Type::Void,
                    };
                    self.push_scope();
                    self.define("this", self_ty.clone(), true);
                    self.check_fn_body(&[], std::slice::from_ref(param), &sig, body);
                    self.pop_scope();
                }
                ClassMember::Ctor { params, body, .. } => {
                    let ptys = self.classes[id]
                        .ctor
                        .clone()
                        .map(|(p, _)| p)
                        .unwrap_or_default();
                    let sig = FnType {
                        tparams: vec![],
                        params: ptys,
                        ret: Type::Void,
                    };
                    self.push_scope();
                    self.define("this", self_ty.clone(), true);
                    self.in_ctor = true;
                    self.check_fn_body(&[], params, &sig, body);
                    self.in_ctor = false;
                    self.pop_scope();
                }
            }
        }

        self.current_class = saved_class;
        self.tp_scopes.pop();
    }

    fn check_overrides(&mut self, id: ClassId, pos: Pos) {
        let Some((pid, pargs)) = self.classes[id].parent.clone() else {
            for m in &self.classes[id].methods {
                if m.has_override {
                    let msg = format!("`{}` is marked override but there is no base class", m.name);
                    let (name_pos, msg) = (pos, msg);
                    self.diags
                        .push(Diagnostic::error(Code::BadOverride, msg, name_pos));
                }
            }
            return;
        };
        let map = self.subst_map(pid, &pargs);
        let method_list: Vec<(String, bool, bool)> = self.classes[id]
            .methods
            .iter()
            .map(|m| (m.name.clone(), m.has_override, m.is_static))
            .collect();
        for (name, has_override, is_static) in method_list {
            if is_static {
                continue;
            }
            let base = self.find_method_in_chain(pid, &name);
            match (base, has_override) {
                (Some((bm_final, base_sig)), true) => {
                    if bm_final {
                        self.error(
                            Code::BadOverride,
                            format!("cannot override final method `{name}`"),
                            pos,
                        );
                    }
                    // The override must be usable wherever the base is:
                    // parameters contravariant, return type covariant.
                    let Some(own) = self.classes[id]
                        .methods
                        .iter()
                        .find(|m| m.name == name && !m.is_static)
                        .map(|m| m.sig.clone())
                    else {
                        continue;
                    };
                    let base_sig = FnType {
                        tparams: base_sig.tparams.clone(),
                        params: base_sig
                            .params
                            .iter()
                            .map(|p| ParamType {
                                ty: subst(&p.ty, &map),
                                ..p.clone()
                            })
                            .collect(),
                        ret: subst(&base_sig.ret, &map),
                    };
                    if own.params.len() != base_sig.params.len() {
                        self.error(
                            Code::BadOverride,
                            format!(
                                "`{name}` overrides a method taking {} parameter(s), but takes {}",
                                base_sig.params.len(),
                                own.params.len()
                            ),
                            pos,
                        );
                    } else {
                        for (i, (o, b)) in own.params.iter().zip(base_sig.params.iter()).enumerate()
                        {
                            // Contravariance: the override must accept at
                            // least what the base accepted.
                            if !self.assignable(&b.ty, &o.ty) {
                                self.error(
                                    Code::BadOverride,
                                    format!(
                                        "`{name}`: parameter {} is `{}`, but the base accepts `{}`",
                                        i + 1,
                                        self.show(&o.ty),
                                        self.show(&b.ty)
                                    ),
                                    pos,
                                );
                            }
                        }
                        // Covariance: the override's result must be usable as
                        // the base's.
                        if !matches!(base_sig.ret, Type::Void)
                            && !self.assignable(&own.ret, &base_sig.ret)
                        {
                            self.error(
                                Code::BadOverride,
                                format!(
                                    "`{name}` returns `{}`, which is not a `{}`",
                                    self.show(&own.ret),
                                    self.show(&base_sig.ret)
                                ),
                                pos,
                            );
                        }
                    }
                }
                (Some(_), false) => self.error(
                    Code::BadOverride,
                    format!("method `{name}` shadows a base method; add `override`"),
                    pos,
                ),
                (None, true) => self.error(
                    Code::BadOverride,
                    format!("`{name}` is marked override but no base method exists"),
                    pos,
                ),
                (None, false) => {}
            }
        }
        // Concrete classes must not leave abstract methods unimplemented.
        if !self.classes[id].is_abstract {
            let mut cur = Some((pid, pargs));
            while let Some((cid, _)) = cur {
                let abstracts: Vec<String> = self.classes[cid]
                    .methods
                    .iter()
                    .filter(|m| m.is_abstract)
                    .map(|m| m.name.clone())
                    .collect();
                for name in abstracts {
                    if self.method_sig(id, &name, false).map(|s| s.ret).is_none()
                        || self.classes[id]
                            .methods
                            .iter()
                            .all(|m| m.name != name || m.is_abstract)
                    {
                        // implemented somewhere between? walk own chain sans abstract
                        let implemented = self.chain_has_concrete(id, &name);
                        if !implemented {
                            self.error(
                                Code::BadOverride,
                                format!(
                                    "class `{}` must implement abstract method `{name}`",
                                    self.classes[id].name
                                ),
                                pos,
                            );
                        }
                    }
                }
                cur = self.classes[cid].parent.clone();
            }
        }
    }

    fn chain_has_concrete(&self, id: ClassId, name: &str) -> bool {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(m) = self.classes[cid].methods.iter().find(|m| m.name == name) {
                return !m.is_abstract;
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        false
    }

    /// Everything you can write after a `.` on this type. Only public members:
    /// completion must not suggest what the checker will then reject (§4.2).
    pub(crate) fn members_of(&mut self, t: &Type) -> Vec<Completion> {
        let mut out: Vec<Completion> = Vec::new();
        match strip_null(t) {
            Type::Class(id, _) | Type::ClassMeta(id) => {
                let statics = matches!(strip_null(t), Type::ClassMeta(_));
                let mut cur = Some(id);
                let mut hosts: Vec<Type> = Vec::new();
                while let Some(id) = cur {
                    let ci = &self.classes[id];
                    let mut found: Vec<Completion> = Vec::new();
                    for f in &ci.fields {
                        if f.access == Access::Public && f.is_static == statics {
                            found.push(Completion {
                                label: f.name.clone(),
                                detail: self.show(&f.ty),
                                kind: KIND_FIELD,
                            });
                        }
                    }
                    for m in &ci.methods {
                        if m.access == Access::Public && m.is_static == statics {
                            found.push(Completion {
                                label: m.name.clone(),
                                detail: self.show(&Type::Fn(Rc::new(m.sig.clone()))),
                                kind: KIND_METHOD,
                            });
                        }
                    }
                    if !statics {
                        for g in &ci.getters {
                            if g.access == Access::Public {
                                found.push(Completion {
                                    label: g.name.clone(),
                                    detail: self.show(&g.ty),
                                    kind: KIND_FIELD,
                                });
                            }
                        }
                    }
                    // A host-backed class (`extends HTMLElement`) also offers
                    // everything the host interface does.
                    if let Some((iface, args)) = &ci.host_parent {
                        hosts.push(Type::Iface(*iface, Rc::new(args.to_vec())));
                    }
                    cur = ci.parent.as_ref().map(|(p, _)| *p);
                    add_all(&mut out, found);
                }
                for h in hosts {
                    let more = self.members_of(&h);
                    add_all(&mut out, more);
                }
            }
            Type::Iface(id, _) => {
                let mut stack = vec![id];
                while let Some(id) = stack.pop() {
                    let ii = &self.ifaces[id];
                    let found: Vec<Completion> = ii
                        .members
                        .iter()
                        .map(|m| Completion {
                            label: m.name.clone(),
                            detail: self.show(&m.ty),
                            kind: if matches!(m.ty, Type::Fn(_)) {
                                KIND_METHOD
                            } else {
                                KIND_FIELD
                            },
                        })
                        .collect();
                    for (parent, _) in &self.ifaces[id].extends {
                        stack.push(*parent);
                    }
                    add_all(&mut out, found);
                }
            }
            Type::Record(fields) => {
                let found: Vec<Completion> = fields
                    .iter()
                    .map(|f| Completion {
                        label: f.name.clone(),
                        detail: self.show(&f.ty),
                        kind: KIND_FIELD,
                    })
                    .collect();
                add_all(&mut out, found);
            }
            Type::EnumMeta(id) => {
                let name = self.enums[id].name.clone();
                let found: Vec<Completion> = self.enums[id]
                    .members
                    .clone()
                    .into_iter()
                    .map(|m| Completion {
                        label: m,
                        detail: name.clone(),
                        kind: KIND_FIELD,
                    })
                    .collect();
                add_all(&mut out, found);
            }
            // A namespace's members are listed once (`namespace_members`) and
            // *typed* by the checker, so documentation, completion and checking
            // cannot disagree about what exists.
            Type::Namespace(ns) => {
                for name in namespace_members(ns) {
                    if let Some(ty) = self.member_type_quiet(&Type::Namespace(ns), name) {
                        let kind = if matches!(ty, Type::Fn(_)) {
                            KIND_METHOD
                        } else {
                            KIND_FIELD
                        };
                        let detail = self.show(&ty);
                        add_all(
                            &mut out,
                            vec![Completion {
                                label: (*name).to_string(),
                                detail,
                                kind,
                            }],
                        );
                    }
                }
            }
            // Arrays, strings, maps, sets: rather than keep a second copy of
            // what they offer, ask the checker the same question it asks itself
            // — so completion cannot suggest a member that checking would then
            // reject.
            other => {
                for name in BUILTIN_MEMBERS {
                    if let Some(ty) = self.member_type_quiet(&other, name) {
                        let kind = if matches!(ty, Type::Fn(_)) {
                            KIND_METHOD
                        } else {
                            KIND_FIELD
                        };
                        let detail = self.show(&ty);
                        add_all(
                            &mut out,
                            vec![Completion {
                                label: name.to_string(),
                                detail,
                                kind,
                            }],
                        );
                    }
                }
            }
        }
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out
    }

    fn find_method_in_chain(&self, id: ClassId, name: &str) -> Option<(bool, FnType)> {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(m) = self.classes[cid].methods.iter().find(|m| m.name == name) {
                return Some((m.is_final, m.sig.clone()));
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        None
    }

    fn check_implements(&mut self, id: ClassId, pos: Pos) {
        let ifaces = self.classes[id].ifaces.clone();
        for (iid, args) in ifaces {
            let imap: HashMap<TvId, Type> = self.ifaces[iid]
                .tparams
                .iter()
                .copied()
                .zip(args.iter().cloned())
                .collect();
            let members: Vec<(String, Type, bool)> = self.ifaces[iid]
                .members
                .iter()
                .map(|m| (m.name.clone(), subst(&m.ty, &imap), m.optional))
                .collect();
            for (name, want, optional) in members {
                // What the class actually provides — and its *type*, not just
                // whether the name exists. An interface that only checks names is
                // barely an interface: `implements Iterable<int32>` with an
                // `iter(): Iter<string>` used to compile, and then `for … of`
                // would hand you strings where you asked for numbers.
                let got: Option<Type> = match &want {
                    Type::Fn(_) => self.method_sig(id, &name, false).map(|sig| {
                        let map = self.subst_map(id, &[]);
                        Type::Fn(Rc::new(FnType {
                            tparams: sig.tparams.clone(),
                            params: sig
                                .params
                                .iter()
                                .map(|p| ParamType {
                                    ty: subst(&p.ty, &map),
                                    ..p.clone()
                                })
                                .collect(),
                            ret: subst(&sig.ret, &map),
                        }))
                    }),
                    _ => self.field_info(id, &name).map(|(t, ..)| t).or_else(|| {
                        self.classes[id]
                            .getters
                            .iter()
                            .find(|g| g.name == name)
                            .map(|g| g.ty.clone())
                    }),
                };
                match got {
                    None if !optional => {
                        self.error(
                            Code::BadOverride,
                            format!(
                                "class `{}` is missing `{name}` required by interface `{}`",
                                self.classes[id].name, self.ifaces[iid].name
                            ),
                            pos,
                        );
                    }
                    Some(have) if !self.assignable(&have, &want) => {
                        self.error(
                            Code::BadOverride,
                            format!(
                                "`{name}` is `{}` on class `{}`, but interface `{}` requires `{}`",
                                self.show(&have),
                                self.classes[id].name,
                                self.ifaces[iid].name,
                                self.show(&want)
                            ),
                            pos,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// Remember what a local holds, if it holds a number.
    fn note_local(&mut self, p: &Pattern, ty: &Type) {
        if let (Pattern::Name(n), Some(k)) = (p, num_of(ty)) {
            self.local_types.insert(n as *const Name as usize, k);
        }
    }

    /// The zero-default of a resolved type, if it has one.
    ///
    /// A type alias is already gone by the time this runs — `type Meters =
    /// float64` arrives here as `F64` — which is why the *checker* answers this
    /// question and the engine only reads the answer.
    fn default_of(&self, t: &Type) -> Option<DefaultVal> {
        Some(match t {
            Type::Int(k) => DefaultVal::Num(Num::Int(*k)),
            Type::F32 => DefaultVal::Num(Num::F32),
            Type::F64 => DefaultVal::Num(Num::F64),
            Type::BigInt => DefaultVal::BigInt,
            Type::BigDec => DefaultVal::BigDec,
            Type::Str => DefaultVal::Str,
            Type::Char => DefaultVal::Char,
            Type::Bool => DefaultVal::Bool,
            Type::Array(_) => DefaultVal::Array,
            // Map, Set and bytes are classes to the type system, but they are
            // *containers* to the language, and a container defaults to empty.
            Type::Class(id, _) => match self.classes.get(*id).map(|c| c.name.as_str()) {
                Some("Map") => DefaultVal::Map,
                Some("Set") => DefaultVal::Set,
                Some("bytes") => DefaultVal::Bytes,
                // A user class has no constructible default: `null`, until
                // definite assignment exists to forbid the situation entirely.
                _ => return None,
            },
            // `T?` defaults to `null`, which for a nullable type is not a lie.
            _ => return None,
        })
    }

    /// A binding or field was declared with `ty` and given no initializer:
    /// record what it starts as.
    fn note_default(&mut self, ty: &TypeExpr, t: &Type) {
        if let Some(d) = self.default_of(t) {
            self.defaults.insert(ty as *const TypeExpr as usize, d);
        }
    }

    fn check_var(&mut self, v: &VarStmt) {
        for b in &v.bindings {
            let declared = b.ty.as_ref().map(|t| self.resolve_type(t));
            let init_ty = b
                .init
                .as_ref()
                .map(|e| self.check_expr(e, declared.as_ref()));
            let ty = match (&declared, init_ty) {
                (Some(d), Some(i)) => {
                    self.require_assignable_at(b.init.as_ref().unwrap(), &i, d, "initializer");
                    d.clone()
                }
                (Some(d), None) => {
                    // No initializer: the binding starts at its type's zero.
                    self.note_default(b.ty.as_ref().expect("declared"), d);
                    d.clone()
                }
                (None, Some(i)) => {
                    if matches!(i, Type::Null) {
                        self.error(
                            Code::TypeMismatch,
                            "cannot infer a type from `null`; add an annotation",
                            pattern_pos(&b.target),
                        );
                        Type::Err
                    } else {
                        i
                    }
                }
                (None, None) => {
                    self.error(
                        Code::TypeMismatch,
                        "binding needs a type annotation or an initializer",
                        pattern_pos(&b.target),
                    );
                    Type::Err
                }
            };
            self.note_local(&b.target, &ty);
            self.bind_pattern_ty(&b.target, &ty, v.kind == VarKind::Const);
        }
    }

    fn bind_pattern_ty(&mut self, p: &Pattern, ty: &Type, is_const: bool) {
        match p {
            Pattern::Name(n) => {
                self.define_at(&n.text, ty.clone(), is_const, n.pos);
            }
            Pattern::Array { elems, rest } => {
                let elem = match strip_null(ty) {
                    Type::Array(e) => e.as_ref().clone(),
                    Type::Str => Type::Char,
                    Type::Tuple(_) | Type::Err => Type::Err, // tuples positional below
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!("cannot destructure `{}` as an array", self.show(&other)),
                            pattern_pos(p),
                        );
                        Type::Err
                    }
                };
                for (i, e) in elems.iter().enumerate() {
                    let et = match strip_null(ty) {
                        Type::Tuple(ts) => ts.get(i).cloned().unwrap_or(Type::Err),
                        _ => elem.clone(),
                    };
                    // A default value removes nullability.
                    let et = if e.default.is_some() {
                        strip_null(&et)
                    } else {
                        et
                    };
                    self.bind_pattern_ty(&e.target, &et, is_const);
                }
                if let Some(r) = rest {
                    self.bind_pattern_ty(r, &Type::Array(Rc::new(elem)), is_const);
                }
            }
            Pattern::Record(fields) => {
                for f in fields {
                    let ft = self
                        .member_type_quiet(&strip_null(ty), &f.name.text)
                        .unwrap_or(Type::Err);
                    let ft = if f.default.is_some() {
                        strip_null(&ft)
                    } else {
                        ft
                    };
                    match &f.target {
                        Some(t) => self.bind_pattern_ty(t, &ft, is_const),
                        None => self.define_at(&f.name.text, ft, is_const, f.name.pos),
                    }
                }
            }
        }
    }

    fn check_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block(b) => {
                self.push_scope();
                self.check_stmts_flowing(b);
                self.pop_scope();
            }
            Stmt::Var(v) => self.check_var(v),
            Stmt::Expr(e) => {
                self.check_expr(e, None);
            }
            Stmt::Empty => {}
            Stmt::If { cond, then, els } => {
                self.check_condition(cond);
                let (then_narrow, else_narrow) = self.narrow_from(cond);
                self.narrows.push(then_narrow);
                self.check_stmt(then);
                self.narrows.pop();
                if let Some(e) = els {
                    self.narrows.push(else_narrow);
                    self.check_stmt(e);
                    self.narrows.pop();
                }
            }
            Stmt::While { cond, body } => {
                self.check_condition(cond);
                let (narrow, _) = self.narrow_from(cond);
                self.narrows.push(narrow);
                self.check_stmt(body);
                self.narrows.pop();
            }
            Stmt::DoWhile { body, cond } => {
                self.check_stmt(body);
                self.check_condition(cond);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                match init {
                    Some(ForInit::Var(v)) => self.check_var(v),
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.check_expr(e, None);
                        }
                    }
                    None => {}
                }
                if let Some(c) = cond {
                    self.check_condition(c);
                }
                for e in step {
                    self.check_expr(e, None);
                }
                self.check_stmt(body);
                self.pop_scope();
            }
            Stmt::ForOf {
                is_await,
                kind,
                target,
                ty,
                iter,
                body,
            } => {
                self.push_scope();
                let it = self.check_expr(iter, None);
                if *is_await {
                    // `for await (const x of gen())` consumes an AsyncIter<T>,
                    // awaiting each `next()`. It is the loop form of `await`,
                    // and lives wherever `await` does.
                    let elem = match self.unwrap_async_iter(&it) {
                        Some(e) => e,
                        None => match strip_null(&it) {
                            Type::Err => Type::Err,
                            // A class that declared `implements AsyncIterable<T>`.
                            Type::Class(id, ref args)
                                if self
                                    .async_iterable_id
                                    .and_then(|i| self.implemented_args(id, i))
                                    .is_some() =>
                            {
                                let iface = self.async_iterable_id.expect("checked");
                                let iargs = self.implemented_args(id, iface).expect("checked");
                                let map = self.subst_map(id, args);
                                iargs
                                    .first()
                                    .map(|t| subst(t, &map))
                                    .unwrap_or(Type::Unknown)
                            }
                            other => {
                                self.error(
                                    Code::BadOperand,
                                    format!(
                                        "`for await` needs an `AsyncIter<T>` (an `async` function that \
                                         yields) or a class implementing `AsyncIterable<T>`, \
                                         found `{}`",
                                        self.show(&other)
                                    ),
                                    pos_of(iter),
                                );
                                Type::Err
                            }
                        },
                    };
                    let declared = ty.as_ref().map(|t| self.resolve_type(t));
                    let bound = declared.unwrap_or(elem);
                    self.bind_pattern_ty(target, &bound, *kind == VarKind::Const);
                    self.check_stmt(body);
                    self.pop_scope();
                    return;
                }
                let elem = match strip_null(&it) {
                    Type::Array(e) => e.as_ref().clone(),
                    Type::Str => Type::Char,
                    Type::Err => Type::Err,
                    // Host iterables (NodeList, HTMLCollection, …): the IDL
                    // element type isn't tracked, so the element is `unknown`
                    // until it is narrowed (`as Element`).
                    Type::Iface(..) => Type::Unknown,
                    // A generator / iterator.
                    ref t if self.unwrap_iter(t).is_some() => self.unwrap_iter(t).expect("checked"),
                    // A class that declared `implements Iterable<T>`. This is
                    // what JS spells `Symbol.iterator` — as an interface, so the
                    // checker can see it.
                    Type::Class(id, args)
                        if self
                            .iterable_id
                            .and_then(|i| self.implemented_args(id, i))
                            .is_some() =>
                    {
                        let iface = self.iterable_id.expect("checked");
                        let iargs = self.implemented_args(id, iface).expect("checked");
                        let map = self.subst_map(id, &args);
                        iargs
                            .first()
                            .map(|t| subst(t, &map))
                            .unwrap_or(Type::Unknown)
                    }
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!(
                                "`for of` needs an array, string, an iterator, or a class that \
                                 implements `Iterable<T>`, got `{}`",
                                self.show(&other)
                            ),
                            pos_of(iter),
                        );
                        Type::Err
                    }
                };
                let elem = match ty {
                    Some(t) => {
                        let want = self.resolve_type(t);
                        self.require_assignable(&elem, &want, pos_of(iter), "loop binding");
                        want
                    }
                    None => elem,
                };
                self.bind_pattern_ty(target, &elem, *kind == VarKind::Const);
                self.check_stmt(body);
                self.pop_scope();
            }
            Stmt::Switch { scrutinee, clauses } => {
                let st = self.check_expr(scrutinee, None);
                self.push_scope();
                for c in clauses {
                    if let Some(t) = &c.test {
                        let tt = self.check_expr(t, Some(&st));
                        if !self.comparable(&st, &tt) {
                            self.error(
                                Code::TypeMismatch,
                                format!(
                                    "case type `{}` does not match switch type `{}`",
                                    self.show(&tt),
                                    self.show(&st)
                                ),
                                pos_of(t),
                            );
                        }
                    }
                    for s in &c.body {
                        self.check_stmt(s);
                    }
                }
                self.pop_scope();
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Return { value, pos } => {
                let want = self.ret_ty.clone().unwrap_or(Type::Void);
                match value {
                    Some(e) => {
                        let t = self.check_expr(e, Some(&want));
                        if matches!(want, Type::Void) {
                            self.error(
                                Code::BadReturn,
                                "this function returns `void`; remove the value",
                                *pos,
                            );
                        } else {
                            self.require_assignable_at(e, &t, &want, "return value");
                        }
                    }
                    None => {
                        if !matches!(want, Type::Void | Type::Err) {
                            self.error(
                                Code::BadReturn,
                                format!("expected a `{}` return value", self.show(&want)),
                                *pos,
                            );
                        }
                    }
                }
            }
            Stmt::Throw(e) => {
                let t = self.check_expr(e, None);
                if !self.is_error_class(&t) {
                    self.error(
                        Code::BadThrow,
                        format!(
                            "only `Error` subclasses may be thrown, got `{}`",
                            self.show(&t)
                        ),
                        pos_of(e),
                    );
                }
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                self.push_scope();
                for s in block {
                    self.check_stmt(s);
                }
                self.pop_scope();
                for c in catches {
                    let ct = self.resolve_type(&c.ty);
                    if !self.is_error_class(&ct) {
                        self.error(
                            Code::BadThrow,
                            format!(
                                "catch type must be an `Error` subclass, got `{}`",
                                self.show(&ct)
                            ),
                            c.name.pos,
                        );
                    }
                    self.push_scope();
                    self.define_at(&c.name.text, ct, false, c.name.pos);
                    for s in &c.block {
                        self.check_stmt(s);
                    }
                    self.pop_scope();
                }
                if let Some(f) = finally {
                    self.push_scope();
                    for s in f {
                        self.check_stmt(s);
                    }
                    self.pop_scope();
                }
            }
            Stmt::Labeled { body, .. } => self.check_stmt(body),
        }
    }

    fn is_error_class(&self, t: &Type) -> bool {
        match t {
            Type::Class(id, _) => {
                let mut cur = Some(*id);
                while let Some(c) = cur {
                    if c == self.error_id {
                        return true;
                    }
                    cur = self.classes[c].parent.as_ref().map(|(p, _)| *p);
                }
                false
            }
            Type::Err => true,
            _ => false,
        }
    }

    fn check_condition(&mut self, e: &Expr) {
        let t = self.check_expr(e, None);
        match strip_narrow_helpers(&t) {
            Type::Bool | Type::Int(_) | Type::F32 | Type::F64 | Type::Err => {}
            other => self.error(
                Code::BadCondition,
                format!(
                    "condition must be bool or numeric, got `{}`; write the comparison (§3.3)",
                    self.show(&other)
                ),
                pos_of(e),
            ),
        }
    }

    /// A narrowable access path (`x`, `x.y`, `this.z`), or None.
    fn narrow_path(e: &Expr) -> Option<String> {
        match e {
            Expr::Ident(n) => Some(n.text.clone()),
            Expr::This(_) => Some("this".to_string()),
            Expr::Member {
                obj,
                name,
                optional: false,
            } => {
                let base = Self::narrow_path(obj)?;
                Some(format!("{base}.{name}"))
            }
            Expr::Paren(inner) => Self::narrow_path(inner),
            _ => None,
        }
    }

    /// `(then, else)` narrowing maps from a condition.
    fn narrow_from(&mut self, cond: &Expr) -> (HashMap<String, Type>, HashMap<String, Type>) {
        let mut then = HashMap::new();
        let mut els = HashMap::new();
        if let Expr::Paren(inner) = cond {
            return self.narrow_from(inner);
        }
        // `a && b`: the then-branch gets both narrowings (b is checked with
        // a's already applied). `a || b`: the else-branch gets both.
        if let Expr::Binary {
            op: op @ (BinOp::And | BinOp::Or),
            l,
            r,
        } = cond
        {
            let (lt, le) = self.narrow_from(l);
            let carry = if *op == BinOp::And {
                lt.clone()
            } else {
                le.clone()
            };
            self.narrows.push(carry);
            let (rt, re) = self.narrow_from(r);
            self.narrows.pop();
            if *op == BinOp::And {
                then.extend(lt);
                then.extend(rt);
            } else {
                els.extend(le);
                els.extend(re);
            }
            return (then, els);
        }
        // `if (x is int32)` narrows x to int32 in the then-branch. This is what
        // makes `unknown` usable without exceptions: you can *ask* whether a
        // value is what you hope, instead of casting and catching when it is not.
        if let Expr::Is { expr, ty } = cond {
            if let Expr::Ident(n) = expr.as_ref() {
                let t = self.resolve_type(ty);
                if self.testable(&t) {
                    then.insert(n.text.clone(), t.clone());
                    // And in the else-branch, a union loses that arm.
                    let from = self.check_expr_quiet(expr);
                    if let Type::Union(arms) = strip_null(&from) {
                        let rest: Vec<Type> = arms
                            .iter()
                            .filter(|a| !self.ty_eq(a, &t))
                            .cloned()
                            .collect();
                        if rest.len() == 1 {
                            els.insert(n.text.clone(), rest[0].clone());
                        } else if rest.len() > 1 {
                            els.insert(n.text.clone(), Type::Union(Rc::new(rest)));
                        }
                    }
                }
            }
            return (then, els);
        }
        // `if (x instanceof Foo)` narrows x to Foo in the then-branch.
        if let Expr::Binary {
            op: BinOp::Instanceof,
            l,
            r,
        } = cond
        {
            if let Expr::Ident(n) = l.as_ref() {
                let narrowed = match self.check_expr(r, None) {
                    Type::ClassMeta(id) => {
                        let args: Vec<Type> = self.classes[id]
                            .tparams
                            .iter()
                            .map(|_| Type::Unknown)
                            .collect();
                        Some(Type::Class(id, Rc::new(args)))
                    }
                    Type::IfaceMeta(id) => {
                        let args: Vec<Type> = self.ifaces[id]
                            .tparams
                            .iter()
                            .map(|_| Type::Unknown)
                            .collect();
                        Some(Type::Iface(id, Rc::new(args)))
                    }
                    _ => None,
                };
                if let Some(t) = narrowed {
                    then.insert(n.text.clone(), t);
                }
            }
            return (then, els);
        }
        if let Expr::Binary { op, l, r } = cond {
            let (path_expr, other) = match (l.as_ref(), r.as_ref()) {
                (
                    e,
                    Expr::Lit {
                        kind: LitKind::Null,
                        ..
                    },
                ) => (Some(e), r.as_ref()),
                (
                    Expr::Lit {
                        kind: LitKind::Null,
                        ..
                    },
                    e,
                ) => (Some(e), l.as_ref()),
                _ => (None, l.as_ref()),
            };
            let _ = other;
            if let Some(e) = path_expr {
                // Any access path narrows, not just a bare identifier:
                // `if (box.item != null) { box.item.length }`.
                if let Some(path) = Self::narrow_path(e) {
                    let ty = self.path_type(&path, e);
                    if let Type::Nullable(inner) = ty {
                        match op {
                            BinOp::Ne => {
                                then.insert(path.clone(), inner.as_ref().clone());
                                els.insert(path, Type::Null);
                            }
                            BinOp::Eq => {
                                els.insert(path.clone(), inner.as_ref().clone());
                                then.insert(path, Type::Null);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        (then, els)
    }

    /// Current (possibly narrowed) type of an access path.
    fn path_type(&mut self, path: &str, e: &Expr) -> Type {
        for n in self.narrows.iter().rev() {
            if let Some(t) = n.get(path) {
                return t.clone();
            }
        }
        self.check_expr_quiet(e)
    }

    /// TypeExpr an expression without emitting diagnostics (narrowing probes).
    fn check_expr_quiet(&mut self, e: &Expr) -> Type {
        let n = self.diags.len();
        let t = self.check_expr(e, None);
        self.diags.truncate(n);
        t
    }

    // ---- expressions ------------------------------------------------------------------

    fn check_expr(&mut self, e: &Expr, expected: Option<&Type>) -> Type {
        let t = self.check_expr_inner(e, expected);
        if self.index.is_some() {
            let shown = self.show(&t);
            let pos = pos_of(e);
            if let Some(ix) = &mut self.index {
                ix.types.push((pos, shown));
            }
            // An identifier use points back at where the name was declared.
            if let Expr::Ident(n) = e {
                if let Some(def) = self.lookup(&n.text).and_then(|v| v.def) {
                    if let Some(ix) = &mut self.index {
                        ix.uses.push((n.pos, def, n.text.clone()));
                    }
                }
            }
        }
        // Unsuffixed integer literals adapt to the expected integer type
        // when the value fits (spec §2.6); overflow is E0110, not a
        // mismatch.
        if let (Some(want), Type::Int(IntKind::I32)) = (expected, &t) {
            if let Type::Int(k) = strip_null(want) {
                if k != IntKind::I32 {
                    if let Some(v) = unsuffixed_int_value(e) {
                        return if int_fits(v, k) {
                            // The literal *is* a `uint32` — but the engine has
                            // no idea. It reads the digits and produces the
                            // default `int32`, which for `4294967295` is a range
                            // error for a value that fits the type it was given,
                            // and for `2147483647` in an `int64` is a number that
                            // wraps at the wrong width. Record the type it
                            // adapted to, so the literal is *built* as one.
                            self.coercions.insert(node_id(e), Num::Int(k));
                            Type::Int(k)
                        } else {
                            self.error(
                                Code::IntOutOfRange,
                                format!("literal `{v}` does not fit `{}`", k.name()),
                                pos_of(e),
                            );
                            Type::Int(k)
                        };
                    }
                }
            }
        }
        t
    }

    fn check_expr_inner(&mut self, e: &Expr, expected: Option<&Type>) -> Type {
        match e {
            Expr::Ident(n) => self.lookup(&n.text).map(|v| v.ty).unwrap_or(Type::Err),
            Expr::This(pos) => match self.current_class {
                Some(id) if !self.in_static => self.class_self_type(id),
                Some(_) => {
                    self.error(
                        Code::TypeMismatch,
                        "`this` is not available in a static member",
                        *pos,
                    );
                    Type::Err
                }
                None => Type::Err, // binder reported
            },
            Expr::Lit { kind, text, .. } => self.literal_ty(*kind, text),
            Expr::Template(parts) => {
                for p in parts {
                    if let TplPart::Expr(e) = p {
                        let t = self.check_expr(e, None);
                        if matches!(t, Type::Void) {
                            self.error(Code::TypeMismatch, "cannot interpolate `void`", pos_of(e));
                        }
                    }
                }
                Type::Str
            }
            Expr::Array(elems) => {
                // Tuple context: check positionally and produce the tuple.
                if let Some(Type::Tuple(ts)) = expected.map(strip_null) {
                    if ts.len() == elems.len() && elems.iter().all(|e| !e.spread) {
                        for (el, want) in elems.iter().zip(ts.iter()) {
                            let t = self.check_expr(&el.expr, Some(want));
                            self.require_assignable_at(&el.expr, &t, want, "tuple element");
                        }
                        return Type::Tuple(ts);
                    }
                }
                let want_elem = expected.map(strip_null).and_then(|t| match t {
                    Type::Array(e) => Some(e.as_ref().clone()),
                    _ => None,
                });
                let mut unified: Option<Type> = want_elem.clone();
                for el in elems {
                    let want = if el.spread {
                        want_elem.clone().map(|e| Type::Array(Rc::new(e)))
                    } else {
                        want_elem.clone()
                    };
                    let t = self.check_expr(&el.expr, want.as_ref());
                    let t = if el.spread {
                        match strip_null(&t) {
                            Type::Array(e) => e.as_ref().clone(),
                            Type::Err => Type::Err,
                            other => {
                                self.error(
                                    Code::TypeMismatch,
                                    format!("can only spread arrays, got `{}`", self.show(&other)),
                                    pos_of(&el.expr),
                                );
                                Type::Err
                            }
                        }
                    } else {
                        t
                    };
                    // `const xs: float64[] = [7]` — the element widens, and the
                    // array must actually hold the widened value. Unification
                    // alone would agree that the array is `float64[]` and leave
                    // an `int32` sitting in it.
                    if !el.spread {
                        if let Some(w) = &want_elem {
                            self.coerce(&el.expr, &t, w);
                        }
                    }
                    unified = Some(match unified {
                        None => t,
                        Some(u) => self.unify_pair(u, t),
                    });
                }
                Type::Array(Rc::new(unified.unwrap_or(Type::Err)))
            }
            Expr::Record(fields) => {
                let mut fs = Vec::new();
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            let want = expected
                                .map(strip_null)
                                .and_then(|t| self.member_type_quiet(&t, &name.text));
                            let t = match value {
                                Some(v) => self.check_expr(v, want.as_ref()),
                                None => self.lookup(&name.text).map(|v| v.ty).unwrap_or(Type::Err),
                            };
                            // `const r: { x: float64 } = { x: 7 }` — the field
                            // widens. The record's own type follows the value it
                            // will actually hold, so the two cannot disagree.
                            let t = match &want {
                                // Both numeric, and different: the field widens,
                                // and its type follows the value it will hold. A
                                // field that simply does not match stays as it is
                                // — retyping it here would hide the mismatch from
                                // the error message that is about to report it.
                                Some(w)
                                    if num_of(&t).is_some()
                                        && num_of(w).is_some()
                                        && num_of(w) != num_of(&t) =>
                                {
                                    match value {
                                        Some(v) => self.coerce(v, &t, w),
                                        // `{ x }` has no expression to hang the
                                        // conversion on; the field itself is the
                                        // thing being converted.
                                        None => {
                                            self.coerce_key(name as *const Name as usize, &t, w)
                                        }
                                    }
                                    w.clone()
                                }
                                _ => t,
                            };
                            fs.push(RecField {
                                name: name.text.clone(),
                                ty: t,
                                optional: false,
                            });
                        }
                        RecordField::Spread(v) => {
                            let t = self.check_expr(v, None);
                            if let Type::Record(inner) = strip_null(&t) {
                                for f in inner.iter() {
                                    fs.push(f.clone());
                                }
                            }
                        }
                    }
                }
                fs.sort_by(|a, b| a.name.cmp(&b.name));
                fs.dedup_by(|a, b| a.name == b.name);
                Type::Record(Rc::new(fs))
            }
            Expr::Paren(inner) => self.check_expr(inner, expected),
            Expr::Arrow {
                params,
                ret,
                body,
                is_async: is_async_arrow,
            } => {
                let ctx_fn = expected.map(strip_null).and_then(|t| match t {
                    Type::Fn(f) => Some(f.clone()),
                    _ => None,
                });
                self.push_scope();
                let mut ptys = Vec::new();
                for (i, p) in params.iter().enumerate() {
                    let ty = match &p.ty {
                        Some(t) => self.resolve_type(t),
                        None => ctx_fn
                            .as_ref()
                            .and_then(|f| f.params.get(i))
                            .map(|pt| pt.ty.clone())
                            .unwrap_or(Type::Err),
                    };
                    let bind_ty = if p.rest {
                        Type::Array(Rc::new(ty.clone()))
                    } else {
                        ty.clone()
                    };
                    self.bind_pattern_ty(&p.target, &bind_ty, false);
                    ptys.push(ParamType {
                        ty,
                        optional: p.optional,
                        rest: p.rest,
                    });
                }
                let declared_ret = ret.as_ref().map(|t| self.resolve_type(t));
                let want_ret = declared_ret
                    .clone()
                    .or_else(|| ctx_fn.as_ref().map(|f| f.ret.clone()));
                let actual_ret = match body {
                    ArrowBody::Expr(e) => self.check_expr(e, want_ret.as_ref()),
                    ArrowBody::Block(stmts) => {
                        let saved = self.ret_ty.replace(want_ret.clone().unwrap_or(Type::Err));
                        for s in stmts {
                            self.check_stmt(s);
                        }
                        self.ret_ty = saved;
                        want_ret.clone().unwrap_or(Type::Void)
                    }
                };
                self.pop_scope();
                // If the context only offers an unresolved type variable
                // (`map<U>(f: (T) => U)`), the body's own type is better —
                // otherwise inference would bind `U` to itself.
                let contextual = match &want_ret {
                    Some(Type::Var(_)) => None,
                    other => other.clone(),
                };
                let ret = declared_ret.or(contextual).unwrap_or(actual_ret);
                let ret = if *is_async_arrow && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
                } else {
                    ret
                };
                Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: ptys,
                    ret,
                }))
            }
            Expr::Unary { op, expr, pos } => {
                let t = self.check_expr(expr, None);
                match op {
                    UnaryOp::Not => {
                        match strip_narrow_helpers(&t) {
                            Type::Bool | Type::Int(_) | Type::F32 | Type::F64 | Type::Err => {}
                            other => self.error(
                                Code::BadOperand,
                                format!("`!` needs bool or numeric, got `{}`", self.show(&other)),
                                *pos,
                            ),
                        }
                        Type::Bool
                    }
                    UnaryOp::Plus | UnaryOp::Neg => match strip_narrow_helpers(&t) {
                        n
                        @ (Type::Int(_) | Type::F32 | Type::F64 | Type::BigInt | Type::BigDec) => n,
                        Type::Err => Type::Err,
                        other => {
                            self.error(
                                Code::BadOperand,
                                format!("unary needs a number, got `{}`", self.show(&other)),
                                *pos,
                            );
                            Type::Err
                        }
                    },
                    UnaryOp::BitNot => match strip_narrow_helpers(&t) {
                        n @ Type::Int(_) => n,
                        Type::Err => Type::Err,
                        other => {
                            self.error(
                                Code::BadOperand,
                                format!("`~` needs an integer, got `{}`", self.show(&other)),
                                *pos,
                            );
                            Type::Err
                        }
                    },
                    UnaryOp::Await => match self.unwrap_promise(&t) {
                        Some(inner) => inner,
                        None => match strip_narrow_helpers(&t) {
                            // A host object may be a JS thenable; a plain
                            // value awaits to itself (as in JS).
                            Type::Err => Type::Err,
                            other => other,
                        },
                    },
                }
            }
            Expr::Update { expr, .. } => {
                let t = self.check_expr(expr, None);
                self.check_assignable_target(expr);
                match strip_narrow_helpers(&t) {
                    n @ (Type::Int(_) | Type::F32 | Type::F64) => {
                        // `i++` is `i = i + 1`, and it is the hottest operator in
                        // the language — every counted loop runs one per
                        // iteration.
                        if let Some(k) = num_of(&n) {
                            self.op_types.insert(node_id(e), k);
                        }
                        n
                    }
                    Type::Err => Type::Err,
                    other => {
                        self.error(
                            Code::BadOperand,
                            format!("`++`/`--` need a number, got `{}`", self.show(&other)),
                            pos_of(expr),
                        );
                        Type::Err
                    }
                }
            }
            Expr::Binary { op, l, r } => self.check_binary(e, *op, l, r),
            Expr::Assign { op, target, value } => {
                // Assignment targets check against the *declared* type, not
                // a narrowed one (the assignment may widen back).
                let tt = match target.as_ref() {
                    Expr::Ident(n) => self
                        .lookup_scope(&n.text)
                        .map(|v| v.ty)
                        .unwrap_or(Type::Err),
                    _ => self.check_expr(target, None),
                };
                self.check_assignable_target(target);
                let vt = self.check_expr(value, Some(&tt));
                if *op == "=" {
                    self.require_assignable_at(value, &vt, &tt, "assignment");
                } else if !matches!(tt, Type::Err) {
                    // Compound assignment: the operation must be valid; the
                    // result converts back to the target type with wrapping,
                    // as in C (`a += 1` on an int16 stays int16).
                    let res = self.check_binary_types(compound_op(op), &tt, &vt, pos_of(value));
                    // The operator itself works in the common type; the *result*
                    // then converts back to the target's (rule 6, recorded below).
                    if let Some(common) = self.numeric_common(&tt, &vt) {
                        if let Some(k) = num_of(&common) {
                            self.op_types.insert(node_id(e), k);
                        }
                    }
                    let numeric = |t: &Type| matches!(t, Type::Int(_) | Type::F32 | Type::F64);
                    if numeric(&res) && numeric(&tt) {
                        // Rule 6 of §3.3: the operation happens in the common
                        // type and the result converts *back* to the target's,
                        // wrapping as C does — `int16 a; a += 1` stays an int16.
                        // Integer promotion means `res` is at least an int32, so
                        // without this the value silently outgrew its own slot:
                        // `a` held 32768, which an int16 cannot represent.
                        if let (Some(r), Some(t)) = (num_of(&res), num_of(&tt)) {
                            if r != t {
                                self.result_coercions.insert(node_id(e), t);
                            }
                        }
                    } else {
                        self.require_assignable(&res, &tt, pos_of(value), "compound assignment");
                    }
                }
                // Assigning through a path invalidates its narrowing (and
                // any narrowing of paths beneath it).
                if let Some(path) = Self::narrow_path(target) {
                    self.kill_narrow(&path);
                }
                tt
            }
            Expr::Cond { cond, then, els } => {
                self.check_condition(cond);
                let (tn, en) = self.narrow_from(cond);
                self.narrows.push(tn);
                let a = self.check_expr(then, expected);
                self.narrows.pop();
                self.narrows.push(en);
                let b = self.check_expr(els, expected);
                self.narrows.pop();
                self.unify_pair(a, b)
            }
            Expr::Cast { expr, wrapping, ty } => {
                let from = self.check_expr(expr, None);
                let to = self.resolve_type(ty);
                self.check_cast(&from, &to, *wrapping, pos_of(expr));
                to
            }
            Expr::Is { expr, ty } => {
                self.check_expr(expr, None);
                let to = self.resolve_type(ty);
                // The test happens on a *value*, so only types a value can be
                // checked against are allowed. A record type is structural — two
                // records with the same fields are the same type — so there is
                // nothing at run time to test it against, and pretending
                // otherwise would make `is` lie.
                if !self.testable(&to) {
                    self.error(
                        Code::BadOperand,
                        format!(
                            "`is` needs a type a value can be tested against (a primitive, a class, or an interface), not `{}`",
                            self.show(&to)
                        ),
                        pos_of(expr),
                    );
                }
                Type::Bool
            }
            Expr::Call {
                callee,
                type_args,
                args,
                optional,
            } => self.check_call(callee, type_args, args, *optional),
            Expr::New { ty, args } => self.check_new(ty, args),
            Expr::Member {
                obj,
                name,
                optional,
            } => {
                // A narrowed path wins: `if (a.b != null) { a.b.c }`.
                if !*optional {
                    if let Some(path) = Self::narrow_path(e) {
                        for n in self.narrows.iter().rev() {
                            if let Some(t) = n.get(&path) {
                                return t.clone();
                            }
                        }
                    }
                }
                let ot = self.check_expr(obj, None);
                // The editor's cursor: `foo.<here>` arrives as
                // `foo.MERSEY__COMPLETE`, and what it wants to know is what
                // `foo` turned out to be.
                if self.want_marker && name == COMPLETION_MARKER {
                    self.marker_recv = Some(ot.clone());
                    return Type::Err;
                }
                self.member_access(&ot, name, *optional, pos_of(obj))
            }
            Expr::Index {
                obj,
                index,
                optional,
            } => {
                let ot = self.check_expr(obj, None);
                let it = self.check_expr(index, None);
                let base = if *optional {
                    strip_null(&ot)
                } else {
                    ot.clone()
                };
                let host_obj = matches!(base, Type::Iface(..));
                if !matches!(strip_narrow_helpers(&it), Type::Int(_) | Type::Err)
                    && !(host_obj && matches!(strip_narrow_helpers(&it), Type::Str))
                {
                    self.error(
                        Code::TypeMismatch,
                        format!("index must be an integer, got `{}`", self.show(&it)),
                        pos_of(index),
                    );
                }
                let out = match &base {
                    Type::Array(e) => e.as_ref().clone(),
                    Type::Str => Type::Char,
                    Type::Err => Type::Err,
                    Type::Unknown => {
                        self.error(
                            Code::BadOperand,
                            "cannot index a value of type `unknown`: narrow it first (`x as SomeType[]`)",
                            pos_of(obj),
                        );
                        Type::Err
                    }
                    // Host objects are indexable (`nodeList[0]`, `obj[key]`);
                    // the element type is not knowable from IDL.
                    Type::Iface(..) => Type::Unknown,
                    // Bytes: uint8 elements, promoted to int32 (§3.3).
                    Type::Class(id, _) if Some(*id) == self.bytes_id => Type::Int(IntKind::I32),
                    Type::Nullable(_) => {
                        self.error(
                            Code::NullableMisuse,
                            "value may be null; use `?.[…]` or narrow first",
                            pos_of(obj),
                        );
                        Type::Err
                    }
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!("`{}` is not indexable", self.show(other)),
                            pos_of(obj),
                        );
                        Type::Err
                    }
                };
                if *optional {
                    nullable(out)
                } else {
                    out
                }
            }
            Expr::SuperMember { name, pos } => {
                let Some(id) = self.current_class else {
                    return Type::Err;
                };
                if let Some((pid, pargs)) = self.classes[id].parent.clone() {
                    let parent_ty = Type::Class(pid, Rc::new(pargs));
                    return self.member_access(&parent_ty, name, false, *pos);
                }
                // Host-backed: `super.m()` is the host implementation.
                if let Some((iid, iargs)) = self.classes[id].host_parent.clone() {
                    let iface_ty = Type::Iface(iid, Rc::new(iargs));
                    return self.member_access(&iface_ty, name, false, *pos);
                }
                Type::Err
            }
            Expr::SuperCall { args, pos } => {
                let Some(id) = self.current_class else {
                    return Type::Err;
                };
                // Host-backed with no Mersey base: `super(…)` constructs the
                // host object (arguments are the interface constructor's).
                if self.classes[id].parent.is_none() && self.classes[id].host_parent.is_some() {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    return Type::Void;
                }
                let Some((pid, pargs)) = self.classes[id].parent.clone() else {
                    return Type::Err;
                };
                let map = self.subst_map(pid, &pargs);
                let params = self
                    .ctor_params(pid)
                    .into_iter()
                    .map(|p| ParamType {
                        ty: subst(&p.ty, &map),
                        ..p
                    })
                    .collect();
                let sig = FnType {
                    tparams: vec![],
                    params,
                    ret: Type::Void,
                };
                self.check_args_against(&sig, &[], args, *pos);
                Type::Void
            }
            Expr::ImportCall(inner) => {
                // The module graph is closed before execution (§4.5), and
                // running code has no authority to fetch more (§5.4). So a
                // dynamic import defers *evaluation*, not loading: the
                // specifier must be a literal, the module is already in the
                // graph, and what the import produces is therefore known
                // exactly — a promise of that module's exports, not `any`.
                let Some(spec) = string_literal(inner) else {
                    self.check_expr(inner, Some(&Type::Str));
                    self.error(
                        Code::BadCall,
                        "`import(…)` needs a literal specifier: the module graph is closed \
                         before execution (§4.5), so a module that is not named here could \
                         not be loaded, checked, or locked",
                        pos_of(inner),
                    );
                    return Type::Err;
                };
                let target = crate::graph::resolve_module(&self.module_spec, &spec);
                let Some(exp) = self.module_exports.get(&target) else {
                    self.error(
                        Code::UndefinedName,
                        format!("`{spec}` is not in the module graph"),
                        pos_of(inner),
                    );
                    return Type::Err;
                };
                let mut fields: Vec<RecField> = exp
                    .values
                    .iter()
                    .map(|(name, ty)| RecField {
                        name: name.clone(),
                        ty: ty.clone(),
                        optional: false,
                    })
                    .collect();
                fields.sort_by(|a, b| a.name.cmp(&b.name));
                let exports = Type::Record(Rc::new(fields));
                match self.promise_id.or(match self.type_defs.get("Promise") {
                    Some(TypeDef::Iface(id)) => Some(*id),
                    _ => None,
                }) {
                    Some(pid) => Type::Iface(pid, Rc::new(vec![exports])),
                    None => exports,
                }
            }
            Expr::Yield { value, pos } => {
                let want = self.yield_ty.clone();
                match (value, &want) {
                    (Some(v), Some(w)) => {
                        let t = self.check_expr(v, Some(w));
                        self.require_assignable_at(v, &t, w, "yielded value");
                    }
                    (Some(v), None) => {
                        self.check_expr(v, None);
                    }
                    (None, Some(w)) if !matches!(w, Type::Void | Type::Err) => {
                        self.error(
                            Code::BadReturn,
                            format!("expected a `{}` value to yield", self.show(w)),
                            *pos,
                        );
                    }
                    _ => {}
                }
                Type::Void
            }
        }
    }

    fn ctor_params(&self, id: ClassId) -> Vec<ParamType> {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some((params, _)) = &self.classes[cid].ctor {
                return params.clone();
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        Vec::new()
    }

    fn literal_ty(&mut self, kind: LitKind, text: &str) -> Type {
        match kind {
            LitKind::Null => Type::Null,
            LitKind::Bool => Type::Bool,
            LitKind::Str => Type::Str,
            LitKind::Char => Type::Char,
            LitKind::BigInt => Type::BigInt,
            LitKind::BigDec => Type::BigDec,
            LitKind::Float => {
                if text.ends_with('f') {
                    Type::F32
                } else {
                    Type::F64
                }
            }
            LitKind::Int => {
                let t = text.replace('_', "");
                const S: &[(&str, IntKind)] = &[
                    ("u64", IntKind::U64),
                    ("u32", IntKind::U32),
                    ("u16", IntKind::U16),
                    ("ul", IntKind::U64),
                    ("u8", IntKind::U8),
                    ("i64", IntKind::I64),
                    ("i32", IntKind::I32),
                    ("i16", IntKind::I16),
                    ("i8", IntKind::I8),
                    ("l", IntKind::I64),
                    ("u", IntKind::U32),
                ];
                let kind = S
                    .iter()
                    .find(|(s, _)| t.ends_with(s))
                    .map(|(_, k)| *k)
                    .unwrap_or(IntKind::I32);
                Type::Int(kind)
            }
        }
    }

    // ---- operators ----------------------------------------------------------------

    fn check_binary(&mut self, e: &Expr, op: BinOp, l: &Expr, r: &Expr) -> Type {
        match op {
            BinOp::And => {
                self.check_condition(l);
                // `a != null && a.b` — the right side sees the left's
                // narrowing, exactly as it does inside the `if` body.
                let (then, _) = self.narrow_from(l);
                self.narrows.push(then);
                self.check_condition(r);
                self.narrows.pop();
                Type::Bool
            }
            BinOp::Or => {
                self.check_condition(l);
                // `a == null || a.b` — the right side sees the *else* branch.
                let (_, els) = self.narrow_from(l);
                self.narrows.push(els);
                self.check_condition(r);
                self.narrows.pop();
                Type::Bool
            }
            BinOp::Coalesce => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                match &lt {
                    Type::Nullable(inner) => self.unify_pair(inner.as_ref().clone(), rt),
                    Type::Null => rt,
                    Type::Err => Type::Err,
                    _ => {
                        self.error(
                            Code::BadOperand,
                            format!("left of `??` is never null (`{}`)", self.show(&lt)),
                            pos_of(l),
                        );
                        lt
                    }
                }
            }
            BinOp::Instanceof => {
                self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                if !matches!(rt, Type::ClassMeta(_) | Type::IfaceMeta(_) | Type::Err) {
                    self.error(
                        Code::BadOperand,
                        "right side of `instanceof` must be a class or a host interface",
                        pos_of(r),
                    );
                }
                Type::Bool
            }
            BinOp::Eq | BinOp::Ne => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, Some(&lt));
                // `1 == 1l` compares as int64 — §3.3's widening applies here too,
                // and once both sides are the same number the engine can be told
                // which one instead of working it out from the values.
                if let Some(common) =
                    self.numeric_common(&strip_narrow_helpers(&lt), &strip_narrow_helpers(&rt))
                {
                    self.coerce(l, &lt, &common);
                    self.coerce(r, &rt, &common);
                    if let Some(n) = num_of(&common) {
                        self.op_types.insert(node_id(e), n);
                    }
                }
                if !self.comparable(&lt, &rt) {
                    self.error(
                        Code::BadOperand,
                        format!(
                            "`==` between `{}` and `{}` (no coercion, §3.3)",
                            self.show(&lt),
                            self.show(&rt)
                        ),
                        pos_of(r),
                    );
                }
                Type::Bool
            }
            _ => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                let out = self.check_binary_types(op, &lt, &rt, pos_of(r));
                // Usual arithmetic conversions (§3.3): both operands become the
                // common type *before* the operator runs. The engine promoted at
                // the operator instead, dispatching on whatever the values
                // happened to be — which gave the right answer only because the
                // values were usually right. Now the promotion is in the
                // bytecode, so the operator sees one type and the JIT can see it
                // too: `x / 2` in a `float64` function is a float divide, not an
                // integer one that happens to get fixed up.
                if let Some(common) =
                    self.numeric_common(&strip_narrow_helpers(&lt), &strip_narrow_helpers(&rt))
                {
                    self.coerce(l, &lt, &common);
                    self.coerce(r, &rt, &common);
                    // Both operands are this type now, and the engine no longer
                    // has to deduce it. An `int32 + int32` walked a string check,
                    // a bigint check, a promotion, and *then* a dispatch — four
                    // matches to add two numbers whose types were known all along.
                    if let Some(n) = num_of(&common) {
                        self.op_types.insert(node_id(e), n);
                    }
                }
                out
            }
        }
    }

    fn check_binary_types(&mut self, op: BinOp, lt: &Type, rt: &Type, pos: Pos) -> Type {
        use BinOp::*;
        let l = strip_narrow_helpers(lt);
        let r = strip_narrow_helpers(rt);
        if matches!(l, Type::Err) || matches!(r, Type::Err) {
            return Type::Err;
        }
        // string / char comparisons and concatenation
        match (&l, &r, op) {
            (Type::Str, Type::Str, Add) => return Type::Str,
            (Type::Str, Type::Str, Lt | Gt | Le | Ge) => return Type::Bool,
            (Type::Char, Type::Char, Lt | Gt | Le | Ge) => return Type::Bool,
            // bigint/bigdec: exact arithmetic among themselves (§3.7); they
            // never mix implicitly with fixed-size numerics (§3.3).
            (Type::BigInt, Type::BigInt, Add | Sub | Mul | Div | Rem | Pow) => return Type::BigInt,
            (Type::BigDec, Type::BigDec, Add | Sub | Mul | Div) => return Type::BigDec,
            (Type::BigInt, Type::BigInt, Lt | Gt | Le | Ge) => return Type::Bool,
            (Type::BigDec, Type::BigDec, Lt | Gt | Le | Ge) => return Type::Bool,
            _ => {}
        }

        // Arithmetic on a *bounded* type parameter: `<T extends Numeric>` means
        // every T that can be substituted is a number, so `a + b` on two T's is
        // a T — whichever number it turns out to be. Without this the bound says
        // something true and useless: a generic `sum<T>` could not add, and the
        // only way to write one was to give up the width and take the value
        // untyped. Both operands must be the *same* parameter; mixing `T` and
        // `U` has no common type to promote to.
        //
        // `%` and `**` are left out on purpose: `Numeric` admits `bigdec`, which
        // has neither, so allowing them here would be a promise the substitution
        // cannot keep.
        if let (Type::Var(a), Type::Var(b)) = (&l, &r) {
            if a == b && self.tv_is_numeric(*a) {
                return match op {
                    Add | Sub | Mul | Div => l.clone(),
                    Lt | Gt | Le | Ge => Type::Bool,
                    _ => {
                        self.error(
                            Code::BadOperand,
                            format!(
                                "`{}` is not available on a `Numeric` type parameter",
                                op.as_str()
                            ),
                            pos,
                        );
                        Type::Err
                    }
                };
            }
        }
        let Some(common) = self.numeric_common(&l, &r) else {
            self.error(
                Code::BadOperand,
                format!(
                    "`{}` needs numeric operands, got `{}` and `{}`",
                    op.as_str(),
                    self.show(&l),
                    self.show(&r)
                ),
                pos,
            );
            return Type::Err;
        };
        match op {
            Lt | Gt | Le | Ge => Type::Bool,
            Shl | Shr | BitAnd | BitOr | BitXor | Rem => {
                if matches!(common, Type::F32 | Type::F64)
                    && matches!(op, Shl | Shr | BitAnd | BitOr | BitXor)
                {
                    self.error(
                        Code::BadOperand,
                        "bitwise operators need integer operands",
                        pos,
                    );
                    Type::Err
                } else {
                    common
                }
            }
            _ => common,
        }
    }

    /// Usual arithmetic conversions (§3.3): promote small ints to int32,
    /// then wider rank wins, float wins, unsigned wins at equal rank.
    /// Is this type parameter bounded by `Numeric`?
    fn tv_is_numeric(&self, tv: TvId) -> bool {
        let Some(numeric) = self.numeric_id else {
            return false;
        };
        matches!(self.tv_bounds.get(&tv), Some(Type::Iface(id, _)) if *id == numeric)
    }

    fn numeric_common(&self, a: &Type, b: &Type) -> Option<Type> {
        let promote = |t: &Type| -> Option<Type> {
            Some(match t {
                Type::Int(k) if k.bits() < 32 => Type::Int(IntKind::I32),
                Type::Int(k) => Type::Int(*k),
                Type::F32 => Type::F32,
                Type::F64 => Type::F64,
                _ => return None,
            })
        };
        let (a, b) = (promote(a)?, promote(b)?);
        let rank = |t: &Type| match t {
            Type::Int(IntKind::I32) => 0,
            Type::Int(IntKind::U32) => 1,
            Type::Int(IntKind::I64) => 2,
            Type::Int(IntKind::U64) => 3,
            Type::F32 => 4,
            _ => 5,
        };
        Some(if rank(&a) >= rank(&b) { a } else { b })
    }

    fn comparable(&self, a: &Type, b: &Type) -> bool {
        let (a, b) = (strip_narrow_helpers(a), strip_narrow_helpers(b));
        if matches!(a, Type::Err) || matches!(b, Type::Err) {
            return true;
        }
        // Host objects compare by identity.
        if matches!(a, Type::Iface(..)) && matches!(b, Type::Iface(..)) {
            return true;
        }
        if self.numeric_common(&a, &b).is_some() {
            return true;
        }
        // null against nullable / null
        if matches!(a, Type::Null) || matches!(b, Type::Null) {
            return true; // nullability of the other side is the binder's TDZ story; == null is always meaningful
        }
        self.assignable(&a, &b) || self.assignable(&b, &a)
    }

    // ---- calls ------------------------------------------------------------------------

    fn check_call(
        &mut self,
        callee: &Expr,
        type_args: &[ast::TypeExpr],
        args: &[ArrayElem],
        optional: bool,
    ) -> Type {
        // console/document natives get bespoke signatures.
        if let Expr::Member { obj, name, .. } = callee {
            let ot = self.check_expr(obj, None);
            // `JSON.stringify({literal})` where the receiver is statically the
            // JSON global — either `std:json` (`Namespace(Ns::Json)`) or the
            // `browser:dom` `__JSON` interface object. Check the argument once,
            // then (if every field is a bakeable constant or an int32/int64)
            // authorize the compiler to fuse it into a template. Non-fusable
            // shapes fall through to the ordinary path unmarked.
            let is_json_global = match strip_null(&ot) {
                Type::Namespace(Ns::Json) => true,
                Type::Iface(iid, _) => self.ifaces[iid].name == "__JSON",
                _ => false,
            };
            if is_json_global && name == "stringify" && args.len() == 1 && !args[0].spread {
                let arg_ty = self.check_expr(&args[0].expr, None);
                self.try_authorize_json_fusion(callee, &args[0].expr, &arg_ty);
                return Type::Str;
            }
            match (&strip_null(&ot), name.as_str()) {
                (Type::Namespace(Ns::Console), "log") => {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    return Type::Void;
                }
                (Type::Namespace(Ns::Document), "getElementById" | "createElement") => {
                    if let Some(a) = args.first() {
                        let t = self.check_expr(&a.expr, Some(&Type::Str));
                        self.require_assignable(&t, &Type::Str, pos_of(&a.expr), "argument");
                    }
                    return Type::Class(self.element_id, Rc::new(vec![]));
                }
                (Type::Namespace(Ns::Opaque), _) => {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    return Type::Unknown;
                }
                (Type::Unknown, _) => {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    self.error(
                        Code::BadCall,
                        format!(
                            "cannot call `{name}` on a value of type `unknown`: narrow it first (`x as SomeType`, or `x instanceof T`)"
                        ),
                        pos_of(obj),
                    );
                    return Type::Err;
                }
                _ => {
                    // WebIDL overloads are emitted as `name`, `name$1`, … —
                    // select the first that accepts this call rather than
                    // merging them into one loose signature.
                    if let Some(sel) =
                        self.select_overload(&ot, name, args, pos_of(callee), optional)
                    {
                        return sel;
                    }
                    let fty = self.member_access(&ot, name, optional, pos_of(obj));
                    return self.invoke(&fty, type_args, args, pos_of(callee), optional);
                }
            }
        }
        let fty = self.check_expr(callee, None);
        self.invoke(&fty, type_args, args, pos_of(callee), optional)
    }

    /// Decide whether `JSON.stringify(arg)` may be fused into a template, and if
    /// so record the authorization. Fusable only when `arg` is an object literal
    /// whose every field is either a bakeable scalar literal (string, bool, null,
    /// integer) or a dynamic value the checker typed `int32`/`int64` — the only
    /// dynamic values whose decimal template rendering is byte-identical to what
    /// `JSON.stringify` would emit. `arg_ty` is the record's already-checked type,
    /// carrying the per-field types this decision needs.
    fn try_authorize_json_fusion(&mut self, callee: &Expr, arg: &Expr, arg_ty: &Type) {
        let Expr::Record(fields) = arg else { return };
        let Type::Record(fs) = strip_null(arg_ty) else {
            return;
        };
        let mut dyn_ints: Vec<usize> = Vec::new();
        for f in fields {
            let RecordField::Named { name, value } = f else {
                return; // a spread field — bail
            };
            // A bakeable scalar literal (string, bool, null, integer) is const-folded
            // into the template fragment by the compiler; nothing to authorize.
            let is_bakeable_lit = matches!(
                value,
                Some(Expr::Lit { kind, .. })
                    if matches!(kind, LitKind::Str | LitKind::Bool | LitKind::Null | LitKind::Int)
            );
            if is_bakeable_lit {
                continue;
            }
            // Otherwise the value must render as an int32/int64. Look its checked
            // type up by name — `Type::Record` does not preserve source order.
            let Some(v) = value else { return }; // `{ x }` shorthand — bail
            let Some(rf) = fs.iter().find(|rf| rf.name == name.text) else {
                return;
            };
            match num_of(&strip_null(&rf.ty)) {
                Some(Num::Int(IntKind::I32)) | Some(Num::Int(IntKind::I64)) => {
                    dyn_ints.push(node_id(v));
                }
                _ => return,
            }
        }
        for id in dyn_ints {
            JSON_DYN_INT.with(|m| m.borrow_mut().insert(id));
        }
        note_json_stringify(callee);
    }

    /// If `name` has WebIDL overloads (`name$1`, `name$2`, …), pick the first
    /// signature that accepts the call. Returns None when there are none, so
    /// the caller falls back to the ordinary path (and its diagnostics).
    fn select_overload(
        &mut self,
        recv: &Type,
        name: &str,
        args: &[ArrayElem],
        pos: Pos,
        optional: bool,
    ) -> Option<Type> {
        // Collect the alternatives, quietly.
        let mut candidates: Vec<Type> = Vec::new();
        let n = self.diags.len();
        for i in 0.. {
            let probe = if i == 0 {
                name.to_string()
            } else {
                format!("{name}${i}")
            };
            let ty = self.member_access(recv, &probe, optional, pos);
            if matches!(ty, Type::Err) {
                break;
            }
            candidates.push(ty);
            if i > 8 {
                break; // no IDL member has this many overloads
            }
        }
        self.diags.truncate(n);
        if candidates.len() <= 1 {
            return None; // not overloaded: the normal path reports properly
        }
        // First signature that type-checks wins (IDL overload resolution
        // order); if none does, report against the first.
        for cand in &candidates {
            let before = self.diags.len();
            let ret = self.invoke(cand, &[], args, pos, optional);
            if self.diags.len() == before {
                return Some(ret);
            }
            self.diags.truncate(before);
        }
        Some(self.invoke(&candidates[0], &[], args, pos, optional))
    }

    fn invoke(
        &mut self,
        fty: &Type,
        type_args: &[ast::TypeExpr],
        args: &[ArrayElem],
        pos: Pos,
        optional: bool,
    ) -> Type {
        let base = if optional {
            strip_null(fty)
        } else {
            fty.clone()
        };
        match &base {
            Type::Fn(f) => {
                let f = f.clone();
                let ret = self.check_args_against(&f, type_args, args, pos);
                if optional {
                    nullable(ret)
                } else {
                    ret
                }
            }
            Type::Err => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                Type::Err
            }
            Type::Nullable(_) => {
                self.error(
                    Code::NullableMisuse,
                    "value may be null; use `?.()` or narrow first",
                    pos,
                );
                Type::Err
            }
            other => {
                self.error(
                    Code::BadCall,
                    format!("`{}` is not callable", self.show(other)),
                    pos,
                );
                Type::Err
            }
        }
    }

    /// Check arguments against a signature; handles explicit type arguments
    /// and simple inference. Returns the (substituted) return type.
    fn check_args_against(
        &mut self,
        f: &FnType,
        type_args: &[ast::TypeExpr],
        args: &[ArrayElem],
        pos: Pos,
    ) -> Type {
        let mut map: HashMap<TvId, Type> = HashMap::new();
        if !type_args.is_empty() {
            if type_args.len() != f.tparams.len() {
                self.error(
                    Code::BadCall,
                    format!(
                        "expected {} type argument(s), got {}",
                        f.tparams.len(),
                        type_args.len()
                    ),
                    pos,
                );
            }
            for (tv, ta) in f.tparams.iter().zip(type_args) {
                let t = self.resolve_type(ta);
                map.insert(*tv, t);
            }
        }

        let rest = f.params.iter().find(|p| p.rest).cloned();
        let positional: Vec<&ParamType> = f.params.iter().filter(|p| !p.rest).collect();
        let required = positional.iter().filter(|p| !p.optional).count();
        let max = if rest.is_some() {
            usize::MAX
        } else {
            positional.len()
        };

        // A spread argument has a length nobody knows until it runs, so a call
        // that uses one is only checkable if the callee takes a rest parameter —
        // then any number of arguments is fine and the element type still is
        // not. Otherwise the arity of the call could not be checked at all, and
        // reporting "expected 2, got 1" would be describing the spread rather
        // than the mistake.
        if args.iter().any(|a| a.spread) {
            if rest.is_none() {
                self.error(
                    Code::BadCall,
                    "a spread argument needs a function with a rest parameter \
                     (`...xs: T[]`): otherwise the number of arguments cannot be checked"
                        .to_string(),
                    pos,
                );
                return f.ret.clone();
            }
            let rest_elem = rest.as_ref().map(|p| p.ty.clone()).unwrap_or(Type::Err);
            for a in args {
                let t = self.check_expr(&a.expr, None);
                if a.spread {
                    let want = Type::Array(Rc::new(rest_elem.clone()));
                    self.require_assignable(&t, &want, pos_of(&a.expr), "spread argument");
                } else {
                    self.require_assignable_at(&a.expr, &t, &rest_elem, "argument");
                }
            }
            return f.ret.clone();
        }

        if args.len() < required || args.len() > max {
            self.error(
                Code::BadCall,
                format!(
                    "expected {}{} argument(s), got {}",
                    required,
                    if max != required {
                        if rest.is_some() {
                            "+".to_string()
                        } else {
                            format!("..{}", positional.len())
                        }
                    } else {
                        String::new()
                    },
                    args.len()
                ),
                pos,
            );
        }

        for (i, a) in args.iter().enumerate() {
            let want_raw = positional
                .get(i)
                .map(|p| p.ty.clone())
                .or_else(|| rest.as_ref().map(|r| r.ty.clone()))
                .unwrap_or(Type::Err);
            let want_spread = if a.spread {
                Type::Array(Rc::new(want_raw.clone()))
            } else {
                want_raw
            };
            // Infer type params not yet fixed, from this argument.
            let hint = subst(&want_spread, &map);
            let at = self.check_expr(&a.expr, Some(&hint));
            if !f.tparams.is_empty() {
                unify_infer(&want_spread, &at, &f.tparams, &mut map);
            }
            let want = subst(&want_spread, &map);
            // The argument's own type may still mention type variables that
            // this same argument just fixed (a callback whose return type is
            // `U`, inferred from its parameters) — substitute it too.
            let at = subst(&at, &map);
            self.require_assignable_at(&a.expr, &at, &want, "argument");
        }
        // A type parameter nobody managed to infer: `Err`, so the failure stays
        // quiet rather than becoming a value that can be used as anything.
        for tv in &f.tparams {
            map.entry(*tv).or_insert(Type::Err);
        }
        // Every inferred/explicit type argument must satisfy its bound.
        if !f.tparams.is_empty() {
            let inferred: Vec<Type> = f.tparams.iter().map(|tv| map[tv].clone()).collect();
            let tvs = f.tparams.clone();
            self.check_bounds(&tvs, &inferred, "this call", pos);
        }
        subst(&f.ret, &map)
    }

    fn check_new(&mut self, ty: &ast::TypeExpr, args: &[ArrayElem]) -> Type {
        let t = self.resolve_type(ty);
        let (id, targs) = match &t {
            Type::Class(id, args) => (*id, args.as_ref().clone()),
            // `new WebSocket(url)`, `new Uint8Array(4)`: web-platform
            // constructors are interfaces, built through the bridge.
            Type::Iface(..) => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                return t;
            }
            Type::Err => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                return Type::Err;
            }
            other => {
                self.error(
                    Code::BadCall,
                    format!("`new` needs a class, got `{}`", self.show(other)),
                    type_pos(ty),
                );
                return Type::Err;
            }
        };
        if self.classes[id].is_abstract {
            self.error(
                Code::BadCall,
                format!(
                    "cannot instantiate abstract class `{}`",
                    self.classes[id].name
                ),
                type_pos(ty),
            );
        }
        // Ctor accessibility.
        if let Some((_, access)) = &self.classes[id].ctor {
            self.check_access(*access, id, type_pos(ty), "constructor");
        }
        let map = self.subst_map(id, &targs);
        let params = self
            .ctor_params(id)
            .into_iter()
            .map(|p| ParamType {
                ty: subst(&p.ty, &map),
                ..p
            })
            .collect();
        let sig = FnType {
            tparams: vec![],
            params,
            ret: Type::Void,
        };
        self.check_args_against(&sig, &[], args, type_pos(ty));
        t
    }

    // ---- members & access control -----------------------------------------------------

    fn subst_map(&self, id: ClassId, args: &[Type]) -> HashMap<TvId, Type> {
        self.classes[id]
            .tparams
            .iter()
            .copied()
            .zip(args.iter().cloned())
            .collect()
    }

    fn field_info(&self, id: ClassId, name: &str) -> Option<(Type, Access, bool, ClassId)> {
        let mut cur = Some((id, Vec::new()));
        let mut acc_map: HashMap<TvId, Type> = HashMap::new();
        while let Some((cid, _)) = cur {
            if let Some(f) = self.classes[cid].fields.iter().find(|f| f.name == name) {
                return Some((subst(&f.ty, &acc_map), f.access, f.readonly, cid));
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Type> = pargs.iter().map(|t| subst(t, &acc_map)).collect();
                    acc_map = self.subst_map(*pid, &substituted);
                    cur = Some((*pid, substituted));
                }
                None => cur = None,
            }
        }
        None
    }

    /// Getter/setter lookup through the inheritance chain (substituted).
    fn accessor_info(
        &self,
        id: ClassId,
        name: &str,
        setter: bool,
    ) -> Option<(Type, Access, ClassId)> {
        let mut cur = Some(id);
        let mut acc_map: HashMap<TvId, Type> = HashMap::new();
        while let Some(cid) = cur {
            let list = if setter {
                &self.classes[cid].setters
            } else {
                &self.classes[cid].getters
            };
            if let Some(a) = list.iter().find(|a| a.name == name) {
                return Some((subst(&a.ty, &acc_map), a.access, cid));
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Type> = pargs.iter().map(|t| subst(t, &acc_map)).collect();
                    acc_map = self.subst_map(*pid, &substituted);
                    cur = Some(*pid);
                }
                None => cur = None,
            }
        }
        None
    }

    fn method_sig(&self, id: ClassId, name: &str, want_static: bool) -> Option<FnType> {
        let mut cur = Some(id);
        let mut acc_map: HashMap<TvId, Type> = HashMap::new();
        while let Some(cid) = cur {
            if let Some(m) = self.classes[cid]
                .methods
                .iter()
                .find(|m| m.name == name && m.is_static == want_static)
            {
                return Some(FnType {
                    tparams: m.sig.tparams.clone(),
                    params: m
                        .sig
                        .params
                        .iter()
                        .map(|p| ParamType {
                            ty: subst(&p.ty, &acc_map),
                            ..p.clone()
                        })
                        .collect(),
                    ret: subst(&m.sig.ret, &acc_map),
                });
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Type> = pargs.iter().map(|t| subst(t, &acc_map)).collect();
                    acc_map = self.subst_map(*pid, &substituted);
                    cur = Some(*pid);
                }
                None => cur = None,
            }
        }
        None
    }

    fn check_access(&mut self, access: Access, owner: ClassId, pos: Pos, what: &str) {
        let ok = match access {
            Access::Public => true,
            Access::Private => self.current_class == Some(owner),
            Access::Protected => {
                let mut cur = self.current_class;
                let mut ok = false;
                while let Some(cid) = cur {
                    if cid == owner {
                        ok = true;
                        break;
                    }
                    cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
                }
                ok
            }
        };
        if !ok {
            self.error(
                Code::AccessViolation,
                format!(
                    "{what} is {} in class `{}`",
                    access.as_str(),
                    self.classes[owner].name
                ),
                pos,
            );
        }
    }

    /// Member type without diagnostics (destructuring, record contexts).
    fn member_type_quiet(&mut self, t: &Type, name: &str) -> Option<Type> {
        match t {
            Type::Record(fs) => fs.iter().find(|f| f.name == name).map(|f| f.ty.clone()),
            Type::Class(id, args) => {
                let map = self.subst_map(*id, args);
                self.field_info(*id, name)
                    .map(|(t, ..)| subst(&t, &map))
                    .or_else(|| {
                        self.method_sig(*id, name, false).map(|s| {
                            Type::Fn(Rc::new(FnType {
                                tparams: s.tparams.clone(),
                                params: s
                                    .params
                                    .iter()
                                    .map(|p| ParamType {
                                        ty: subst(&p.ty, &map),
                                        ..p.clone()
                                    })
                                    .collect(),
                                ret: subst(&s.ret, &map),
                            }))
                        })
                    })
            }
            // Host interfaces too — otherwise a union of two host types could
            // never share a member, which is most of what the IDL's unions are.
            Type::Iface(id, args) => {
                let imap: HashMap<TvId, Type> = self.ifaces[*id]
                    .tparams
                    .iter()
                    .copied()
                    .zip(args.iter().cloned())
                    .collect();
                self.iface_member(*id, name).map(|(t, optional)| {
                    let t = subst(&t, &imap);
                    if optional {
                        nullable(t)
                    } else {
                        t
                    }
                })
            }
            Type::Err => Some(Type::Err),
            // Everything else — strings, arrays, namespaces — goes through the
            // *same* typing as a real member access, with the diagnostics
            // suppressed. So documentation and completion cannot describe a
            // member differently from how it is checked, or miss one entirely,
            // because they are asking the checker rather than keeping a list.
            other => {
                let n = self.diags.len();
                let ty = self.member_access(&other, name, false, Pos { line: 0, col: 0 });
                let failed = self.diags.len() > n;
                self.diags.truncate(n);
                if failed || matches!(ty, Type::Err) {
                    None
                } else {
                    Some(ty)
                }
            }
        }
    }

    fn member_access(&mut self, ot: &Type, name: &str, optional: bool, pos: Pos) -> Type {
        let base = if optional { strip_null(ot) } else { ot.clone() };
        let out = match &base {
            // A union has a member when *every* arm has it, at the same type:
            // then reading it is safe whichever arm the value turns out to be.
            // Otherwise it does not, and you narrow first (`x is T`,
            // `x instanceof T`, `x as T`).
            Type::Union(arms) => {
                let arms = arms.clone();
                let mut found: Option<Type> = None;
                for arm in arms.iter() {
                    let Some(t) = self.member_type_quiet(arm, name) else {
                        self.error(
                            Code::UnknownMember,
                            format!(
                                "`{}` has no member `{name}`, so the union does not either: narrow it first",
                                self.show(arm)
                            ),
                            pos,
                        );
                        return Type::Err;
                    };
                    match &found {
                        None => found = Some(t),
                        Some(prev) if self.ty_eq(prev, &t) => {}
                        Some(prev) => {
                            // Same name, different types: reading it would give
                            // you one of two things and you would not know which.
                            self.error(
                                Code::UnknownMember,
                                format!(
                                    "`{name}` is `{}` on one arm of the union and `{}` on another: narrow it first",
                                    self.show(prev),
                                    self.show(&t)
                                ),
                                pos,
                            );
                            return Type::Err;
                        }
                    }
                }
                found.unwrap_or(Type::Err)
            }
            // The whole point of `unknown`: you cannot read a member of a value
            // whose type nobody knows. Narrow it first. (`any` allowed this, and
            // then every member it produced was `any` too — which is how one
            // untyped value at a boundary made a whole program untyped.)
            Type::Unknown => {
                self.error(
                    Code::UnknownMember,
                    format!(
                        "cannot read `{name}` on a value of type `unknown`: narrow it first (`x as SomeType`, or `x instanceof T`)"
                    ),
                    pos,
                );
                Type::Err
            }
            Type::Nullable(_) => {
                self.error(
                    Code::NullableMisuse,
                    format!("value may be null; use `?.{name}` or narrow first"),
                    pos,
                );
                Type::Err
            }
            Type::Null => {
                self.error(Code::NullableMisuse, "value is null here", pos);
                Type::Err
            }
            Type::Err => Type::Err,
            Type::Str => {
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                let f = |params: Vec<ParamType>, ret: Type| {
                    Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params,
                        ret,
                    }))
                };
                match name {
                    "length" => Type::Int(IntKind::I32),
                    "toString" => to_string_fn(),
                    "indexOf" => f(vec![p(Type::Str)], Type::Int(IntKind::I32)),
                    "contains" => f(vec![p(Type::Str)], Type::Bool),
                    "startsWith" | "endsWith" => f(vec![p(Type::Str)], Type::Bool),
                    "slice" => f(
                        vec![
                            p(Type::Int(IntKind::I32)),
                            ParamType {
                                ty: Type::Int(IntKind::I32),
                                optional: true,
                                rest: false,
                            },
                        ],
                        Type::Str,
                    ),
                    "split" => f(vec![p(Type::Str)], Type::Array(Rc::new(Type::Str))),
                    "toUpperCase" | "toLowerCase" | "trim" => f(vec![], Type::Str),
                    "trimStart" | "trimEnd" => f(vec![], Type::Str),
                    // Like `slice`, but it *swaps* the bounds when they are the
                    // wrong way round — that is the whole difference between the
                    // two, and the reason both exist rather than one being an
                    // alias of the other.
                    "substring" => f(
                        vec![
                            p(Type::Int(IntKind::I32)),
                            ParamType {
                                ty: Type::Int(IntKind::I32),
                                optional: true,
                                rest: false,
                            },
                        ],
                        Type::Str,
                    ),
                    "concat" => f(
                        vec![ParamType {
                            ty: Type::Str,
                            optional: false,
                            rest: true,
                        }],
                        Type::Str,
                    ),
                    // `charAt` is the JS-shaped one: a *string* of one code
                    // point, empty when the index is out of range. `s[i]` and
                    // `s.at(i)` give a `char`, which is usually what you want.
                    "charAt" => f(vec![p(Type::Int(IntKind::I32))], Type::Str),
                    // The code point's numeric value. `null` out of range —
                    // there is no `undefined` here to return instead (§3.2).
                    "codePointAt" => f(
                        vec![p(Type::Int(IntKind::I32))],
                        nullable(Type::Int(IntKind::I32)),
                    ),
                    "lastIndexOf" => f(vec![p(Type::Str)], Type::Int(IntKind::I32)),
                    // A string is a sequence of code points, so `s[i]` is a
                    // `char`; `at` is the form that admits it can miss, and
                    // counts from the end for a negative index.
                    "at" => f(vec![p(Type::Int(IntKind::I32))], nullable(Type::Char)),
                    "replace" | "replaceAll" => f(vec![p(Type::Str), p(Type::Str)], Type::Str),
                    "repeat" => f(vec![p(Type::Int(IntKind::I32))], Type::Str),
                    "padStart" | "padEnd" => f(
                        vec![
                            p(Type::Int(IntKind::I32)),
                            ParamType {
                                ty: Type::Str,
                                optional: true,
                                rest: false,
                            },
                        ],
                        Type::Str,
                    ),
                    _ => self.no_member("string", name, pos),
                }
            }
            Type::BigDec if name == "divide" => Type::Fn(Rc::new(FnType {
                tparams: vec![],
                params: vec![
                    ParamType {
                        ty: Type::BigDec,
                        optional: false,
                        rest: false,
                    },
                    ParamType {
                        ty: Type::Record(Rc::new(vec![
                            RecField {
                                name: "scale".into(),
                                ty: Type::Int(IntKind::I32),
                                optional: false,
                            },
                            RecField {
                                name: "mode".into(),
                                ty: Type::Str,
                                optional: true,
                            },
                        ])),
                        optional: false,
                        rest: false,
                    },
                ],
                ret: Type::BigDec,
            })),
            Type::Char
            | Type::Bool
            | Type::Int(_)
            | Type::F32
            | Type::F64
            | Type::BigInt
            | Type::BigDec
            | Type::Enum(_) => match name {
                "toString" => to_string_fn(),
                _ => self.no_member(&self.show(&base), name, pos),
            },
            Type::Array(elem) => {
                let e = elem.as_ref().clone();
                let arr = Type::Array(Rc::new(e.clone()));
                let i32t = Type::Int(IntKind::I32);
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                let opt = |ty: Type| ParamType {
                    ty,
                    optional: true,
                    rest: false,
                };
                let f = |tparams: Vec<TvId>, params: Vec<ParamType>, ret: Type| {
                    Type::Fn(Rc::new(FnType {
                        tparams,
                        params,
                        ret,
                    }))
                };
                // Callback shapes.
                let pred = Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![p(e.clone())],
                    ret: Type::Bool,
                }));
                let each = Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![p(e.clone())],
                    ret: Type::Void,
                }));
                let cmp = Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![p(e.clone()), p(e.clone())],
                    ret: i32t.clone(),
                }));
                match name {
                    "length" => i32t,
                    // §1.3: mutation is explicit in the name; mutators return void.
                    "push" => f(
                        vec![],
                        vec![ParamType {
                            ty: e.clone(),
                            optional: false,
                            rest: true,
                        }],
                        Type::Void,
                    ),
                    "pop" => f(vec![], vec![], nullable(e.clone())),
                    "clear" => f(vec![], vec![], Type::Void),
                    "sortInPlace" => f(vec![], vec![p(cmp.clone())], Type::Void),
                    "reverseInPlace" => f(vec![], vec![], Type::Void),
                    "toSorted" => f(vec![], vec![p(cmp)], arr.clone()),
                    "toReversed" => f(vec![], vec![], arr.clone()),
                    // Views and transforms.
                    "map" => {
                        let u = self.fresh_tv("U");
                        let mapper = Type::Fn(Rc::new(FnType {
                            tparams: vec![],
                            params: vec![p(e.clone())],
                            ret: Type::Var(u),
                        }));
                        f(vec![u], vec![p(mapper)], Type::Array(Rc::new(Type::Var(u))))
                    }
                    "reduce" => {
                        let u = self.fresh_tv("U");
                        let folder = Type::Fn(Rc::new(FnType {
                            tparams: vec![],
                            params: vec![p(Type::Var(u)), p(e.clone())],
                            ret: Type::Var(u),
                        }));
                        f(vec![u], vec![p(folder), p(Type::Var(u))], Type::Var(u))
                    }
                    "filter" => f(vec![], vec![p(pred.clone())], arr.clone()),
                    "find" => f(vec![], vec![p(pred.clone())], nullable(e.clone())),
                    "findIndex" => f(vec![], vec![p(pred.clone())], i32t.clone()),
                    "some" | "every" => f(vec![], vec![p(pred)], Type::Bool),
                    "forEach" => f(vec![], vec![p(each)], Type::Void),
                    "indexOf" => f(vec![], vec![p(e.clone())], i32t.clone()),
                    "contains" => f(vec![], vec![p(e.clone())], Type::Bool),
                    "slice" => f(vec![], vec![p(i32t.clone()), opt(i32t)], arr.clone()),
                    "concat" => f(vec![], vec![p(arr.clone())], arr),
                    "keys" => f(
                        vec![],
                        vec![],
                        Type::Array(Rc::new(Type::Int(IntKind::I32))),
                    ),
                    "join" => f(vec![], vec![opt(Type::Str)], Type::Str),
                    // Indexing that admits it can miss: `xs[i]` is `T`, but
                    // `xs.at(i)` is `T?` — and it counts from the end for a
                    // negative index.
                    "at" => f(vec![], vec![p(i32t.clone())], nullable(e.clone())),
                    "lastIndexOf" => f(vec![], vec![p(e.clone())], i32t.clone()),
                    // §1.3: mutation is explicit in the name, and mutators
                    // return void. These are what JS spells `unshift`, `shift`
                    // and `splice`.
                    "insertAt" => f(vec![], vec![p(i32t.clone()), p(e.clone())], Type::Void),
                    "removeAt" => f(vec![], vec![p(i32t.clone())], nullable(e.clone())),
                    "fillInPlace" => f(vec![], vec![p(e.clone())], Type::Void),
                    // `T[][]` -> `T[]`. Only one level: a deeper flatten cannot
                    // be given a type without a variadic depth, so it is not
                    // pretended to.
                    "flat" => match &e {
                        Type::Array(inner) => {
                            f(vec![], vec![], Type::Array(Rc::new(inner.as_ref().clone())))
                        }
                        other => {
                            self.error(
                                Code::BadOperand,
                                format!(
                                    "`flat` needs an array of arrays, this is `{}[]`",
                                    self.show(other)
                                ),
                                pos,
                            );
                            Type::Err
                        }
                    },
                    "toString" => to_string_fn(),
                    _ => self.no_member("array", name, pos),
                }
            }
            Type::Record(fs) => match fs.iter().find(|f| f.name == name) {
                Some(f) => {
                    if f.optional {
                        nullable(f.ty.clone())
                    } else {
                        f.ty.clone()
                    }
                }
                None => self.no_member("record", name, pos),
            },
            Type::Class(id, args) => {
                let id = *id;
                let map = self.subst_map(id, args);
                if let Some((t, access, _ro, owner)) = self.field_info(id, name) {
                    self.check_access(access, owner, pos, &format!("field `{name}`"));
                    subst(&t, &map)
                } else if let Some((t, access, owner)) = self.accessor_info(id, name, false) {
                    self.check_access(access, owner, pos, &format!("accessor `{name}`"));
                    subst(&t, &map)
                } else if let Some(sig) = self.method_sig(id, name, false) {
                    let access = self.method_access(id, name);
                    self.check_access(access, id, pos, &format!("method `{name}`"));
                    Type::Fn(Rc::new(FnType {
                        tparams: sig.tparams.clone(),
                        params: sig
                            .params
                            .iter()
                            .map(|p| ParamType {
                                ty: subst(&p.ty, &map),
                                ..p.clone()
                            })
                            .collect(),
                        ret: subst(&sig.ret, &map),
                    }))
                } else if let Some((iid, iargs)) = self.host_parent_of(id) {
                    // Host-backed: fall back to the interface's members.
                    let imap: HashMap<TvId, Type> = self.ifaces[iid]
                        .tparams
                        .iter()
                        .copied()
                        .zip(iargs.iter().cloned())
                        .collect();
                    match self.iface_member(iid, name) {
                        Some((t, optional)) => {
                            let t = subst(&t, &imap);
                            if optional {
                                nullable(t)
                            } else {
                                t
                            }
                        }
                        None => self.no_member(&self.classes[id].name.clone(), name, pos),
                    }
                } else {
                    self.no_member(&self.classes[id].name.clone(), name, pos)
                }
            }
            Type::Iface(id, args) => {
                let id = *id;
                let imap: HashMap<TvId, Type> = self.ifaces[id]
                    .tparams
                    .iter()
                    .copied()
                    .zip(args.iter().cloned())
                    .collect();
                match self.iface_member(id, name) {
                    Some((t, optional)) => {
                        let t = subst(&t, &imap);
                        if optional {
                            nullable(t)
                        } else {
                            t
                        }
                    }
                    None => self.no_member(&self.ifaces[id].name.clone(), name, pos),
                }
            }
            Type::ClassMeta(id) => {
                let id = *id;
                if let Some(f) = self.classes[id]
                    .fields
                    .iter()
                    .find(|f| f.name == name && f.is_static)
                {
                    let (t, access) = (f.ty.clone(), f.access);
                    self.check_access(access, id, pos, &format!("static field `{name}`"));
                    t
                } else if let Some(sig) = self.method_sig(id, name, true) {
                    let access = self.method_access(id, name);
                    self.check_access(access, id, pos, &format!("static method `{name}`"));
                    Type::Fn(Rc::new(sig))
                } else {
                    self.no_member(&format!("class {}", self.classes[id].name), name, pos)
                }
            }
            Type::IfaceMeta(id) => {
                let id = *id;
                // Constants and statics live on the interface object; the
                // generator emits them as `__static_<Iface>`.
                let statics = format!("__static_{}", self.ifaces[id].name);
                if let Some(TypeDef::Iface(sid)) = self.type_defs.get(&statics) {
                    let sid = *sid;
                    if let Some((t, optional)) = self.iface_member(sid, name) {
                        return if optional { nullable(t) } else { t };
                    }
                }
                self.no_member(&format!("interface {}", self.ifaces[id].name), name, pos)
            }
            Type::EnumMeta(id) => {
                let id = *id;
                if self.enums[id].members.iter().any(|m| m == name) {
                    Type::Enum(id)
                } else {
                    self.no_member(&self.enums[id].name.clone(), name, pos)
                }
            }
            Type::Namespace(Ns::Bytes) => {
                let bytes_ty = match self.bytes_id {
                    Some(id) => Type::Class(id, Rc::new(vec![])),
                    None => Type::Err,
                };
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                match name {
                    "alloc" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::Int(IntKind::I32))],
                        ret: bytes_ty,
                    })),
                    "fromHost" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::Unknown)],
                        ret: bytes_ty,
                    })),
                    "toHost" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(bytes_ty)],
                        ret: Type::Unknown,
                    })),
                    "fill" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(bytes_ty), p(Type::Int(IntKind::I32))],
                        ret: Type::Void,
                    })),
                    // A Mersey string is a sequence of code points (§2.1); bytes
                    // are what a file or a socket actually holds. These are the
                    // only two functions that cross between them.
                    "encodeUtf8" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::Str)],
                        ret: bytes_ty,
                    })),
                    // `null` when the bytes are not valid UTF-8 — no replacement
                    // characters silently papering over a decoding failure.
                    "decodeUtf8" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(bytes_ty)],
                        ret: nullable(Type::Str),
                    })),
                    _ => self.no_member("bytes", name, pos),
                }
            }
            Type::Namespace(Ns::Regex) => {
                let regex_ty = match self.regex_id {
                    Some(id) => Type::Class(id, Rc::new(vec![])),
                    None => Type::Err,
                };
                match name {
                    "compile" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![
                            ParamType {
                                ty: Type::Str,
                                optional: false,
                                rest: false,
                            },
                            ParamType {
                                ty: Type::Str,
                                optional: true,
                                rest: false,
                            },
                        ],
                        ret: regex_ty,
                    })),
                    _ => self.no_member("regex", name, pos),
                }
            }
            Type::Namespace(Ns::Parse) => {
                // Parsing returns null on failure (§1.3: no sentinels).
                let s = ParamType {
                    ty: Type::Str,
                    optional: false,
                    rest: false,
                };
                let radix = ParamType {
                    ty: Type::Int(IntKind::I32),
                    optional: true,
                    rest: false,
                };
                let f = |params: Vec<ParamType>, ret: Type| {
                    Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params,
                        ret,
                    }))
                };
                match name {
                    "int32" => f(vec![s, radix], nullable(Type::Int(IntKind::I32))),
                    "int64" => f(vec![s, radix], nullable(Type::Int(IntKind::I64))),
                    "float64" => f(vec![s], nullable(Type::F64)),
                    "bigint" => f(vec![s], nullable(Type::BigInt)),
                    "bigdec" => f(vec![s], nullable(Type::BigDec)),
                    // Only "true"/"false", and null for anything else — no
                    // truthiness games, and no sentinel (§1.3).
                    "bool" => f(vec![s], nullable(Type::Bool)),
                    _ => self.no_member("parse", name, pos),
                }
            }
            Type::Namespace(Ns::Gc) => match name {
                "collect" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: Type::Void,
                })),
                "stats" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![],
                    ret: Type::Record(Rc::new(vec![RecField {
                        name: "live".into(),
                        ty: Type::Int(IntKind::I32),
                        optional: false,
                    }])),
                })),
                _ => self.no_member("gc", name, pos),
            },
            Type::Namespace(Ns::Time) => {
                let i32t = Type::Int(IntKind::I32);
                let parts = Type::Record(Rc::new(
                    [
                        "year", "month", "day", "hour", "minute", "second", "millis", "weekday",
                    ]
                    .iter()
                    .map(|n| RecField {
                        name: (*n).into(),
                        ty: i32t.clone(),
                        optional: false,
                    })
                    .collect::<Vec<_>>(),
                ));
                match name {
                    "now" | "monotonic" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![],
                        ret: Type::F64,
                    })),
                    "parts" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: Type::F64,
                            optional: false,
                            rest: false,
                        }],
                        ret: parts,
                    })),
                    "fromParts" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: parts,
                            optional: false,
                            rest: false,
                        }],
                        ret: Type::F64,
                    })),
                    // ISO-8601 in UTC, both ways. Null on a parse failure —
                    // never a guess (§1.3).
                    "format" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: Type::F64,
                            optional: false,
                            rest: false,
                        }],
                        ret: Type::Str,
                    })),
                    "parse" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: Type::Str,
                            optional: false,
                            rest: false,
                        }],
                        ret: nullable(Type::F64),
                    })),
                    _ => self.no_member("time", name, pos),
                }
            }
            // These namespaces used to be `any`, which meant
            // `const s: string = math.sqrt(16.0);` compiled, and so did
            // `fs.deleteEverything()`.
            Type::Namespace(Ns::Math) => {
                let num = Type::F64;
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                let f = |params: Vec<ParamType>, ret: Type| {
                    Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params,
                        ret,
                    }))
                };
                match name {
                    // `abs` keeps the width it was given (§3.3): `abs(-3)` is an
                    // int32, `abs(-3.5)` a float64. That is a *generic* function
                    // with a bound, not an untyped one — `math.abs("hi")` is an
                    // error now, and used to compile.
                    "abs" => {
                        let t = self.numeric_tv();
                        Type::Fn(Rc::new(FnType {
                            tparams: vec![t],
                            params: vec![p(Type::Var(t))],
                            ret: Type::Var(t),
                        }))
                    }
                    "floor" | "ceil" | "sqrt" | "round" | "trunc" | "sign" | "cbrt" | "exp"
                    | "log" | "log2" | "log10" | "sin" | "cos" | "tan" | "asin" | "acos"
                    | "atan" => f(vec![p(num.clone())], num),
                    "pow" | "atan2" | "hypot" => f(vec![p(num.clone()), p(num.clone())], num),
                    "clamp" => f(vec![p(num.clone()), p(num.clone()), p(num.clone())], num),
                    "isNaN" | "isFinite" => f(vec![p(num.clone())], Type::Bool),
                    // Same width in, same width out — and every argument the
                    // same type, which `any` could never say.
                    "min" | "max" => {
                        let t = self.numeric_tv();
                        Type::Fn(Rc::new(FnType {
                            tparams: vec![t],
                            params: vec![ParamType {
                                ty: Type::Var(t),
                                optional: false,
                                rest: true,
                            }],
                            ret: Type::Var(t),
                        }))
                    }
                    "PI" | "E" => num,
                    _ => self.no_member("math", name, pos),
                }
            }
            Type::Namespace(Ns::Format) => {
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                let i32t = Type::Int(IntKind::I32);
                match name {
                    "pad" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::Unknown), p(i32t)],
                        ret: Type::Str,
                    })),
                    "fixed" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::F64), p(i32t)],
                        ret: Type::Str,
                    })),
                    _ => self.no_member("format", name, pos),
                }
            }
            Type::Namespace(Ns::Fs) => match name {
                "readText" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        ty: Type::Str,
                        optional: false,
                        rest: false,
                    }],
                    ret: Type::Str,
                })),
                _ => self.no_member("fs", name, pos),
            },
            Type::Namespace(Ns::Env) => match name {
                // Absent variables are `null`, not `""` — the caller has to say
                // what to do about that (§3.2).
                "get" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        ty: Type::Str,
                        optional: false,
                        rest: false,
                    }],
                    ret: nullable(Type::Str),
                })),
                _ => self.no_member("env", name, pos),
            },
            Type::Namespace(Ns::Caps) => {
                let str_p = ParamType {
                    ty: Type::Str,
                    optional: false,
                    rest: false,
                };
                match name {
                    "has" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![str_p],
                        ret: Type::Bool,
                    })),
                    "list" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![],
                        ret: Type::Array(Rc::new(Type::Str)),
                    })),
                    "drop" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![str_p],
                        ret: Type::Void,
                    })),
                    _ => self.no_member("caps", name, pos),
                }
            }
            // Randomness is authority, not arithmetic: it seeds tokens and keys,
            // and it is an observable side channel. So it lives behind a
            // capability (§5.3) rather than in `math` where it would be reached
            // for without a thought.
            Type::Namespace(Ns::Random) => {
                let bytes_ty = match self.bytes_id {
                    Some(id) => Type::Class(id, Rc::new(vec![])),
                    None => Type::Err,
                };
                let i32t = Type::Int(IntKind::I32);
                let p = |ty: Type| ParamType {
                    ty,
                    optional: false,
                    rest: false,
                };
                match name {
                    // Uniform in [0, 1).
                    "float" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![],
                        ret: Type::F64,
                    })),
                    // Uniform in [lo, hi] — inclusive, and unbiased.
                    "int" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(Type::Int(IntKind::I64)), p(Type::Int(IntKind::I64))],
                        ret: Type::Int(IntKind::I64),
                    })),
                    "bytes" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![p(i32t)],
                        ret: bytes_ty,
                    })),
                    _ => self.no_member("random", name, pos),
                }
            }
            Type::Namespace(Ns::Json) => match name {
                "stringify" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        ty: Type::Unknown,
                        optional: false,
                        rest: false,
                    }],
                    ret: Type::Str,
                })),
                // Parsing gives back `any`: the shape of a JSON document is not
                // known until it is read, and pretending otherwise would be a
                // lie the checker cannot back up.
                "parse" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        ty: Type::Str,
                        optional: false,
                        rest: false,
                    }],
                    ret: Type::Unknown,
                })),
                _ => self.no_member("JSON", name, pos),
            },
            Type::Namespace(Ns::PromiseNs) => {
                let tv = self.fresh_tv("T");
                let t = Type::Var(tv);
                match name {
                    "resolve" => Type::Fn(Rc::new(FnType {
                        tparams: vec![tv],
                        params: vec![ParamType {
                            ty: t.clone(),
                            optional: false,
                            rest: false,
                        }],
                        ret: self.promise_of(t),
                    })),
                    "reject" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: Type::Unknown,
                            optional: false,
                            rest: false,
                        }],
                        ret: self.promise_of(Type::Unknown),
                    })),
                    // `Promise.all([…])` — the element type is not tracked
                    // through the array of promises yet.
                    "all" => Type::Fn(Rc::new(FnType {
                        tparams: vec![],
                        params: vec![ParamType {
                            ty: Type::Array(Rc::new(Type::Unknown)),
                            optional: false,
                            rest: false,
                        }],
                        ret: self.promise_of(Type::Array(Rc::new(Type::Unknown))),
                    })),
                    _ => self.no_member("Promise", name, pos),
                }
            }
            Type::Namespace(Ns::Console) => match name {
                "log" | "warn" | "error" | "info" | "debug" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        // Anything is assignable *to* `unknown`, which is exactly
                        // what "prints whatever you give it" means.
                        ty: Type::Unknown,
                        optional: false,
                        rest: true,
                    }],
                    ret: Type::Void,
                })),
                _ => self.no_member("console", name, pos),
            },
            Type::Namespace(Ns::Document) => match name {
                "getElementById" | "createElement" => Type::Fn(Rc::new(FnType {
                    tparams: vec![],
                    params: vec![ParamType {
                        ty: Type::Str,
                        optional: false,
                        rest: false,
                    }],
                    ret: Type::Class(self.element_id, Rc::new(vec![])),
                })),
                _ => self.no_member("document", name, pos),
            },
            Type::Namespace(Ns::Opaque) => Type::Unknown,
            // A bounded type parameter exposes its bound's members:
            // `<T extends Comparable<T>>` makes `t.compareTo(u)` legal.
            Type::Var(tv) => match self.tv_bounds.get(tv).cloned() {
                Some(bound) => self.member_access(&bound, name, false, pos),
                None => self.no_member(&self.show(&base), name, pos),
            },
            _ => self.no_member(&self.show(&base), name, pos),
        };
        if optional && matches!(ot, Type::Nullable(_)) {
            nullable(out)
        } else {
            out
        }
    }

    /// The host interface backing this class, if any (walks the chain).
    fn host_parent_of(&self, id: ClassId) -> Option<(IfaceId, Vec<Type>)> {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(hp) = &self.classes[cid].host_parent {
                return Some(hp.clone());
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        None
    }

    fn method_access(&self, id: ClassId, name: &str) -> Access {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(m) = self.classes[cid].methods.iter().find(|m| m.name == name) {
                return m.access;
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        Access::Public
    }

    /// Is this member readonly through the interface? A `readonly` property, or
    /// one declared with only a `get`.
    fn iface_member_readonly(&self, id: IfaceId, name: &str) -> Option<bool> {
        if let Some(m) = self.ifaces[id].members.iter().find(|m| m.name == name) {
            return Some(m.readonly);
        }
        self.ifaces[id]
            .extends
            .iter()
            .find_map(|(pid, _)| self.iface_member_readonly(*pid, name))
    }

    fn iface_member(&self, id: IfaceId, name: &str) -> Option<(Type, bool)> {
        if let Some(m) = self.ifaces[id].members.iter().find(|m| m.name == name) {
            return Some((m.ty.clone(), m.optional));
        }
        for (pid, _) in &self.ifaces[id].extends {
            if let Some(found) = self.iface_member(*pid, name) {
                return Some(found);
            }
        }
        None
    }

    fn no_member(&mut self, on: &str, name: &str, pos: Pos) -> Type {
        let msg = match name {
            "prototype" | "__proto__" => {
                format!("`{name}` does not exist: Mersey has no prototypes (§1.1, §4.1)")
            }
            "constructor" => "the constructor is not a reachable member (§4.1)".to_string(),
            _ => format!("no member `{name}` on `{on}`"),
        };
        self.error(Code::UnknownMember, msg, pos);
        Type::Err
    }

    fn check_assignable_target(&mut self, target: &Expr) {
        match target {
            Expr::Ident(_) | Expr::Index { .. } => {} // const-ness is the binder's E0304
            Expr::Member { obj, name, .. } => {
                // readonly + setter access checks.
                let ot = self.check_expr(obj, None);
                match strip_null(&ot) {
                    Type::Class(id, _) => {
                        if let Some((_, _access, readonly, owner)) = self.field_info(id, name) {
                            if readonly && !(self.in_ctor && self.current_class == Some(owner)) {
                                self.error(
                                    Code::ReadonlyViolation,
                                    format!(
                                        "`{name}` is readonly; it can only be assigned in the \
                                         constructor of `{}`",
                                        self.classes[owner].name
                                    ),
                                    pos_of(obj),
                                );
                            }
                        } else if let Some((_, access, owner)) = self.accessor_info(id, name, true)
                        {
                            self.check_access(
                                access,
                                owner,
                                pos_of(obj),
                                &format!("setter `{name}`"),
                            );
                        } else if self.accessor_info(id, name, false).is_some() {
                            self.error(
                                Code::ReadonlyViolation,
                                format!("`{name}` has no setter"),
                                pos_of(obj),
                            );
                        }
                    }
                    // The same rule through an interface: a member declared
                    // `readonly`, or with only a `get`, can be read and not
                    // written. Only class receivers were checked, so an interface
                    // was a way around the class's own rule.
                    Type::Iface(id, _) => {
                        if self.iface_member_readonly(id, name) == Some(true) {
                            self.error(
                                Code::ReadonlyViolation,
                                format!(
                                    "`{name}` is readonly on interface `{}`",
                                    self.ifaces[id].name
                                ),
                                pos_of(obj),
                            );
                        }
                    }
                    Type::Record(_) | Type::Err | Type::ClassMeta(_) => {}
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---- casts ---------------------------------------------------------------------

    fn check_cast(&mut self, from: &Type, to: &Type, wrapping: bool, pos: Pos) {
        // A cast to a non-nullable type is also the null assertion
        // (`document.body as Element`); nullability is checked at runtime.
        let f = match (strip_narrow_helpers(from), to) {
            (Type::Nullable(inner), t) if !matches!(t, Type::Nullable(_)) => inner.as_ref().clone(),
            (f, _) => f,
        };
        let t = strip_narrow_helpers(to);
        let numeric = |x: &Type| matches!(x, Type::Int(_) | Type::F32 | Type::F64 | Type::Char);
        if matches!(f, Type::Err) || matches!(t, Type::Err) {
            return;
        }
        if wrapping {
            if !(numeric(&f) && matches!(t, Type::Int(_))) {
                self.error(
                    Code::BadCast,
                    "`as wrapping` applies only to numeric-to-integer casts",
                    pos,
                );
            }
            return;
        }
        if numeric(&f) && numeric(&t) {
            return;
        }
        // Reference casts: up or down a class/interface chain.
        let ok = self.assignable(&f, &t) || self.assignable(&t, &f);
        if !ok {
            self.error(
                Code::BadCast,
                format!("cannot cast `{}` to `{}`", self.show(&f), self.show(&t)),
                pos,
            );
        }
    }

    // ---- assignability ---------------------------------------------------------------

    /// Check assignability *and* record the conversion the value needs.
    ///
    /// These are the same thing seen from two sides: the checker permits `7` to
    /// flow into a `float64` precisely because it knows how to widen it, and this
    /// is where it writes down that it must. Every place a value crosses into a
    /// context of a different numeric type goes through here.
    fn require_assignable_at(&mut self, e: &Expr, from: &Type, to: &Type, what: &str) {
        self.require_assignable(from, to, pos_of(e), what);
        self.coerce(e, from, to);
    }

    /// Record that `e`'s value must be converted to `to` before it is used.
    fn coerce(&mut self, e: &Expr, from: &Type, to: &Type) {
        self.coerce_key(node_id(e), from, to);
    }

    /// As `coerce`, for a value with no expression of its own: the `{ x }`
    /// shorthand, whose value comes from the binding `x` names.
    fn coerce_key(&mut self, key: usize, from: &Type, to: &Type) {
        let (Some(f), Some(t)) = (num_of(from), num_of(to)) else {
            return;
        };
        if f != t {
            self.coercions.insert(key, t);
        }
    }

    fn require_assignable(&mut self, from: &Type, to: &Type, pos: Pos, what: &str) {
        if !self.assignable(from, to) {
            self.error(
                Code::TypeMismatch,
                format!(
                    "{what}: `{}` is not assignable to `{}`",
                    self.show(from),
                    self.show(to)
                ),
                pos,
            );
        }
    }

    fn assignable(&self, from: &Type, to: &Type) -> bool {
        use Type::*;
        match (from, to) {
            // `Err` is poison: an error was already reported, so stay quiet.
            (Err, _) | (_, Err) => true,
            // Anything may be *given* to `unknown` — that is what makes it the
            // top type. Nothing comes back out without narrowing, which is the
            // difference between a top type and a hole.
            (_, Unknown) => true,
            (Unknown, _) => false,
            // Every number satisfies the `Numeric` bound, and nothing else does.
            (Int(_) | F32 | F64 | BigInt | BigDec, Iface(id, _))
                if Some(*id) == self.numeric_id =>
            {
                true
            }
            (Void, Void) => true,
            (Null, Null | Nullable(_)) => true,
            (Nullable(a), Nullable(b)) => self.assignable(a, b),
            (t, Nullable(b)) => self.assignable(t, b),
            (Bool, Bool) | (Char, Char) | (Str, Str) | (BigInt, BigInt) | (BigDec, BigDec) => true,
            (Int(a), Int(b)) =>
            // Implicit widening only (§3.3 rule 3): same signedness and
            // wider, or signed-to-wider-signed from unsigned.
            {
                a == b
                    || (a.signed() == b.signed() && a.bits() < b.bits())
                    || (!a.signed() && b.signed() && a.bits() < b.bits())
            }
            (Int(_), F32 | F64) => true,
            (F32, F32) | (F64, F64) | (F32, F64) => true,
            (Array(a), Array(b)) => self.ty_eq(a, b) || matches!(b.as_ref(), Type::Unknown),
            (Tuple(a), Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.assignable(x, y))
            }
            (Record(a), Record(b)) => {
                b.iter()
                    .all(|bf| match a.iter().find(|af| af.name == bf.name) {
                        Some(af) => self.assignable(&af.ty, &bf.ty),
                        None => bf.optional,
                    })
            }
            (Class(a, aargs), Record(b)) => {
                // Classes satisfy record types structurally via public fields.
                let _ = aargs;
                b.iter().all(|bf| {
                    self.field_info(*a, &bf.name)
                        .map(|(t, acc, ..)| acc == Access::Public && self.assignable(&t, &bf.ty))
                        .unwrap_or(bf.optional)
                })
            }
            (Fn(a), Fn(b)) => {
                // The source may accept FEWER parameters (extra call-site
                // arguments are ignored) — the callback convention.
                a.params.len() <= b.params.len()
                    && a.params
                        .iter()
                        .zip(b.params.iter())
                        .all(|(x, y)| self.assignable(&y.ty, &x.ty))
                    // A target that returns `void` discards the result, so the
                    // source may return whatever it likes.
                    && (matches!(b.ret, Void) || self.assignable(&a.ret, &b.ret))
            }
            (Class(a, aargs), Class(b, bargs)) => {
                let mut cur = Some((*a, aargs.as_ref().clone()));
                while let Some((cid, cargs)) = cur {
                    if cid == *b {
                        return cargs.len() == bargs.len()
                            && cargs
                                .iter()
                                .zip(bargs.iter())
                                .all(|(x, y)| self.ty_eq(x, y));
                    }
                    let map = self.subst_map(cid, &cargs);
                    cur = self.classes[cid]
                        .parent
                        .as_ref()
                        .map(|(p, pa)| (*p, pa.iter().map(|t| subst(t, &map)).collect()));
                }
                false
            }
            (Class(a, aargs), Iface(b, bargs)) => {
                // A host-backed class IS its host interface.
                if let Some((iid, iargs)) = self.host_parent_of(*a) {
                    if self.iface_extends(iid, &iargs, *b, bargs) {
                        return true;
                    }
                }
                let mut cur = Some((*a, aargs.as_ref().clone()));
                while let Some((cid, cargs)) = cur {
                    let map = self.subst_map(cid, &cargs);
                    for (iid, iargs) in &self.classes[cid].ifaces {
                        let ia: Vec<Type> = iargs.iter().map(|t| subst(t, &map)).collect();
                        if self.iface_extends(*iid, &ia, *b, bargs) {
                            return true;
                        }
                    }
                    cur = self.classes[cid]
                        .parent
                        .as_ref()
                        .map(|(p, pa)| (*p, pa.iter().map(|t| subst(t, &map)).collect()));
                }
                false
            }
            (Iface(a, aargs), Iface(b, bargs)) => self.iface_extends(*a, aargs, *b, bargs),
            (Enum(a), Enum(b)) => a == b,
            (Union(arms), t) => arms.iter().all(|a| self.assignable(a, t)),
            (t, Union(arms)) => arms.iter().any(|a| self.assignable(t, a)),
            (Var(a), Var(b)) => a == b,
            (ClassMeta(a), ClassMeta(b)) => a == b,
            _ => false,
        }
    }

    fn iface_extends(&self, a: IfaceId, aargs: &[Type], b: IfaceId, bargs: &[Type]) -> bool {
        if a == b {
            return aargs.len() == bargs.len()
                && aargs
                    .iter()
                    .zip(bargs.iter())
                    .all(|(x, y)| self.ty_eq(x, y));
        }
        let map: HashMap<TvId, Type> = self.ifaces[a]
            .tparams
            .iter()
            .copied()
            .zip(aargs.iter().cloned())
            .collect();
        for (pid, pargs) in &self.ifaces[a].extends {
            let pa: Vec<Type> = pargs.iter().map(|t| subst(t, &map)).collect();
            if self.iface_extends(*pid, &pa, b, bargs) {
                return true;
            }
        }
        false
    }

    fn ty_eq(&self, a: &Type, b: &Type) -> bool {
        self.assignable(a, b) && self.assignable(b, a)
    }

    fn unify_pair(&mut self, a: Type, b: Type) -> Type {
        if self.assignable(&b, &a) {
            return a;
        }
        if self.assignable(&a, &b) {
            return b;
        }
        if matches!(a, Type::Null) {
            return nullable(b);
        }
        if matches!(b, Type::Null) {
            return nullable(a);
        }
        if let Some(c) = self.numeric_common(&a, &b) {
            return c;
        }
        fold_union(vec![a, b])
    }
}

// ---- free helpers ------------------------------------------------------------------

/// Value of an unsuffixed integer literal (possibly negated/parenthesized).
fn unsuffixed_int_value(e: &Expr) -> Option<i128> {
    match e {
        Expr::Lit {
            kind: LitKind::Int,
            text,
            ..
        } => {
            let t = text.replace('_', "");
            if t.ends_with(|c: char| c.is_ascii_alphabetic()) && !t.starts_with("0x") {
                return None; // suffixed: keeps its own type
            }
            let (radix, body) = if let Some(b) = t.strip_prefix("0x") {
                if b.ends_with(|c: char| !c.is_ascii_hexdigit()) {
                    return None;
                }
                (16, b)
            } else if let Some(b) = t.strip_prefix("0o") {
                (8, b)
            } else if let Some(b) = t.strip_prefix("0b") {
                (2, b)
            } else {
                (10, t.as_str())
            };
            i128::from_str_radix(body, radix).ok()
        }
        Expr::Paren(inner) => unsuffixed_int_value(inner),
        Expr::Unary {
            op: UnaryOp::Neg,
            expr,
            ..
        } => unsuffixed_int_value(expr).map(|v| -v),
        _ => None,
    }
}

fn int_fits(v: i128, k: IntKind) -> bool {
    use IntKind::*;
    match k {
        I8 => i8::try_from(v).is_ok(),
        I16 => i16::try_from(v).is_ok(),
        I32 => i32::try_from(v).is_ok(),
        I64 => i64::try_from(v).is_ok(),
        U8 => u8::try_from(v).is_ok(),
        U16 => u16::try_from(v).is_ok(),
        U32 => u32::try_from(v).is_ok(),
        U64 => u64::try_from(v).is_ok(),
    }
}

/// Does this body contain a `yield`? Nested arrows are separate functions,
/// so a `yield` inside one belongs to *that* function.
/// Does anything here create a closure?
///
/// A `for (let i = …)` loop is specified to give each iteration its own `i`,
/// but that is only *observable* if something captures it — an arrow function
/// is the only construct that can (declarations are module-level, §6.7). When
/// nothing does, the engines skip the per-iteration scope entirely, which
/// keeps an ordinary counted loop a counted loop: no scope allocation per
/// iteration, and still inside the JIT's subset.
pub fn makes_closure(body: &[Stmt]) -> bool {
    body.iter().any(stmt_makes_closure)
}

/// Does this expression create a closure?
pub fn expr_makes_closure(e: &Expr) -> bool {
    match e {
        Expr::Arrow { .. } => true,
        Expr::Paren(i)
        | Expr::Unary { expr: i, .. }
        | Expr::Update { expr: i, .. }
        | Expr::Cast { expr: i, .. }
        | Expr::Is { expr: i, .. }
        | Expr::ImportCall(i) => expr_makes_closure(i),
        Expr::Binary { l, r, .. }
        | Expr::Assign {
            target: l,
            value: r,
            ..
        } => expr_makes_closure(l) || expr_makes_closure(r),
        Expr::Cond { cond, then, els } => {
            expr_makes_closure(cond) || expr_makes_closure(then) || expr_makes_closure(els)
        }
        Expr::Call { callee, args, .. } => {
            expr_makes_closure(callee) || args.iter().any(|a| expr_makes_closure(&a.expr))
        }
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            args.iter().any(|a| expr_makes_closure(&a.expr))
        }
        Expr::Member { obj, .. } => expr_makes_closure(obj),
        Expr::Index { obj, index, .. } => expr_makes_closure(obj) || expr_makes_closure(index),
        Expr::Array(items) => items.iter().any(|a| expr_makes_closure(&a.expr)),
        Expr::Record(fields) => fields.iter().any(|f| match f {
            RecordField::Named { value: Some(v), .. } => expr_makes_closure(v),
            RecordField::Spread(v) => expr_makes_closure(v),
            _ => false,
        }),
        Expr::Template(parts) => parts.iter().any(|p| match p {
            TplPart::Expr(e) => expr_makes_closure(e),
            _ => false,
        }),
        _ => false,
    }
}

/// Does this statement create a closure?
pub fn stmt_makes_closure(s: &Stmt) -> bool {
    match s {
        Stmt::Expr(e) | Stmt::Throw(e) => expr_makes_closure(e),
        Stmt::Return { value: Some(e), .. } => expr_makes_closure(e),
        Stmt::Var(v) => v
            .bindings
            .iter()
            .any(|b| b.init.as_ref().is_some_and(expr_makes_closure)),
        Stmt::Block(b) => b.iter().any(stmt_makes_closure),
        Stmt::If { cond, then, els } => {
            expr_makes_closure(cond)
                || stmt_makes_closure(then)
                || els.as_ref().is_some_and(|e| stmt_makes_closure(e))
        }
        Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
            expr_makes_closure(cond) || stmt_makes_closure(body)
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            cond.as_ref().is_some_and(expr_makes_closure)
                || step.iter().any(expr_makes_closure)
                || stmt_makes_closure(body)
                || matches!(init, Some(ForInit::Exprs(es)) if es.iter().any(expr_makes_closure))
        }
        Stmt::ForOf { iter, body, .. } => expr_makes_closure(iter) || stmt_makes_closure(body),
        Stmt::Switch { scrutinee, clauses } => {
            expr_makes_closure(scrutinee)
                || clauses
                    .iter()
                    .any(|c| c.body.iter().any(stmt_makes_closure))
        }
        Stmt::Try {
            block,
            catches,
            finally,
        } => {
            block.iter().any(stmt_makes_closure)
                || catches
                    .iter()
                    .any(|c| c.block.iter().any(stmt_makes_closure))
                || finally
                    .as_ref()
                    .is_some_and(|f| f.iter().any(stmt_makes_closure))
        }
        Stmt::Labeled { body, .. } => stmt_makes_closure(body),
        _ => false,
    }
}

pub(crate) fn body_yields(body: &[Stmt]) -> bool {
    fn in_expr(e: &Expr) -> bool {
        match e {
            Expr::Yield { .. } => true,
            Expr::Arrow { .. } => false, // its own function
            Expr::Paren(i)
            | Expr::Unary { expr: i, .. }
            | Expr::Update { expr: i, .. }
            | Expr::Cast { expr: i, .. }
            | Expr::Is { expr: i, .. }
            | Expr::ImportCall(i) => in_expr(i),
            Expr::Binary { l, r, .. }
            | Expr::Assign {
                target: l,
                value: r,
                ..
            } => in_expr(l) || in_expr(r),
            Expr::Cond { cond, then, els } => in_expr(cond) || in_expr(then) || in_expr(els),
            Expr::Call { callee, args, .. } => {
                in_expr(callee) || args.iter().any(|a| in_expr(&a.expr))
            }
            Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
                args.iter().any(|a| in_expr(&a.expr))
            }
            Expr::Member { obj, .. } => in_expr(obj),
            Expr::Index { obj, index, .. } => in_expr(obj) || in_expr(index),
            Expr::Array(items) => items.iter().any(|a| in_expr(&a.expr)),
            Expr::Record(fields) => fields.iter().any(|f| match f {
                RecordField::Named { value: Some(v), .. } => in_expr(v),
                RecordField::Spread(v) => in_expr(v),
                _ => false,
            }),
            Expr::Template(parts) => parts.iter().any(|p| match p {
                TplPart::Expr(e) => in_expr(e),
                _ => false,
            }),
            _ => false,
        }
    }
    fn in_stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Expr(e) | Stmt::Throw(e) => in_expr(e),
            Stmt::Return { value: Some(e), .. } => in_expr(e),
            Stmt::Var(v) => v
                .bindings
                .iter()
                .any(|b| b.init.as_ref().is_some_and(in_expr)),
            Stmt::Block(b) => b.iter().any(in_stmt),
            Stmt::If { cond, then, els } => {
                in_expr(cond) || in_stmt(then) || els.as_ref().is_some_and(|e| in_stmt(e))
            }
            Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
                in_expr(cond) || in_stmt(body)
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                cond.as_ref().is_some_and(in_expr)
                    || step.iter().any(in_expr)
                    || in_stmt(body)
                    || matches!(init, Some(ForInit::Exprs(es)) if es.iter().any(in_expr))
            }
            Stmt::ForOf { iter, body, .. } => in_expr(iter) || in_stmt(body),
            Stmt::Switch { scrutinee, clauses } => {
                in_expr(scrutinee) || clauses.iter().any(|c| c.body.iter().any(in_stmt))
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                block.iter().any(in_stmt)
                    || catches.iter().any(|c| c.block.iter().any(in_stmt))
                    || finally.as_ref().is_some_and(|f| f.iter().any(in_stmt))
            }
            Stmt::Labeled { body, .. } => in_stmt(body),
            _ => false,
        }
    }
    body.iter().any(in_stmt)
}

fn nullable(t: Type) -> Type {
    match t {
        Type::Nullable(_) | Type::Null | Type::Err => t,
        other => Type::Nullable(Rc::new(other)),
    }
}

fn strip_null(t: &Type) -> Type {
    match t {
        Type::Nullable(inner) => inner.as_ref().clone(),
        other => other.clone(),
    }
}

fn strip_narrow_helpers(t: &Type) -> Type {
    t.clone()
}

fn fold_union(tys: Vec<Type>) -> Type {
    let mut has_null = false;
    let mut arms: Vec<Type> = Vec::new();
    for t in tys {
        match t {
            Type::Null => has_null = true,
            Type::Nullable(inner) => {
                has_null = true;
                arms.push(inner.as_ref().clone());
            }
            other => arms.push(other),
        }
    }
    let base = if arms.len() == 1 {
        arms.pop().unwrap()
    } else {
        Type::Union(Rc::new(arms))
    };
    if has_null {
        nullable(base)
    } else {
        base
    }
}

fn to_string_fn() -> Type {
    Type::Fn(Rc::new(FnType {
        tparams: vec![],
        params: vec![],
        ret: Type::Str,
    }))
}

fn subst(t: &Type, map: &HashMap<TvId, Type>) -> Type {
    if map.is_empty() {
        return t.clone();
    }
    match t {
        Type::Var(tv) => map.get(tv).cloned().unwrap_or(Type::Var(*tv)),
        Type::Nullable(inner) => nullable(subst(inner, map)),
        Type::Array(e) => Type::Array(Rc::new(subst(e, map))),
        Type::Tuple(ts) => Type::Tuple(Rc::new(ts.iter().map(|t| subst(t, map)).collect())),
        Type::Record(fs) => Type::Record(Rc::new(
            fs.iter()
                .map(|f| RecField {
                    name: f.name.clone(),
                    ty: subst(&f.ty, map),
                    optional: f.optional,
                })
                .collect(),
        )),
        Type::Fn(f) => Type::Fn(Rc::new(FnType {
            tparams: f.tparams.clone(),
            params: f
                .params
                .iter()
                .map(|p| ParamType {
                    ty: subst(&p.ty, map),
                    ..p.clone()
                })
                .collect(),
            ret: subst(&f.ret, map),
        })),
        Type::Class(id, args) => {
            Type::Class(*id, Rc::new(args.iter().map(|a| subst(a, map)).collect()))
        }
        Type::Iface(id, args) => {
            Type::Iface(*id, Rc::new(args.iter().map(|a| subst(a, map)).collect()))
        }
        Type::Union(arms) => fold_union(arms.iter().map(|a| subst(a, map)).collect()),
        other => other.clone(),
    }
}

/// One-pass inference: bind free `tparams` in `want` from `got`.
fn unify_infer(want: &Type, got: &Type, tparams: &[TvId], out: &mut HashMap<TvId, Type>) {
    match (want, got) {
        // Never bind a parameter to itself (or to another still-free
        // parameter): that would "infer" `U := U` and poison the call.
        (Type::Var(tv), Type::Var(g)) if tparams.contains(tv) && tparams.contains(g) => {}
        (Type::Var(tv), g) if tparams.contains(tv) => {
            out.entry(*tv).or_insert_with(|| g.clone());
        }
        (Type::Nullable(a), Type::Nullable(b)) => unify_infer(a, b, tparams, out),
        (Type::Nullable(a), b) => unify_infer(a, b, tparams, out),
        (Type::Array(a), Type::Array(b)) => unify_infer(a, b, tparams, out),
        (Type::Tuple(a), Type::Tuple(b)) => {
            for (x, y) in a.iter().zip(b.iter()) {
                unify_infer(x, y, tparams, out);
            }
        }
        (Type::Fn(a), Type::Fn(b)) => {
            for (x, y) in a.params.iter().zip(b.params.iter()) {
                unify_infer(&x.ty, &y.ty, tparams, out);
            }
            unify_infer(&a.ret, &b.ret, tparams, out);
        }
        (Type::Class(i, a), Type::Class(j, b)) | (Type::Iface(i, a), Type::Iface(j, b))
            if i == j =>
        {
            for (x, y) in a.iter().zip(b.iter()) {
                unify_infer(x, y, tparams, out);
            }
        }
        _ => {}
    }
}

/// Does this statement always leave — return, throw, break, continue?
///
/// Only that makes a guard clause a guard: if the body might fall through, the
/// code after it is reachable both ways and nothing can be concluded.
fn always_diverges(s: &Stmt) -> bool {
    match s {
        Stmt::Return { .. } | Stmt::Throw(_) | Stmt::Break { .. } | Stmt::Continue { .. } => true,
        // A block leaves if any statement in it does (the ones after are dead).
        Stmt::Block(b) => b.iter().any(always_diverges),
        // Both ways out, or it is not a certainty.
        Stmt::If {
            then, els: Some(e), ..
        } => always_diverges(then) && always_diverges(e),
        _ => false,
    }
}

// ---- position helpers ------------------------------------------------------------

pub(crate) fn pos_of(e: &Expr) -> Pos {
    match e {
        Expr::Ident(n) => n.pos,
        Expr::This(p) => *p,
        Expr::Unary { pos, .. } => *pos,
        Expr::SuperMember { pos, .. } | Expr::SuperCall { pos, .. } => *pos,
        Expr::Paren(inner) | Expr::ImportCall(inner) | Expr::Is { expr: inner, .. } => {
            pos_of(inner)
        }
        Expr::Update { expr, .. } | Expr::Cast { expr, .. } => pos_of(expr),
        Expr::Binary { l, .. } | Expr::Assign { target: l, .. } | Expr::Cond { cond: l, .. } => {
            pos_of(l)
        }
        Expr::Call { callee, .. } => pos_of(callee),
        Expr::Member { obj, .. } | Expr::Index { obj, .. } => pos_of(obj),
        Expr::New { ty, .. } => type_pos(ty),
        Expr::Template(parts) => parts
            .iter()
            .find_map(|p| match p {
                TplPart::Expr(e) => Some(pos_of(e)),
                _ => None,
            })
            .unwrap_or(Pos { line: 0, col: 0 }),
        Expr::Array(elems) => elems
            .first()
            .map(|e| pos_of(&e.expr))
            .unwrap_or(Pos { line: 0, col: 0 }),
        Expr::Record(fs) => fs
            .iter()
            .find_map(|f| match f {
                RecordField::Named { name, .. } => Some(name.pos),
                RecordField::Spread(e) => Some(pos_of(e)),
            })
            .unwrap_or(Pos { line: 0, col: 0 }),
        Expr::Arrow { params, .. } => params
            .first()
            .map(|p| pattern_pos(&p.target))
            .unwrap_or(Pos { line: 0, col: 0 }),
        Expr::Lit { pos, .. } => *pos,
        Expr::Yield { pos, .. } => *pos,
    }
}

fn pattern_pos(p: &Pattern) -> Pos {
    match p {
        Pattern::Name(n) => n.pos,
        Pattern::Array { elems, rest } => elems
            .first()
            .map(|e| pattern_pos(&e.target))
            .or_else(|| rest.as_ref().map(|r| pattern_pos(r)))
            .unwrap_or(Pos { line: 0, col: 0 }),
        Pattern::Record(fs) => fs
            .first()
            .map(|f| f.name.pos)
            .unwrap_or(Pos { line: 0, col: 0 }),
    }
}

fn type_pos(t: &ast::TypeExpr) -> Pos {
    match t {
        ast::TypeExpr::Named { pos, .. } => *pos,
        ast::TypeExpr::Nullable(inner) | ast::TypeExpr::ArrayOf(inner) => type_pos(inner),
        ast::TypeExpr::Union(arms) => arms
            .first()
            .map(type_pos)
            .unwrap_or(Pos { line: 0, col: 0 }),
        ast::TypeExpr::Tuple(ts) => ts.first().map(type_pos).unwrap_or(Pos { line: 0, col: 0 }),
        ast::TypeExpr::Record(_) => Pos { line: 0, col: 0 },
        ast::TypeExpr::Function { ret, .. } => type_pos(ret),
    }
}

fn compound_op(op: &str) -> BinOp {
    match op {
        "+=" => BinOp::Add,
        "-=" => BinOp::Sub,
        "*=" => BinOp::Mul,
        "/=" => BinOp::Div,
        "%=" => BinOp::Rem,
        "**=" => BinOp::Pow,
        "<<=" => BinOp::Shl,
        ">>=" => BinOp::Shr,
        "&=" => BinOp::BitAnd,
        "|=" => BinOp::BitOr,
        "^=" => BinOp::BitXor,
        "&&=" => BinOp::And,
        "||=" => BinOp::Or,
        _ => BinOp::Coalesce,
    }
}
