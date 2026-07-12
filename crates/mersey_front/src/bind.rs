//! Binder: name resolution and scope analysis (Phase 1, pre-typechecking).
//!
//! Responsibilities:
//! - block scoping with temporal dead zone for `let`/`const` (E0303);
//! - undefined/duplicate names (E0301/E0302), in separate value and type
//!   namespaces;
//! - `const` assignment (E0304), labels (E0305), `await` (E0306),
//!   `this`/`super` contexts (E0307), type references (E0308),
//!   `return` placement (E0309), `break`/`continue` placement (E0310).
//!
//! Module-level declarations (functions, classes, interfaces, enums, type
//! aliases, imports) are order-independent: they are hoisted and usable
//! anywhere in the module. Module-level `let`/`const` follow textual order
//! (TDZ), exactly like locals.
//!
//! Imports are bound as opaque symbols; cross-module validation arrives
//! with the module-graph loader.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::{Code, Diagnostic, Pos};

pub struct BindOutput {
    pub diagnostics: Vec<Diagnostic>,
}

/// Names every module sees without importing: the built-in error classes
/// (spec §4.6 — only `Error` subclasses may be thrown).
const PRELUDE_CLASSES: &[&str] = &[
    "Error",
    "RangeError",
    "TypeError",
    "Map",
    "Set",
    "Element",
    "Bytes",
    "Regex",
    "Iter",
];

pub fn bind(module: &Module) -> BindOutput {
    let mut prelude = Scope::default();
    for name in PRELUDE_CLASSES {
        let pos = Pos { line: 0, col: 0 };
        prelude.values.insert(
            name.to_string(),
            VSym {
                kind: VKind::Class,
                tdz: false,
                pos,
            },
        );
        prelude.types.insert(
            name.to_string(),
            TSym {
                kind: TKind::Class,
                pos,
            },
        );
    }
    // Ambient web-platform TYPE names (spec §5.4: types are ambient,
    // values require an import).
    for name in &crate::webapi::webapi().type_names {
        prelude.types.entry(name.clone()).or_insert(TSym {
            kind: TKind::Class,
            pos: Pos { line: 0, col: 0 },
        });
    }
    let mut b = Binder {
        scopes: vec![prelude, Scope::default()],
        diags: Vec::new(),
        ctx: Ctx {
            in_function: false,
            in_async: false,
            class: ClassCtx::None,
        },
        labels: Vec::new(),
        loop_depth: 0,
        switch_depth: 0,
    };
    b.bind_module(module);
    let mut diagnostics = b.diags;
    // The hoisting pass reports duplicates before the in-order walk runs;
    // present everything in source order.
    diagnostics.sort_by_key(|d| (d.pos.line, d.pos.col));
    BindOutput { diagnostics }
}

#[derive(Clone, Copy, PartialEq)]
enum VKind {
    Var { is_const: bool },
    Func,
    Class,
    Enum,
    Import,
    Namespace,
}

#[derive(Clone, Copy)]
struct VSym {
    kind: VKind,
    tdz: bool,
    pos: Pos,
}

#[derive(Clone, Copy)]
enum TKind {
    Class,
    Interface,
    Enum,
    Alias,
    TypeParam,
    Import,
}

#[derive(Clone, Copy)]
struct TSym {
    #[allow(dead_code)] // will be used by "defined here" notes
    kind: TKind,
    pos: Pos,
}

#[derive(Default)]
struct Scope {
    values: HashMap<String, VSym>,
    types: HashMap<String, TSym>,
}

#[derive(Clone, Copy)]
enum ClassCtx {
    None,
    InClass { has_super: bool, in_ctor: bool },
}

#[derive(Clone, Copy)]
struct Ctx {
    in_function: bool,
    in_async: bool,
    class: ClassCtx,
}

struct Binder {
    scopes: Vec<Scope>,
    diags: Vec<Diagnostic>,
    ctx: Ctx,
    labels: Vec<String>,
    loop_depth: u32,
    switch_depth: u32,
}

const PREDEFINED_TYPES: &[&str] = &[
    "bool", "char", "string", "bigint", "bigdec", "void", "int", "int8", "int16", "int32", "int64",
    "uint", "uint8", "uint16", "uint32", "uint64", "float", "float32", "float64",
];

impl Binder {
    fn error(&mut self, code: Code, msg: impl Into<String>, pos: Pos) {
        self.diags.push(Diagnostic::error(code, msg, pos));
    }

    // ---- scope plumbing ----------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare_value(&mut self, name: &Name, kind: VKind, tdz: bool) {
        let scope = self.scopes.last_mut().expect("scope");
        if let Some(prev) = scope.values.get(&name.text) {
            let msg = format!(
                "duplicate declaration of `{}` (first declared at {}:{})",
                name.text, prev.pos.line, prev.pos.col
            );
            let pos = name.pos;
            self.error(Code::DuplicateDeclaration, msg, pos);
            return;
        }
        scope.values.insert(
            name.text.clone(),
            VSym {
                kind,
                tdz,
                pos: name.pos,
            },
        );
    }

    fn declare_type(&mut self, name: &Name, kind: TKind) {
        let scope = self.scopes.last_mut().expect("scope");
        if let Some(prev) = scope.types.get(&name.text) {
            let msg = format!(
                "duplicate declaration of type `{}` (first declared at {}:{})",
                name.text, prev.pos.line, prev.pos.col
            );
            let pos = name.pos;
            self.error(Code::DuplicateDeclaration, msg, pos);
            return;
        }
        scope.types.insert(
            name.text.clone(),
            TSym {
                kind,
                pos: name.pos,
            },
        );
    }

    /// Flip a pre-registered `let`/`const` out of its TDZ once its
    /// declaration statement is reached.
    fn mark_declared(&mut self, name: &str) {
        if let Some(sym) = self.scopes.last_mut().and_then(|s| s.values.get_mut(name)) {
            sym.tdz = false;
        }
    }

    fn resolve_value(&mut self, name: &Name) -> Option<VSym> {
        for scope in self.scopes.iter().rev() {
            if let Some(sym) = scope.values.get(&name.text) {
                if sym.tdz {
                    let msg = format!(
                        "`{}` is used before its declaration at {}:{} (temporal dead zone)",
                        name.text, sym.pos.line, sym.pos.col
                    );
                    let (sym, pos) = (*sym, name.pos);
                    self.error(Code::UseBeforeDeclaration, msg, pos);
                    return Some(sym);
                }
                return Some(*sym);
            }
        }
        // Targeted messages for the JS constructs Mersey removed (§1.1).
        let msg = match name.text.as_str() {
            "undefined" => {
                "there is no `undefined`; the null type is `T?` with `null` (§3.2)".into()
            }
            "eval" => "there is no `eval`; no string ever becomes code (§5.1)".into(),
            "arguments" => {
                "there is no `arguments`; use a rest parameter `...args: T[]` (§4.4)".into()
            }
            "require" => "use `import { … } from \"…\";` (§4.5)".into(),
            "globalThis" | "window" => {
                "there is no global object; import what you need (§5.4)".into()
            }
            "NaN" | "Infinity" => {
                "float constants live in the standard library, not the global scope".into()
            }
            _ => format!("cannot find name `{}`", name.text),
        };
        let pos = name.pos;
        self.error(Code::UndefinedName, msg, pos);
        None
    }

    fn type_exists(&self, text: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.types.contains_key(text))
    }

    fn value_exists(&self, text: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|s| s.values.contains_key(text))
    }

    // ---- module ------------------------------------------------------------

    fn bind_module(&mut self, m: &Module) {
        // Pass 1: hoist module-level declarations and imports; pre-register
        // module-level let/const in their TDZ.
        for item in &m.items {
            match item {
                Item::Import(im) => self.hoist_import(im),
                Item::Decl(d) => self.hoist_decl(d),
                Item::Export(ex) => match &ex.kind {
                    ExportKind::Decl(d) => self.hoist_decl(d),
                    ExportKind::Var(v) => self.preregister_var(v),
                    ExportKind::Named { .. } => {}
                },
                Item::Stmt(Stmt::Var(v)) => self.preregister_var(v),
                Item::Stmt(_) => {}
            }
        }
        // Pass 2: walk in order.
        let mut reexport_checks: Vec<Name> = Vec::new();
        for item in &m.items {
            match item {
                Item::Import(_) => {}
                Item::Decl(d) => self.bind_decl(d),
                Item::Export(ex) => match &ex.kind {
                    ExportKind::Decl(d) => self.bind_decl(d),
                    ExportKind::Var(v) => self.bind_var_stmt(v),
                    ExportKind::Named { specs, from } => {
                        if from.is_none() {
                            for s in specs {
                                reexport_checks.push(s.name.clone());
                            }
                        }
                    }
                },
                Item::Stmt(s) => self.bind_stmt(s),
            }
        }
        // `export { x }` resolves against the whole module (hoisting), so
        // check at the end, in either namespace.
        for name in reexport_checks {
            if !self.value_exists(&name.text) && !self.type_exists(&name.text) {
                let msg = format!("cannot export `{}`: no such name in this module", name.text);
                self.error(Code::UndefinedName, msg, name.pos);
            }
        }
    }

    fn hoist_import(&mut self, im: &ImportDecl) {
        match &im.clause {
            None => {}
            Some(ImportClause::Namespace(n)) => {
                self.declare_value(n, VKind::Namespace, false);
            }
            Some(ImportClause::Named(specs)) => {
                for s in specs {
                    let local = s.alias.as_ref().unwrap_or(&s.name);
                    // An import may be a value, a type, or both; bind in
                    // both namespaces until the module graph can say.
                    self.declare_value(local, VKind::Import, false);
                    self.declare_type(local, TKind::Import);
                }
            }
        }
    }

    fn hoist_decl(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => self.declare_value(&f.name, VKind::Func, false),
            Decl::Class(c) => {
                self.declare_value(&c.name, VKind::Class, false);
                self.declare_type(&c.name, TKind::Class);
            }
            Decl::Interface(i) => self.declare_type(&i.name, TKind::Interface),
            Decl::Enum(e) => {
                self.declare_value(&e.name, VKind::Enum, false);
                self.declare_type(&e.name, TKind::Enum);
            }
            Decl::TypeAlias(t) => self.declare_type(&t.name, TKind::Alias),
        }
    }

    fn preregister_var(&mut self, v: &VarStmt) {
        let is_const = v.kind == VarKind::Const;
        for b in &v.bindings {
            self.preregister_pattern(&b.target, is_const);
        }
    }

    fn preregister_pattern(&mut self, p: &Pattern, is_const: bool) {
        match p {
            Pattern::Name(n) => self.declare_value(n, VKind::Var { is_const }, true),
            Pattern::Array { elems, rest } => {
                for e in elems {
                    self.preregister_pattern(&e.target, is_const);
                }
                if let Some(r) = rest {
                    self.preregister_pattern(r, is_const);
                }
            }
            Pattern::Record(fields) => {
                for f in fields {
                    match &f.target {
                        Some(t) => self.preregister_pattern(t, is_const),
                        None => self.declare_value(&f.name, VKind::Var { is_const }, true),
                    }
                }
            }
        }
    }

    fn pattern_names<'a>(p: &'a Pattern, out: &mut Vec<&'a Name>) {
        match p {
            Pattern::Name(n) => out.push(n),
            Pattern::Array { elems, rest } => {
                for e in elems {
                    Self::pattern_names(&e.target, out);
                }
                if let Some(r) = rest {
                    Self::pattern_names(r, out);
                }
            }
            Pattern::Record(fields) => {
                for f in fields {
                    match &f.target {
                        Some(t) => Self::pattern_names(t, out),
                        None => out.push(&f.name),
                    }
                }
            }
        }
    }

    // ---- declarations --------------------------------------------------------

    fn bind_decl(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => self.bind_fn_body(
                &f.type_params,
                &f.params,
                f.ret.as_ref(),
                &f.body,
                Ctx {
                    in_function: true,
                    in_async: f.is_async,
                    class: ClassCtx::None,
                },
            ),
            Decl::Class(c) => self.bind_class(c),
            Decl::Interface(i) => {
                self.push_scope();
                self.bind_type_params(&i.type_params);
                for e in &i.extends {
                    self.bind_type(e);
                }
                for m in &i.members {
                    match m {
                        InterfaceMember::Prop { ty, .. } => self.bind_type(ty),
                        InterfaceMember::Method {
                            type_params,
                            params,
                            ret,
                            ..
                        } => {
                            self.push_scope();
                            self.bind_type_params(type_params);
                            for p in params {
                                if let Some(t) = &p.ty {
                                    self.bind_type(t);
                                }
                            }
                            self.bind_type(ret);
                            self.pop_scope();
                        }
                    }
                }
                self.pop_scope();
            }
            Decl::Enum(e) => {
                if let Some(b) = &e.backing {
                    const INT_TYPES: &[&str] = &[
                        "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16",
                        "uint32", "uint64",
                    ];
                    if !INT_TYPES.contains(&b.text.as_str()) {
                        let msg = format!(
                            "enum backing type must be an integer type, not `{}`",
                            b.text
                        );
                        self.error(Code::UnknownTypeName, msg, b.pos);
                    }
                }
                let mut seen: HashMap<&str, Pos> = HashMap::new();
                for (n, init) in &e.members {
                    if let Some(prev) = seen.get(n.text.as_str()) {
                        let msg = format!(
                            "duplicate enum member `{}` (first declared at {}:{})",
                            n.text, prev.line, prev.col
                        );
                        self.error(Code::DuplicateDeclaration, msg, n.pos);
                    } else {
                        seen.insert(&n.text, n.pos);
                    }
                    if let Some(init) = init {
                        self.bind_expr(init);
                    }
                }
            }
            Decl::TypeAlias(t) => {
                self.push_scope();
                self.bind_type_params(&t.type_params);
                self.bind_type(&t.ty);
                self.pop_scope();
            }
        }
    }

    fn bind_type_params(&mut self, tps: &[TypeParam]) {
        for tp in tps {
            self.declare_type(&tp.name, TKind::TypeParam);
        }
        for tp in tps {
            if let Some(c) = &tp.constraint {
                self.bind_type(c);
            }
        }
    }

    fn bind_class(&mut self, c: &ClassDecl) {
        self.push_scope();
        self.bind_type_params(&c.type_params);
        if let Some(e) = &c.extends {
            self.bind_type(e);
        }
        for i in &c.implements {
            self.bind_type(i);
        }
        let has_super = c.extends.is_some();
        let in_class = |in_ctor| Ctx {
            in_function: true,
            in_async: false,
            class: ClassCtx::InClass { has_super, in_ctor },
        };

        let mut fields: HashMap<&str, Pos> = HashMap::new();
        for m in &c.members {
            if let ClassMember::Field { name, .. } = m {
                // Field positions aren't tracked (member names are plain
                // strings); report duplicates at the class name.
                if fields.contains_key(name.as_str()) {
                    let msg = format!("duplicate field `{name}` in class `{}`", c.name.text);
                    let pos = c.name.pos;
                    self.error(Code::DuplicateDeclaration, msg, pos);
                } else {
                    fields.insert(name, c.name.pos);
                }
            }
        }

        for m in &c.members {
            match m {
                ClassMember::Field { ty, init, .. } => {
                    self.bind_type(ty);
                    if let Some(init) = init {
                        let saved = self.ctx;
                        self.ctx = in_class(false);
                        self.bind_expr(init);
                        self.ctx = saved;
                    }
                }
                ClassMember::Method {
                    is_async,
                    type_params,
                    params,
                    ret,
                    body,
                    ..
                } => {
                    let mut ctx = in_class(false);
                    ctx.in_async = *is_async;
                    if let Some(body) = body {
                        self.bind_fn_body(type_params, params, Some(ret), body, ctx);
                    } else {
                        self.push_scope();
                        self.bind_type_params(type_params);
                        for p in params {
                            if let Some(t) = &p.ty {
                                self.bind_type(t);
                            }
                        }
                        self.bind_type(ret);
                        self.pop_scope();
                    }
                }
                ClassMember::Getter { ret, body, .. } => {
                    self.bind_fn_body(&[], &[], Some(ret), body, in_class(false));
                }
                ClassMember::Setter { param, body, .. } => {
                    self.bind_fn_body(
                        &[],
                        std::slice::from_ref(param),
                        None,
                        body,
                        in_class(false),
                    );
                }
                ClassMember::Ctor { params, body, .. } => {
                    self.bind_fn_body(&[], params, None, body, in_class(true));
                }
            }
        }
        self.pop_scope();
    }

    /// Shared body binding for functions, methods, accessors, constructors,
    /// and (with `ctx` preserved) arrows.
    fn bind_fn_body(
        &mut self,
        type_params: &[TypeParam],
        params: &[Param],
        ret: Option<&TypeExpr>,
        body: &[Stmt],
        ctx: Ctx,
    ) {
        let saved_ctx = self.ctx;
        let saved_labels = std::mem::take(&mut self.labels);
        let saved_loop = std::mem::replace(&mut self.loop_depth, 0);
        let saved_switch = std::mem::replace(&mut self.switch_depth, 0);
        self.ctx = ctx;

        self.push_scope();
        self.bind_type_params(type_params);
        for p in params {
            if let Some(t) = &p.ty {
                self.bind_type(t);
            }
            let mut names = Vec::new();
            Self::pattern_names(&p.target, &mut names);
            for n in names {
                self.declare_value(n, VKind::Var { is_const: false }, false);
            }
        }
        for p in params {
            if let Some(d) = &p.default {
                self.bind_expr(d);
            }
        }
        if let Some(r) = ret {
            self.bind_type(r);
        }
        self.bind_stmts_in_current_scope(body);
        self.pop_scope();

        self.ctx = saved_ctx;
        self.labels = saved_labels;
        self.loop_depth = saved_loop;
        self.switch_depth = saved_switch;
    }

    // ---- statements ------------------------------------------------------------

    /// Pre-register the block's `let`/`const` (TDZ), then walk. The caller
    /// owns the scope.
    fn bind_stmts_in_current_scope(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            if let Stmt::Var(v) = s {
                self.preregister_var(v);
            }
        }
        for s in stmts {
            self.bind_stmt(s);
        }
    }

    fn bind_block(&mut self, stmts: &[Stmt]) {
        self.push_scope();
        self.bind_stmts_in_current_scope(stmts);
        self.pop_scope();
    }

    fn bind_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block(b) => self.bind_block(b),
            Stmt::Var(v) => self.bind_var_stmt(v),
            Stmt::Expr(e) => self.bind_expr(e),
            Stmt::Empty => {}
            Stmt::If { cond, then, els } => {
                self.bind_expr(cond);
                self.bind_stmt(then);
                if let Some(e) = els {
                    self.bind_stmt(e);
                }
            }
            Stmt::While { cond, body } => {
                self.bind_expr(cond);
                self.loop_depth += 1;
                self.bind_stmt(body);
                self.loop_depth -= 1;
            }
            Stmt::DoWhile { body, cond } => {
                self.loop_depth += 1;
                self.bind_stmt(body);
                self.loop_depth -= 1;
                self.bind_expr(cond);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                match init {
                    Some(ForInit::Var(v)) => {
                        self.preregister_var(v);
                        self.bind_var_stmt(v);
                    }
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.bind_expr(e);
                        }
                    }
                    None => {}
                }
                if let Some(c) = cond {
                    self.bind_expr(c);
                }
                for e in step {
                    self.bind_expr(e);
                }
                self.loop_depth += 1;
                self.bind_stmt(body);
                self.loop_depth -= 1;
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
                // `for await` is the loop form of `await`, and lives where
                // `await` does: an async function, or a module's top level.
                if *is_await && self.ctx.in_function && !self.ctx.in_async {
                    self.error(
                        Code::AwaitOutsideAsync,
                        "`for await` outside an `async` function",
                        crate::check::pos_of(iter),
                    );
                }
                self.push_scope();
                // The iterable is evaluated before the binding exists.
                self.bind_expr(iter);
                if let Some(t) = ty {
                    self.bind_type(t);
                }
                let is_const = *kind == VarKind::Const;
                let mut names = Vec::new();
                Self::pattern_names(target, &mut names);
                for n in names {
                    self.declare_value(n, VKind::Var { is_const }, false);
                }
                self.loop_depth += 1;
                self.bind_stmt(body);
                self.loop_depth -= 1;
                self.pop_scope();
            }
            Stmt::Switch { scrutinee, clauses } => {
                self.bind_expr(scrutinee);
                // All clauses share one scope (as in JS).
                self.push_scope();
                for c in clauses {
                    for s in &c.body {
                        if let Stmt::Var(v) = s {
                            self.preregister_var(v);
                        }
                    }
                }
                self.switch_depth += 1;
                for c in clauses {
                    if let Some(t) = &c.test {
                        self.bind_expr(t);
                    }
                    for s in &c.body {
                        self.bind_stmt(s);
                    }
                }
                self.switch_depth -= 1;
                self.pop_scope();
            }
            Stmt::Break { label, pos } => match label {
                Some(l) => {
                    if !self.labels.contains(&l.text) {
                        let msg = format!("cannot find label `{}`", l.text);
                        self.error(Code::UndefinedLabel, msg, l.pos);
                    }
                }
                None => {
                    if self.loop_depth == 0 && self.switch_depth == 0 {
                        self.error(
                            Code::BreakOutsideLoop,
                            "`break` outside a loop or `switch`",
                            *pos,
                        );
                    }
                }
            },
            Stmt::Continue { label, pos } => match label {
                Some(l) => {
                    if !self.labels.contains(&l.text) {
                        let msg = format!("cannot find label `{}`", l.text);
                        self.error(Code::UndefinedLabel, msg, l.pos);
                    }
                }
                None => {
                    if self.loop_depth == 0 {
                        self.error(Code::BreakOutsideLoop, "`continue` outside a loop", *pos);
                    }
                }
            },
            Stmt::Return { value, pos } => {
                if !self.ctx.in_function {
                    self.error(
                        Code::ReturnOutsideFunction,
                        "`return` outside a function",
                        *pos,
                    );
                }
                if let Some(v) = value {
                    self.bind_expr(v);
                }
            }
            Stmt::Throw(e) => self.bind_expr(e),
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                self.bind_block(block);
                for c in catches {
                    self.bind_type(&c.ty);
                    self.push_scope();
                    self.declare_value(&c.name, VKind::Var { is_const: false }, false);
                    self.bind_stmts_in_current_scope(&c.block);
                    self.pop_scope();
                }
                if let Some(f) = finally {
                    self.bind_block(f);
                }
            }
            Stmt::Labeled { label, body } => {
                self.labels.push(label.text.clone());
                self.bind_stmt(body);
                self.labels.pop();
            }
        }
    }

    fn bind_var_stmt(&mut self, v: &VarStmt) {
        for b in &v.bindings {
            if let Some(t) = &b.ty {
                self.bind_type(t);
            }
            if let Some(init) = &b.init {
                self.bind_expr(init);
            }
            // Defaults inside destructuring patterns are expressions too.
            self.bind_pattern_defaults(&b.target);
            let mut names = Vec::new();
            Self::pattern_names(&b.target, &mut names);
            for n in names {
                self.mark_declared(&n.text);
            }
        }
    }

    fn bind_pattern_defaults(&mut self, p: &Pattern) {
        match p {
            Pattern::Name(_) => {}
            Pattern::Array { elems, rest } => {
                for e in elems {
                    if let Some(d) = &e.default {
                        self.bind_expr(d);
                    }
                    self.bind_pattern_defaults(&e.target);
                }
                if let Some(r) = rest {
                    self.bind_pattern_defaults(r);
                }
            }
            Pattern::Record(fields) => {
                for f in fields {
                    if let Some(d) = &f.default {
                        self.bind_expr(d);
                    }
                    if let Some(t) = &f.target {
                        self.bind_pattern_defaults(t);
                    }
                }
            }
        }
    }

    // ---- expressions --------------------------------------------------------------

    fn bind_expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident(n) => {
                self.resolve_value(n);
            }
            Expr::This(pos) => {
                if matches!(self.ctx.class, ClassCtx::None) {
                    self.error(Code::InvalidThisSuper, "`this` outside a class", *pos);
                }
            }
            Expr::Lit { .. } => {}
            Expr::Template(parts) => {
                for p in parts {
                    if let TplPart::Expr(e) = p {
                        self.bind_expr(e);
                    }
                }
            }
            Expr::Array(elems) => {
                for a in elems {
                    self.bind_expr(&a.expr);
                }
            }
            Expr::Record(fields) => {
                for f in fields {
                    match f {
                        RecordField::Named {
                            name,
                            value: Some(v),
                        } => {
                            let _ = name;
                            self.bind_expr(v);
                        }
                        // Shorthand `{x}` references the binding `x`.
                        RecordField::Named { name, value: None } => {
                            self.resolve_value(name);
                        }
                        RecordField::Spread(e) => self.bind_expr(e),
                    }
                }
            }
            Expr::Paren(e) => self.bind_expr(e),
            Expr::Arrow {
                is_async,
                params,
                ret,
                body,
            } => {
                // Arrows inherit `this` (class context survives); they do
                // not inherit async-ness.
                let ctx = Ctx {
                    in_function: true,
                    in_async: *is_async,
                    class: self.ctx.class,
                };
                match body {
                    ArrowBody::Block(stmts) => {
                        self.bind_fn_body(&[], params, ret.as_ref(), stmts, ctx);
                    }
                    ArrowBody::Expr(e) => {
                        // Same scaffolding, body is a single expression.
                        let saved = self.ctx;
                        self.ctx = ctx;
                        self.push_scope();
                        for p in params {
                            if let Some(t) = &p.ty {
                                self.bind_type(t);
                            }
                            let mut names = Vec::new();
                            Self::pattern_names(&p.target, &mut names);
                            for n in names {
                                self.declare_value(n, VKind::Var { is_const: false }, false);
                            }
                        }
                        for p in params {
                            if let Some(d) = &p.default {
                                self.bind_expr(d);
                            }
                        }
                        if let Some(r) = ret {
                            self.bind_type(r);
                        }
                        self.bind_expr(e);
                        self.pop_scope();
                        self.ctx = saved;
                    }
                }
            }
            Expr::Unary { op, pos, expr } => {
                // `await` is allowed in an async function and at the top level
                // of a module (§4.5: a module that awaits is itself async, and
                // its importers wait for it). Only a *synchronous function*
                // body has nowhere to suspend to.
                if *op == UnaryOp::Await && self.ctx.in_function && !self.ctx.in_async {
                    self.error(
                        Code::AwaitOutsideAsync,
                        "`await` outside an `async` function",
                        *pos,
                    );
                }
                self.bind_expr(expr);
            }
            Expr::Update { expr, .. } => {
                self.check_const_target(expr);
                self.bind_expr(expr);
            }
            Expr::Binary { l, r, .. } => {
                self.bind_expr(l);
                self.bind_expr(r);
            }
            Expr::Assign { target, value, .. } => {
                self.check_const_target(target);
                self.bind_expr(target);
                self.bind_expr(value);
            }
            Expr::Cond { cond, then, els } => {
                self.bind_expr(cond);
                self.bind_expr(then);
                self.bind_expr(els);
            }
            Expr::Cast { expr, ty, .. } => {
                self.bind_expr(expr);
                self.bind_type(ty);
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                self.bind_expr(callee);
                for t in type_args {
                    self.bind_type(t);
                }
                for a in args {
                    self.bind_expr(&a.expr);
                }
            }
            Expr::New { ty, args } => {
                self.bind_type(ty);
                for a in args {
                    self.bind_expr(&a.expr);
                }
            }
            Expr::Member { obj, .. } => self.bind_expr(obj),
            Expr::Index { obj, index, .. } => {
                self.bind_expr(obj);
                self.bind_expr(index);
            }
            Expr::SuperMember { pos, .. } => {
                if !matches!(
                    self.ctx.class,
                    ClassCtx::InClass {
                        has_super: true,
                        ..
                    }
                ) {
                    self.error(
                        Code::InvalidThisSuper,
                        "`super` requires a class with an `extends` clause",
                        *pos,
                    );
                }
            }
            Expr::SuperCall { args, pos } => {
                if !matches!(
                    self.ctx.class,
                    ClassCtx::InClass {
                        has_super: true,
                        in_ctor: true
                    }
                ) {
                    self.error(
                        Code::InvalidThisSuper,
                        "`super(…)` is only valid in the constructor of a class with an \
                         `extends` clause",
                        *pos,
                    );
                }
                for a in args {
                    self.bind_expr(&a.expr);
                }
            }
            Expr::ImportCall(e) => self.bind_expr(e),
            Expr::Yield { value, pos } => {
                if !self.ctx.in_function {
                    self.error(
                        Code::YieldOutsideFunction,
                        "`yield` is only valid inside a function (which makes it a generator)",
                        *pos,
                    );
                }
                if let Some(v) = value {
                    self.bind_expr(v);
                }
            }
        }
    }

    fn check_const_target(&mut self, target: &Expr) {
        if let Expr::Ident(n) = target {
            for scope in self.scopes.iter().rev() {
                if let Some(sym) = scope.values.get(&n.text) {
                    if matches!(sym.kind, VKind::Var { is_const: true }) {
                        let msg = format!("cannot assign to `{}`: it is a `const`", n.text);
                        let pos = n.pos;
                        self.error(Code::AssignToConst, msg, pos);
                    }
                    return;
                }
            }
        }
    }

    // ---- types ----------------------------------------------------------------------

    fn bind_type(&mut self, t: &TypeExpr) {
        match t {
            TypeExpr::Named { name, pos, args } => {
                for a in args {
                    self.bind_type(a);
                }
                if let Some(first) = name.split('.').next() {
                    if name.contains('.') {
                        // Qualified: the head must be some known name (a
                        // namespace import); deeper resolution needs the
                        // module graph.
                        if !self.value_exists(first) && !self.type_exists(first) {
                            let msg = format!("cannot find name `{first}` (in type `{name}`)");
                            self.error(Code::UnknownTypeName, msg, *pos);
                        }
                        return;
                    }
                }
                if PREDEFINED_TYPES.contains(&name.as_str()) {
                    return;
                }
                if !self.type_exists(name) {
                    let msg = format!("cannot find type `{name}`");
                    self.error(Code::UnknownTypeName, msg, *pos);
                }
            }
            TypeExpr::Nullable(t) | TypeExpr::ArrayOf(t) => self.bind_type(t),
            TypeExpr::Union(arms) => {
                for a in arms {
                    self.bind_type(a);
                }
            }
            TypeExpr::Tuple(ts) => {
                for t in ts {
                    self.bind_type(t);
                }
            }
            TypeExpr::Record(members) => {
                for m in members {
                    self.bind_type(&m.ty);
                }
            }
            TypeExpr::Function {
                type_params,
                params,
                ret,
            } => {
                self.push_scope();
                self.bind_type_params(type_params);
                for p in params {
                    self.bind_type(&p.ty);
                }
                self.bind_type(ret);
                self.pop_scope();
            }
        }
    }
}
