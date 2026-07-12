//! Type checker v1: enforces §3 (strict typing, numeric-only implicit
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

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{self, *};
use crate::diag::{Code, Diagnostic, Pos};

pub struct CheckOutput {
    pub diagnostics: Vec<Diagnostic>,
}

pub fn check(module: &Module) -> CheckOutput {
    let mut out = check_graph(&[("<main>".to_string(), module)]);
    out.pop().map(|(_, o)| o).unwrap_or(CheckOutput { diagnostics: Vec::new() })
}

/// Check a whole module graph (dependency-first). One `Checker` spans the
/// graph so a class declared in one module is the *same* type when imported
/// into another; scopes and type namespaces are per-module.
pub fn check_graph(modules: &[(String, &Module)]) -> Vec<(String, CheckOutput)> {
    let mut c = Checker::new();
    // Ambient web platform (generated from WebIDL); its own collection
    // diagnostics are suppressed — the generator is validated separately.
    let n = c.diags.len();
    c.collect(crate::webapi::webapi().module);
    c.diags.truncate(n);

    let base_types = c.type_defs.clone();
    let base_scope = c.scopes[0].clone();
    let mut exports: HashMap<String, ModuleExports> = HashMap::new();
    let mut results = Vec::new();

    for (spec, module) in modules {
        c.diags.clear();
        c.type_defs = base_types.clone();
        c.scopes = vec![base_scope.clone(), HashMap::new()];
        c.module_spec = spec.clone();
        c.imported.clear();

        // Bind this module's relative imports from already-checked modules.
        for item in &module.items {
            let Item::Import(im) = item else { continue };
            if !crate::graph::is_relative(&im.from) {
                continue;
            }
            let target = crate::graph::resolve(spec, &im.from);
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
                    // Namespace imports type as `any` (v1).
                    c.define(&n.text, Ty::Any, true);
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
        exports.insert(spec.clone(), e);

        let mut diagnostics = std::mem::take(&mut c.diags);
        diagnostics.sort_by_key(|d| (d.pos.line, d.pos.col));
        results.push((spec.clone(), CheckOutput { diagnostics }));
    }
    results
}

#[derive(Default)]
struct ModuleExports {
    values: HashMap<String, Ty>,
    types: HashMap<String, TypeDef>,
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
        matches!(self, IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64)
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
pub enum Ty {
    Any,
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
    Nullable(Rc<Ty>),
    Array(Rc<Ty>),
    Tuple(Rc<Vec<Ty>>),
    Record(Rc<Vec<RecField>>),
    Fn(Rc<FnTy>),
    Class(ClassId, Rc<Vec<Ty>>),
    Iface(IfaceId, Rc<Vec<Ty>>),
    Enum(EnumId),
    /// The class object itself (statics, `instanceof` RHS).
    ClassMeta(ClassId),
    /// The enum object itself (`Color.RED`).
    EnumMeta(EnumId),
    /// Built-in namespaces (`console`, `document`); Any-typed namespace for
    /// unknown imports.
    Namespace(Ns),
    Var(TvId),
    Union(Rc<Vec<Ty>>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Ns {
    Console,
    Document,
    Opaque,
}

#[derive(Clone)]
pub struct RecField {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

#[derive(Clone)]
pub struct FnTy {
    pub tparams: Vec<TvId>,
    pub params: Vec<ParamTy>,
    pub ret: Ty,
}

#[derive(Clone)]
pub struct ParamTy {
    pub ty: Ty,
    pub optional: bool,
    pub rest: bool,
}

// ---- declaration tables ------------------------------------------------------

struct FieldInfo {
    name: String,
    ty: Ty,
    access: Access,
    is_static: bool,
    readonly: bool,
}

struct MethodInfo {
    name: String,
    sig: FnTy,
    access: Access,
    is_static: bool,
    is_abstract: bool,
    is_final: bool,
    has_override: bool,
}

struct AccessorInfo {
    name: String,
    ty: Ty, // getter return / setter param
    access: Access,
}

struct ClassInfo {
    name: String,
    tparams: Vec<TvId>,
    parent: Option<(ClassId, Vec<Ty>)>,
    ifaces: Vec<(IfaceId, Vec<Ty>)>,
    fields: Vec<FieldInfo>,
    methods: Vec<MethodInfo>,
    getters: Vec<AccessorInfo>,
    setters: Vec<AccessorInfo>,
    ctor: Option<(Vec<ParamTy>, Access)>,
    is_abstract: bool,
    is_final: bool,
}

struct IfaceMember {
    name: String,
    ty: Ty, // property type or Fn
    optional: bool,
}

struct IfaceInfo {
    name: String,
    tparams: Vec<TvId>,
    extends: Vec<(IfaceId, Vec<Ty>)>,
    members: Vec<IfaceMember>,
}

struct EnumInfo {
    name: String,
    members: Vec<String>,
}

struct AliasInfo {
    tparams: Vec<TvId>,
    target: Ty,
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
    ty: Ty,
    is_const: bool,
}

struct Checker {
    diags: Vec<Diagnostic>,
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
    narrows: Vec<HashMap<String, Ty>>,
    /// Type-parameter scopes for resolution.
    tp_scopes: Vec<HashMap<String, TvId>>,
    current_class: Option<ClassId>,
    in_ctor: bool,
    in_static: bool,
    ret_ty: Option<Ty>,
    // Built-in class ids
    error_id: ClassId,
    element_id: ClassId,
    /// The generated `interface Promise<T>` (webapi), resolved lazily.
    promise_id: Option<IfaceId>,
    /// The module being checked (diagnostics/context).
    module_spec: String,
    /// Type names pulled in from other modules (not declared here).
    imported: std::collections::HashSet<String>,
}

const PREDEFINED: &[(&str, Ty)] = &[
    ("bool", Ty::Bool),
    ("char", Ty::Char),
    ("string", Ty::Str),
    ("bigint", Ty::BigInt),
    ("bigdec", Ty::BigDec),
    ("void", Ty::Void),
    ("int", Ty::Int(IntKind::I32)),
    ("int8", Ty::Int(IntKind::I8)),
    ("int16", Ty::Int(IntKind::I16)),
    ("int32", Ty::Int(IntKind::I32)),
    ("int64", Ty::Int(IntKind::I64)),
    ("uint", Ty::Int(IntKind::U32)),
    ("uint8", Ty::Int(IntKind::U8)),
    ("uint16", Ty::Int(IntKind::U16)),
    ("uint32", Ty::Int(IntKind::U32)),
    ("uint64", Ty::Int(IntKind::U64)),
    ("float", Ty::F64),
    ("float32", Ty::F32),
    ("float64", Ty::F64),
];

fn predefined(name: &str) -> Option<Ty> {
    PREDEFINED.iter().find(|(n, _)| *n == name).map(|(_, t)| t.clone())
}

impl Checker {
    fn new() -> Checker {
        let mut c = Checker {
            diags: Vec::new(),
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
            module_spec: String::new(),
            imported: std::collections::HashSet::new(),
        };
        c.install_builtins();
        c
    }

    fn install_builtins(&mut self) {
        let str_param = ParamTy { ty: Ty::Str, optional: true, rest: false };
        // Error hierarchy (spec §4.6).
        let error_id = self.classes.len();
        self.error_id = error_id;
        self.classes.push(ClassInfo {
            name: "Error".into(),
            tparams: vec![],
            parent: None,
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "message".into(),
                ty: Ty::Str,
                access: Access::Public,
                is_static: false,
                readonly: false,
            }],
            methods: vec![],
            getters: vec![],
            setters: vec![],
            ctor: Some((vec![str_param.clone()], Access::Public)),
            is_abstract: false,
            is_final: false,
        });
        self.type_defs.insert("Error".into(), TypeDef::Class(error_id));
        for name in ["RangeError", "TypeError"] {
            let id = self.classes.len();
            self.classes.push(ClassInfo {
                name: name.into(),
                tparams: vec![],
                parent: Some((error_id, vec![])),
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
            ifaces: vec![],
            fields: vec![
                FieldInfo {
                    name: "textContent".into(),
                    ty: Ty::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: false,
                },
                FieldInfo {
                    name: "value".into(),
                    ty: Ty::Str,
                    access: Access::Public,
                    is_static: false,
                    readonly: false,
                },
            ],
            methods: vec![MethodInfo {
                name: "addEventListener".into(),
                sig: FnTy {
                    tparams: vec![],
                    params: vec![
                        ParamTy { ty: Ty::Str, optional: false, rest: false },
                        ParamTy {
                            ty: Ty::Fn(Rc::new(FnTy {
                                tparams: vec![],
                                params: vec![],
                                ret: Ty::Void,
                            })),
                            optional: false,
                            rest: false,
                        },
                    ],
                    ret: Ty::Void,
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
        let elem_ty = Ty::Class(element_id, Rc::new(vec![]));
        for (name, params, ret) in [
            (
                "appendChild",
                vec![ParamTy { ty: elem_ty.clone(), optional: false, rest: false }],
                Ty::Void,
            ),
            ("remove", vec![], Ty::Void),
        ] {
            self.classes[element_id].methods.push(MethodInfo {
                name: name.into(),
                sig: FnTy { tparams: vec![], params, ret },
                access: Access::Public,
                is_static: false,
                is_abstract: false,
                is_final: false,
                has_override: false,
            });
        }
        self.type_defs.insert("Element".into(), TypeDef::Class(element_id));

        for name in ["Error", "RangeError", "TypeError"] {
            let TypeDef::Class(id) = self.type_defs[name] else { unreachable!() };
            self.scopes[0]
                .insert(name.to_string(), VarInfo { ty: Ty::ClassMeta(id), is_const: true });
        }
        self.install_collections();
    }

    /// Built-in generic collections (spec §3.8): Map<K,V>, Set<T>. Methods
    /// follow the consistent-API rules (§1.3): mutators return void/bool,
    /// views are verbs.
    fn install_collections(&mut self) {
        let kv = (self.fresh_tv("K"), self.fresh_tv("V"));
        let m = |tparams: Vec<TvId>, params: Vec<ParamTy>, ret: Ty| MethodInfo {
            name: String::new(),
            sig: FnTy { tparams, params, ret },
            access: Access::Public,
            is_static: false,
            is_abstract: false,
            is_final: false,
            has_override: false,
        };
        let p = |ty: Ty| ParamTy { ty, optional: false, rest: false };
        let (k, v) = (Ty::Var(kv.0), Ty::Var(kv.1));

        let mut map_methods = Vec::new();
        for (name, params, ret) in [
            ("set", vec![p(k.clone()), p(v.clone())], Ty::Void),
            ("get", vec![p(k.clone())], nullable(v.clone())),
            ("has", vec![p(k.clone())], Ty::Bool),
            ("remove", vec![p(k.clone())], Ty::Bool),
            ("keys", vec![], Ty::Array(Rc::new(k.clone()))),
            ("values", vec![], Ty::Array(Rc::new(v.clone()))),
            (
                "entries",
                vec![],
                Ty::Array(Rc::new(Ty::Tuple(Rc::new(vec![k.clone(), v.clone()])))),
            ),
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
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "size".into(),
                ty: Ty::Int(IntKind::I32),
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
        self.scopes[0]
            .insert("Map".into(), VarInfo { ty: Ty::ClassMeta(map_id), is_const: true });

        let t = self.fresh_tv("T");
        let tv = Ty::Var(t);
        let mut set_methods = Vec::new();
        for (name, params, ret) in [
            ("add", vec![p(tv.clone())], Ty::Void),
            ("has", vec![p(tv.clone())], Ty::Bool),
            ("remove", vec![p(tv.clone())], Ty::Bool),
            ("values", vec![], Ty::Array(Rc::new(tv.clone()))),
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
            ifaces: vec![],
            fields: vec![FieldInfo {
                name: "size".into(),
                ty: Ty::Int(IntKind::I32),
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
        self.scopes[0]
            .insert("Set".into(), VarInfo { ty: Ty::ClassMeta(set_id), is_const: true });
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

    fn show(&self, t: &Ty) -> String {
        match t {
            Ty::Any => "any".into(),
            Ty::Err => "<error>".into(),
            Ty::Void => "void".into(),
            Ty::Null => "null".into(),
            Ty::Bool => "bool".into(),
            Ty::Char => "char".into(),
            Ty::Str => "string".into(),
            Ty::BigInt => "bigint".into(),
            Ty::BigDec => "bigdec".into(),
            Ty::Int(k) => k.name().into(),
            Ty::F32 => "float32".into(),
            Ty::F64 => "float64".into(),
            Ty::Nullable(t) => format!("{}?", self.show(t)),
            Ty::Array(t) => format!("{}[]", self.show(t)),
            Ty::Tuple(ts) => {
                format!("[{}]", ts.iter().map(|t| self.show(t)).collect::<Vec<_>>().join(", "))
            }
            Ty::Record(fs) => format!(
                "{{{}}}",
                fs.iter()
                    .map(|f| format!("{}: {}", f.name, self.show(&f.ty)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Fn(f) => format!(
                "({}) => {}",
                f.params.iter().map(|p| self.show(&p.ty)).collect::<Vec<_>>().join(", "),
                self.show(&f.ret)
            ),
            Ty::Class(id, args) | Ty::Iface(id, args) => {
                let name = match t {
                    Ty::Class(..) => &self.classes[*id].name,
                    _ => &self.ifaces[*id].name,
                };
                if args.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{name}<{}>",
                        args.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", ")
                    )
                }
            }
            Ty::Enum(id) => self.enums[*id].name.clone(),
            Ty::ClassMeta(id) => format!("class {}", self.classes[*id].name),
            Ty::EnumMeta(id) => format!("enum {}", self.enums[*id].name),
            Ty::Namespace(_) => "namespace".into(),
            Ty::Var(tv) => self.tv_names[*tv].clone(),
            Ty::Union(arms) => {
                arms.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(" | ")
            }
        }
    }

    // ---- scopes ---------------------------------------------------------------

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, ty: Ty, is_const: bool) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), VarInfo { ty, is_const });
    }

    fn lookup(&self, name: &str) -> Option<VarInfo> {
        for n in self.narrows.iter().rev() {
            if let Some(t) = n.get(name) {
                // Narrow overlays refine the type; const-ness from scope.
                let base = self.lookup_scope(name);
                return Some(VarInfo {
                    ty: t.clone(),
                    is_const: base.map(|b| b.is_const).unwrap_or(false),
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

    fn kill_narrow(&mut self, name: &str) {
        for n in &mut self.narrows {
            n.remove(name);
        }
    }

    // ---- collection (headers) ----------------------------------------------------

    fn collect(&mut self, module: &Module) {
        // Phase A: allocate ids so mutual references resolve.
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl { kind: ExportKind::Decl(d), .. }) => d,
                _ => continue,
            };
            match d {
                Decl::Class(c) => {
                    let id = self.classes.len();
                    self.classes.push(ClassInfo {
                        name: c.name.text.clone(),
                        tparams: vec![],
                        parent: None,
                        ifaces: vec![],
                        fields: vec![],
                        methods: vec![],
                        getters: vec![],
                        setters: vec![],
                        ctor: None,
                        is_abstract: c.is_abstract,
                        is_final: c.is_final,
                    });
                    self.type_defs.insert(c.name.text.clone(), TypeDef::Class(id));
                }
                Decl::Interface(i) => {
                    let id = self.ifaces.len();
                    self.ifaces.push(IfaceInfo {
                        name: i.name.text.clone(),
                        tparams: vec![],
                        extends: vec![],
                        members: vec![],
                    });
                    self.type_defs.insert(i.name.text.clone(), TypeDef::Iface(id));
                }
                Decl::Enum(e) => {
                    let id = self.enums.len();
                    self.enums.push(EnumInfo {
                        name: e.name.text.clone(),
                        members: e.members.iter().map(|(n, _)| n.text.clone()).collect(),
                    });
                    self.type_defs.insert(e.name.text.clone(), TypeDef::Enum(id));
                }
                Decl::TypeAlias(t) => {
                    let id = self.aliases.len();
                    self.aliases.push(AliasInfo { tparams: vec![], target: Ty::Any });
                    self.type_defs.insert(t.name.text.clone(), TypeDef::Alias(id));
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
                Item::Export(ExportDecl { kind: ExportKind::Decl(d), .. }) => d,
                _ => continue,
            };
            match d {
                Decl::Class(c) => {
                    let tvs: Vec<TvId> =
                        c.type_params.iter().map(|tp| self.fresh_tv(&tp.name.text)).collect();
                    if let TypeDef::Class(id) = self.type_defs[&c.name.text] {
                        self.classes[id].tparams = tvs;
                    }
                }
                Decl::Interface(i) => {
                    let tvs: Vec<TvId> =
                        i.type_params.iter().map(|tp| self.fresh_tv(&tp.name.text)).collect();
                    if let TypeDef::Iface(id) = self.type_defs[&i.name.text] {
                        self.ifaces[id].tparams = tvs;
                    }
                }
                Decl::TypeAlias(t) => {
                    let tvs: Vec<TvId> =
                        t.type_params.iter().map(|tp| self.fresh_tv(&tp.name.text)).collect();
                    if let TypeDef::Alias(id) = self.type_defs[&t.name.text] {
                        self.aliases[id].tparams = tvs;
                    }
                }
                _ => {}
            }
        }
        // Phase B: resolve headers.
        for item in &module.items {
            let d = match item {
                Item::Decl(d) => d,
                Item::Export(ExportDecl { kind: ExportKind::Decl(d), .. }) => d,
                _ => continue,
            };
            self.collect_decl_header(d);
        }
    }

    fn collect_import(&mut self, im: &ImportDecl) {
        // Relative imports are bound precisely by `check_graph` from the
        // exporting module — don't clobber those with `any` here.
        if crate::graph::is_relative(&im.from) {
            return;
        }
        let Some(clause) = &im.clause else { return };
        match clause {
            ImportClause::Namespace(n) => {
                self.define(&n.text, Ty::Namespace(Ns::Opaque), true);
            }
            ImportClause::Named(specs) => {
                for s in specs {
                    let local = s.alias.as_ref().unwrap_or(&s.name);
                    let ty = match (im.from.as_str(), s.name.text.as_str()) {
                        ("std:console", "console") => Ty::Namespace(Ns::Console),
                        ("browser:dom", global) => {
                            match crate::webapi::global_type(global) {
                                Some(ast_ty) => self.resolve_type(ast_ty),
                                None => Ty::Any,
                            }
                        }
                        _ => Ty::Any,
                    };
                    self.define(&local.text, ty, true);
                    self.type_defs.insert(local.text.clone(), TypeDef::Imported);
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
                        Ty::Err
                    }
                };
                self.tp_scopes.pop();
                // `async function f(): T` — the body returns T, callers get
                // Promise<T> (an already-Promise<…> annotation is kept).
                let ret = if f.is_async && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
                } else {
                    ret
                };
                let fnty = Ty::Fn(Rc::new(FnTy { tparams: tvs, params, ret }));
                self.define(&f.name.text, fnty, true);
            }
            Decl::Class(c) => {
                let TypeDef::Class(id) = self.type_defs[&c.name.text] else { return };
                let tvs = self.classes[id].tparams.clone();
                self.push_tp_scope(&c.type_params, &tvs);

                let parent = c.extends.as_ref().and_then(|t| {
                    let rt = self.resolve_type(t);
                    match rt {
                        Ty::Class(pid, args) => {
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
                        Ty::Err => None,
                        _ => {
                            self.error(
                                Code::TypeMismatch,
                                "`extends` must name a class",
                                c.name.pos,
                            );
                            None
                        }
                    }
                });
                self.classes[id].parent = parent;
                let mut ifaces = Vec::new();
                for t in &c.implements {
                    match self.resolve_type(t) {
                        Ty::Iface(iid, args) => ifaces.push((iid, args.as_ref().clone())),
                        Ty::Err => {}
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
                let TypeDef::Iface(id) = self.type_defs[&i.name.text] else { return };
                let tvs = self.ifaces[id].tparams.clone();
                self.push_tp_scope(&i.type_params, &tvs);
                let mut extends = Vec::new();
                for t in &i.extends {
                    match self.resolve_type(t) {
                        Ty::Iface(iid, args) => extends.push((iid, args.as_ref().clone())),
                        Ty::Err => {}
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
                        InterfaceMember::Prop { name, optional, ty, .. } => {
                            let t = self.resolve_type(ty);
                            members.push(IfaceMember {
                                name: name.clone(),
                                ty: t,
                                optional: *optional,
                            });
                        }
                        InterfaceMember::Method { name, type_params, params, ret } => {
                            let tvs = self.bind_tparams(type_params);
                            let params = self.resolve_params(params);
                            let ret = self.resolve_type(ret);
                            self.tp_scopes.pop();
                            members.push(IfaceMember {
                                name: name.clone(),
                                ty: Ty::Fn(Rc::new(FnTy { tparams: tvs, params, ret })),
                                optional: false,
                            });
                        }
                    }
                }
                self.ifaces[id].members = members;
            }
            Decl::Enum(e) => {
                let TypeDef::Enum(id) = self.type_defs[&e.name.text] else { return };
                self.define(&e.name.text, Ty::EnumMeta(id), true);
            }
            Decl::TypeAlias(t) => {
                let TypeDef::Alias(id) = self.type_defs[&t.name.text] else { return };
                let tvs = self.aliases[id].tparams.clone();
                self.push_tp_scope(&t.type_params, &tvs);
                let target = self.resolve_type(&t.ty);
                self.tp_scopes.pop();
                self.aliases[id].target = target;
            }
        }
        // Class values (constructors as values / statics).
        if let Decl::Class(c) = d {
            if let TypeDef::Class(id) = self.type_defs[&c.name.text] {
                self.define(&c.name.text, Ty::ClassMeta(id), true);
            }
        }
    }

    fn collect_member(&mut self, id: ClassId, m: &ClassMember, class_pos: Pos) {
        match m {
            ClassMember::Field { mods, readonly, name, ty, .. } => {
                let t = self.resolve_type(ty);
                self.classes[id].fields.push(FieldInfo {
                    name: name.clone(),
                    ty: t,
                    access: mods.access.unwrap_or(Access::Private),
                    is_static: mods.is_static,
                    readonly: *readonly,
                });
            }
            ClassMember::Method { mods, is_async, name, type_params, params, ret, body } => {
                let tvs = self.bind_tparams(type_params);
                let params = self.resolve_params(params);
                let ret = self.resolve_type(ret);
                self.tp_scopes.pop();
                let ret = if *is_async && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
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
                    sig: FnTy { tparams: tvs, params, ret },
                    access: mods.access.unwrap_or(Access::Private),
                    is_static: mods.is_static,
                    is_abstract,
                    is_final: mods.virt == Some(Virt::Final),
                    has_override: mods.virt == Some(Virt::Override),
                });
            }
            ClassMember::Getter { mods, name, ret, .. } => {
                let t = self.resolve_type(ret);
                self.classes[id].getters.push(AccessorInfo {
                    name: name.clone(),
                    ty: t,
                    access: mods.access.unwrap_or(Access::Private),
                });
            }
            ClassMember::Setter { mods, name, param, .. } => {
                let t = param.ty.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Any);
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

    fn resolve_params(&mut self, params: &[Param]) -> Vec<ParamTy> {
        params
            .iter()
            .map(|p| {
                let mut ty = p.ty.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Any);
                if p.rest {
                    // `...xs: int32[]` — the per-argument type is the element.
                    ty = match ty {
                        Ty::Array(e) => e.as_ref().clone(),
                        Ty::Any | Ty::Err => Ty::Any,
                        other => {
                            self.error(
                                Code::TypeMismatch,
                                format!(
                                    "rest parameter needs an array type, got `{}`",
                                    self.show(&other)
                                ),
                                pattern_pos(&p.target),
                            );
                            Ty::Err
                        }
                    };
                }
                if p.optional {
                    ty = nullable(ty);
                }
                ParamTy { ty, optional: p.optional || p.default.is_some(), rest: p.rest }
            })
            .collect()
    }

    // ---- type resolution -----------------------------------------------------------

    fn resolve_type(&mut self, t: &ast::Type) -> Ty {
        match t {
            ast::Type::Named { name, pos, args } => self.resolve_named(name, *pos, args),
            ast::Type::Nullable(inner) => nullable(self.resolve_type(inner)),
            ast::Type::ArrayOf(inner) => Ty::Array(Rc::new(self.resolve_type(inner))),
            ast::Type::Union(arms) => {
                let tys: Vec<Ty> = arms.iter().map(|a| self.resolve_type(a)).collect();
                fold_union(tys)
            }
            ast::Type::Tuple(ts) => {
                Ty::Tuple(Rc::new(ts.iter().map(|t| self.resolve_type(t)).collect()))
            }
            ast::Type::Record(members) => {
                let fs = members
                    .iter()
                    .map(|m| RecField {
                        name: m.name.clone(),
                        ty: self.resolve_type(&m.ty),
                        optional: m.optional,
                    })
                    .collect();
                Ty::Record(Rc::new(fs))
            }
            ast::Type::Function { params, ret, .. } => {
                let params = params
                    .iter()
                    .map(|p| ParamTy {
                        ty: self.resolve_type(&p.ty),
                        optional: p.optional,
                        rest: p.rest,
                    })
                    .collect();
                let ret = self.resolve_type(ret);
                Ty::Fn(Rc::new(FnTy { tparams: vec![], params, ret }))
            }
        }
    }

    fn resolve_named(&mut self, name: &str, pos: Pos, args: &[ast::Type]) -> Ty {
        let rargs: Vec<Ty> = args.iter().map(|a| self.resolve_type(a)).collect();
        // Type parameters shadow everything.
        for scope in self.tp_scopes.iter().rev() {
            if let Some(tv) = scope.get(name) {
                return Ty::Var(*tv);
            }
        }
        // Generated marker for IDL `any`/`object`.
        if name == "JsAny" {
            return Ty::Any;
        }
        if let Some(t) = predefined(name) {
            return t;
        }
        if name.contains('.') {
            return Ty::Any; // namespace-qualified: module graph later
        }
        match self.type_defs.get(name) {
            Some(TypeDef::Class(id)) => {
                let id = *id;
                self.check_arity(name, self.classes[id].tparams.len(), rargs.len(), pos);
                Ty::Class(id, Rc::new(rargs))
            }
            Some(TypeDef::Iface(id)) => {
                let id = *id;
                if name == "Promise" {
                    self.promise_id = Some(id);
                }
                self.check_arity(name, self.ifaces[id].tparams.len(), rargs.len(), pos);
                Ty::Iface(id, Rc::new(rargs))
            }
            Some(TypeDef::Enum(id)) => Ty::Enum(*id),
            Some(TypeDef::Alias(id)) => {
                let id = *id;
                let info = &self.aliases[id];
                let (tvs, target) = (info.tparams.clone(), info.target.clone());
                self.check_arity(name, tvs.len(), rargs.len(), pos);
                let map: HashMap<TvId, Ty> = tvs.into_iter().zip(rargs).collect();
                subst(&target, &map)
            }
            Some(TypeDef::Imported) => Ty::Any,
            None => Ty::Err, // binder already reported E0308
        }
    }

    /// `Promise<T>` for the generated interface (falls back to `any`).
    fn promise_of(&mut self, t: Ty) -> Ty {
        let id = match self.promise_id {
            Some(id) => id,
            None => match self.type_defs.get("Promise") {
                Some(TypeDef::Iface(id)) => {
                    self.promise_id = Some(*id);
                    *id
                }
                _ => return Ty::Any,
            },
        };
        Ty::Iface(id, Rc::new(vec![t]))
    }

    /// `T` from `Promise<T>`; `None` if not a promise.
    fn unwrap_promise(&mut self, t: &Ty) -> Option<Ty> {
        let pid = self.promise_id.or(match self.type_defs.get("Promise") {
            Some(TypeDef::Iface(id)) => Some(*id),
            _ => None,
        })?;
        match strip_null(t) {
            Ty::Iface(id, args) if id == pid => {
                Some(args.first().cloned().unwrap_or(Ty::Any))
            }
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

    // ---- module & statements ------------------------------------------------------

    fn check_module(&mut self, module: &Module) {
        // Module vars first (types available to bodies), then bodies.
        for item in &module.items {
            match item {
                Item::Stmt(Stmt::Var(v)) | Item::Export(ExportDecl { kind: ExportKind::Var(v), .. }) => {
                    self.check_var(v)
                }
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
                Item::Decl(d) | Item::Export(ExportDecl { kind: ExportKind::Decl(d), .. }) => {
                    self.check_decl_body(d)
                }
                _ => {}
            }
        }
    }

    fn check_decl_body(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => {
                let Some(VarInfo { ty: Ty::Fn(sig), .. }) = self.lookup(&f.name.text) else {
                    return;
                };
                self.check_fn_body_async(&f.type_params, &f.params, &sig, &f.body, f.is_async);
            }
            Decl::Class(c) => self.check_class_body(c),
            Decl::Enum(e) => {
                for (_, init) in &e.members {
                    if let Some(init) = init {
                        let t = self.check_expr(init, None);
                        if !matches!(t, Ty::Int(_) | Ty::Err | Ty::Any) {
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
        sig: &FnTy,
        body: &[Stmt],
        is_async: bool,
    ) {
        if is_async {
            if let Some(inner) = self.unwrap_promise(&sig.ret) {
                let unwrapped =
                    FnTy { tparams: sig.tparams.clone(), params: sig.params.clone(), ret: inner };
                return self.check_fn_body(tps, params, &unwrapped, body);
            }
        }
        self.check_fn_body(tps, params, sig, body)
    }

    fn check_fn_body(
        &mut self,
        tps: &[TypeParam],
        params: &[Param],
        sig: &FnTy,
        body: &[Stmt],
    ) {
        let mut scope = HashMap::new();
        for (tp, tv) in tps.iter().zip(&sig.tparams) {
            scope.insert(tp.name.text.clone(), *tv);
        }
        self.tp_scopes.push(scope);
        self.push_scope();
        for (p, pt) in params.iter().zip(&sig.params) {
            let ty = if p.rest { Ty::Array(Rc::new(pt.ty.clone())) } else { pt.ty.clone() };
            self.bind_pattern_ty(&p.target, &ty, false);
            if let Some(d) = &p.default {
                let dt = self.check_expr(d, Some(&pt.ty));
                self.require_assignable(&dt, &pt.ty, pos_of(d), "default value");
            }
        }
        let saved_ret = self.ret_ty.replace(sig.ret.clone());
        for s in body {
            self.check_stmt(s);
        }
        self.ret_ty = saved_ret;
        self.pop_scope();
        self.tp_scopes.pop();
    }

    fn class_self_type(&self, id: ClassId) -> Ty {
        let args: Vec<Ty> = self.classes[id].tparams.iter().map(|tv| Ty::Var(*tv)).collect();
        Ty::Class(id, Rc::new(args))
    }

    fn check_class_body(&mut self, c: &ClassDecl) {
        let TypeDef::Class(id) = self.type_defs[&c.name.text] else { return };
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
                ClassMember::Field { name, init, mods, .. } => {
                    if let Some(init) = init {
                        let want = self
                            .field_info(id, name)
                            .map(|f| f.0)
                            .unwrap_or(Ty::Err);
                        self.push_scope();
                        self.in_static = mods.is_static;
                        if !mods.is_static {
                            self.define("this", self_ty.clone(), true);
                        }
                        let t = self.check_expr(init, Some(&want));
                        self.require_assignable(&t, &want, pos_of(init), "field initializer");
                        self.in_static = false;
                        self.pop_scope();
                    }
                }
                ClassMember::Method { name, type_params, params, body, mods, is_async, .. } => {
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
                    let ret = self
                        .classes[id]
                        .getters
                        .iter()
                        .find(|g| g.name == *name)
                        .map(|g| g.ty.clone())
                        .unwrap_or(Ty::Err);
                    let sig = FnTy { tparams: vec![], params: vec![], ret };
                    self.push_scope();
                    self.define("this", self_ty.clone(), true);
                    self.check_fn_body(&[], &[], &sig, body);
                    self.pop_scope();
                }
                ClassMember::Setter { name, param, body, .. } => {
                    let pt = self
                        .classes[id]
                        .setters
                        .iter()
                        .find(|s| s.name == *name)
                        .map(|s| s.ty.clone())
                        .unwrap_or(Ty::Err);
                    let sig = FnTy {
                        tparams: vec![],
                        params: vec![ParamTy { ty: pt, optional: false, rest: false }],
                        ret: Ty::Void,
                    };
                    self.push_scope();
                    self.define("this", self_ty.clone(), true);
                    self.check_fn_body(&[], std::slice::from_ref(param), &sig, body);
                    self.pop_scope();
                }
                ClassMember::Ctor { params, body, .. } => {
                    let ptys = self.classes[id].ctor.clone().map(|(p, _)| p).unwrap_or_default();
                    let sig = FnTy { tparams: vec![], params: ptys, ret: Ty::Void };
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
                    self.diags.push(Diagnostic::error(Code::BadOverride, msg, name_pos));
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
                (Some((bm_final, _)), true) => {
                    if bm_final {
                        self.error(
                            Code::BadOverride,
                            format!("cannot override final method `{name}`"),
                            pos,
                        );
                    }
                    let _ = &map;
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
                        || self
                            .classes[id]
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

    fn find_method_in_chain(&self, id: ClassId, name: &str) -> Option<(bool, FnTy)> {
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
            let imap: HashMap<TvId, Ty> =
                self.ifaces[iid].tparams.iter().copied().zip(args.iter().cloned()).collect();
            let members: Vec<(String, Ty, bool)> = self.ifaces[iid]
                .members
                .iter()
                .map(|m| (m.name.clone(), subst(&m.ty, &imap), m.optional))
                .collect();
            for (name, want, optional) in members {
                let has = match &want {
                    Ty::Fn(_) => self.method_sig(id, &name, false).is_some(),
                    _ => self.field_info(id, &name).is_some()
                        || self.classes[id].getters.iter().any(|g| g.name == name),
                };
                if !has && !optional {
                    self.error(
                        Code::BadOverride,
                        format!(
                            "class `{}` is missing `{name}` required by interface `{}`",
                            self.classes[id].name, self.ifaces[iid].name
                        ),
                        pos,
                    );
                }
            }
        }
    }

    fn check_var(&mut self, v: &VarStmt) {
        for b in &v.bindings {
            let declared = b.ty.as_ref().map(|t| self.resolve_type(t));
            let init_ty = b.init.as_ref().map(|e| self.check_expr(e, declared.as_ref()));
            let ty = match (&declared, init_ty) {
                (Some(d), Some(i)) => {
                    self.require_assignable(&i, d, pos_of(b.init.as_ref().unwrap()), "initializer");
                    d.clone()
                }
                (Some(d), None) => d.clone(),
                (None, Some(i)) => {
                    if matches!(i, Ty::Null) {
                        self.error(
                            Code::TypeMismatch,
                            "cannot infer a type from `null`; add an annotation",
                            pattern_pos(&b.target),
                        );
                        Ty::Err
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
                    Ty::Err
                }
            };
            self.bind_pattern_ty(&b.target, &ty, v.kind == VarKind::Const);
        }
    }

    fn bind_pattern_ty(&mut self, p: &Pattern, ty: &Ty, is_const: bool) {
        match p {
            Pattern::Name(n) => {
                self.define(&n.text, ty.clone(), is_const);
            }
            Pattern::Array { elems, rest } => {
                let elem = match strip_null(ty) {
                    Ty::Array(e) => e.as_ref().clone(),
                    Ty::Str => Ty::Char,
                    Ty::Tuple(_) | Ty::Any | Ty::Err => Ty::Any, // tuples positional below
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!("cannot destructure `{}` as an array", self.show(&other)),
                            pattern_pos(p),
                        );
                        Ty::Err
                    }
                };
                for (i, e) in elems.iter().enumerate() {
                    let et = match strip_null(ty) {
                        Ty::Tuple(ts) => ts.get(i).cloned().unwrap_or(Ty::Err),
                        _ => elem.clone(),
                    };
                    // A default value removes nullability.
                    let et = if e.default.is_some() { strip_null(&et) } else { et };
                    self.bind_pattern_ty(&e.target, &et, is_const);
                }
                if let Some(r) = rest {
                    self.bind_pattern_ty(r, &Ty::Array(Rc::new(elem)), is_const);
                }
            }
            Pattern::Record(fields) => {
                for f in fields {
                    let ft = self
                        .member_type_quiet(&strip_null(ty), &f.name.text)
                        .unwrap_or(Ty::Any);
                    let ft = if f.default.is_some() { strip_null(&ft) } else { ft };
                    match &f.target {
                        Some(t) => self.bind_pattern_ty(t, &ft, is_const),
                        None => self.define(&f.name.text, ft, is_const),
                    }
                }
            }
        }
    }

    fn check_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Block(b) => {
                self.push_scope();
                for s in b {
                    self.check_stmt(s);
                }
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
            Stmt::For { init, cond, step, body } => {
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
            Stmt::ForOf { kind, target, ty, iter, body, .. } => {
                self.push_scope();
                let it = self.check_expr(iter, None);
                let elem = match strip_null(&it) {
                    Ty::Array(e) => e.as_ref().clone(),
                    Ty::Str => Ty::Char,
                    Ty::Any | Ty::Err => Ty::Any,
                    // Host iterables (NodeList, HTMLCollection, …): the IDL
                    // element type isn't tracked, so annotate to refine.
                    Ty::Iface(..) => Ty::Any,
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!(
                                "`for of` needs an array, string, or host iterable, got `{}`",
                                self.show(&other)
                            ),
                            pos_of(iter),
                        );
                        Ty::Err
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
                let want = self.ret_ty.clone().unwrap_or(Ty::Void);
                match value {
                    Some(e) => {
                        let t = self.check_expr(e, Some(&want));
                        if matches!(want, Ty::Void) {
                            self.error(
                                Code::BadReturn,
                                "this function returns `void`; remove the value",
                                *pos,
                            );
                        } else {
                            self.require_assignable(&t, &want, pos_of(e), "return value");
                        }
                    }
                    None => {
                        if !matches!(want, Ty::Void | Ty::Err | Ty::Any) {
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
                        format!("only `Error` subclasses may be thrown, got `{}`", self.show(&t)),
                        pos_of(e),
                    );
                }
            }
            Stmt::Try { block, catches, finally } => {
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
                            format!("catch type must be an `Error` subclass, got `{}`", self.show(&ct)),
                            c.name.pos,
                        );
                    }
                    self.push_scope();
                    self.define(&c.name.text, ct, false);
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

    fn is_error_class(&self, t: &Ty) -> bool {
        match t {
            Ty::Class(id, _) => {
                let mut cur = Some(*id);
                while let Some(c) = cur {
                    if c == self.error_id {
                        return true;
                    }
                    cur = self.classes[c].parent.as_ref().map(|(p, _)| *p);
                }
                false
            }
            Ty::Any | Ty::Err => true,
            _ => false,
        }
    }

    fn check_condition(&mut self, e: &Expr) {
        let t = self.check_expr(e, None);
        match strip_narrow_helpers(&t) {
            Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Any | Ty::Err => {}
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

    /// `(then, else)` narrowing maps from a condition.
    fn narrow_from(&mut self, cond: &Expr) -> (HashMap<String, Ty>, HashMap<String, Ty>) {
        let mut then = HashMap::new();
        let mut els = HashMap::new();
        if let Expr::Binary { op, l, r } = cond {
            let (ident, other) = match (l.as_ref(), r.as_ref()) {
                (Expr::Ident(n), o) | (o, Expr::Ident(n)) => (Some(n), o),
                _ => (None, l.as_ref()),
            };
            if let Some(n) = ident {
                if matches!(other, Expr::Lit { kind: LitKind::Null, .. }) {
                    if let Some(v) = self.lookup(&n.text) {
                        if let Ty::Nullable(inner) = &v.ty {
                            match op {
                                BinOp::Ne => {
                                    then.insert(n.text.clone(), inner.as_ref().clone());
                                    els.insert(n.text.clone(), Ty::Null);
                                }
                                BinOp::Eq => {
                                    els.insert(n.text.clone(), inner.as_ref().clone());
                                    then.insert(n.text.clone(), Ty::Null);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        (then, els)
    }

    // ---- expressions ------------------------------------------------------------------

    fn check_expr(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        let t = self.check_expr_inner(e, expected);
        // Unsuffixed integer literals adapt to the expected integer type
        // when the value fits (spec §2.6); overflow is E0110, not a
        // mismatch.
        if let (Some(want), Ty::Int(IntKind::I32)) = (expected, &t) {
            if let Ty::Int(k) = strip_null(want) {
                if k != IntKind::I32 {
                    if let Some(v) = unsuffixed_int_value(e) {
                        return if int_fits(v, k) {
                            Ty::Int(k)
                        } else {
                            self.error(
                                Code::IntOutOfRange,
                                format!("literal `{v}` does not fit `{}`", k.name()),
                                pos_of(e),
                            );
                            Ty::Int(k)
                        };
                    }
                }
            }
        }
        t
    }

    fn check_expr_inner(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        match e {
            Expr::Ident(n) => self.lookup(&n.text).map(|v| v.ty).unwrap_or(Ty::Err),
            Expr::This(pos) => match self.current_class {
                Some(id) if !self.in_static => self.class_self_type(id),
                Some(_) => {
                    self.error(
                        Code::TypeMismatch,
                        "`this` is not available in a static member",
                        *pos,
                    );
                    Ty::Err
                }
                None => Ty::Err, // binder reported
            },
            Expr::Lit { kind, text, .. } => self.literal_ty(*kind, text),
            Expr::Template(parts) => {
                for p in parts {
                    if let TplPart::Expr(e) = p {
                        let t = self.check_expr(e, None);
                        if matches!(t, Ty::Void) {
                            self.error(
                                Code::TypeMismatch,
                                "cannot interpolate `void`",
                                pos_of(e),
                            );
                        }
                    }
                }
                Ty::Str
            }
            Expr::Array(elems) => {
                // Tuple context: check positionally and produce the tuple.
                if let Some(Ty::Tuple(ts)) = expected.map(strip_null) {
                    if ts.len() == elems.len() && elems.iter().all(|e| !e.spread) {
                        for (el, want) in elems.iter().zip(ts.iter()) {
                            let t = self.check_expr(&el.expr, Some(want));
                            self.require_assignable(&t, want, pos_of(&el.expr), "tuple element");
                        }
                        return Ty::Tuple(ts);
                    }
                }
                let want_elem = expected.map(strip_null).and_then(|t| match t {
                    Ty::Array(e) => Some(e.as_ref().clone()),
                    _ => None,
                });
                let mut unified: Option<Ty> = want_elem.clone();
                for el in elems {
                    let want = if el.spread {
                        want_elem.clone().map(|e| Ty::Array(Rc::new(e)))
                    } else {
                        want_elem.clone()
                    };
                    let t = self.check_expr(&el.expr, want.as_ref());
                    let t = if el.spread {
                        match strip_null(&t) {
                            Ty::Array(e) => e.as_ref().clone(),
                            Ty::Any | Ty::Err => Ty::Any,
                            other => {
                                self.error(
                                    Code::TypeMismatch,
                                    format!("can only spread arrays, got `{}`", self.show(&other)),
                                    pos_of(&el.expr),
                                );
                                Ty::Err
                            }
                        }
                    } else {
                        t
                    };
                    unified = Some(match unified {
                        None => t,
                        Some(u) => self.unify_pair(u, t),
                    });
                }
                Ty::Array(Rc::new(unified.unwrap_or(Ty::Any)))
            }
            Expr::Record(fields) => {
                let mut fs = Vec::new();
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            let want = expected.map(strip_null).and_then(|t| {
                                self.member_type_quiet(&t, &name.text)
                            });
                            let t = match value {
                                Some(v) => self.check_expr(v, want.as_ref()),
                                None => self
                                    .lookup(&name.text)
                                    .map(|v| v.ty)
                                    .unwrap_or(Ty::Err),
                            };
                            fs.push(RecField { name: name.text.clone(), ty: t, optional: false });
                        }
                        RecordField::Spread(v) => {
                            let t = self.check_expr(v, None);
                            if let Ty::Record(inner) = strip_null(&t) {
                                for f in inner.iter() {
                                    fs.push(f.clone());
                                }
                            }
                        }
                    }
                }
                fs.sort_by(|a, b| a.name.cmp(&b.name));
                fs.dedup_by(|a, b| a.name == b.name);
                Ty::Record(Rc::new(fs))
            }
            Expr::Paren(inner) => self.check_expr(inner, expected),
            Expr::Arrow { params, ret, body, is_async: is_async_arrow } => {
                let ctx_fn = expected.map(strip_null).and_then(|t| match t {
                    Ty::Fn(f) => Some(f.clone()),
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
                            .unwrap_or(Ty::Any),
                    };
                    let bind_ty =
                        if p.rest { Ty::Array(Rc::new(ty.clone())) } else { ty.clone() };
                    self.bind_pattern_ty(&p.target, &bind_ty, false);
                    ptys.push(ParamTy { ty, optional: p.optional, rest: p.rest });
                }
                let declared_ret = ret.as_ref().map(|t| self.resolve_type(t));
                let want_ret =
                    declared_ret.clone().or_else(|| ctx_fn.as_ref().map(|f| f.ret.clone()));
                let actual_ret = match body {
                    ArrowBody::Expr(e) => self.check_expr(e, want_ret.as_ref()),
                    ArrowBody::Block(stmts) => {
                        let saved = self.ret_ty.replace(want_ret.clone().unwrap_or(Ty::Any));
                        for s in stmts {
                            self.check_stmt(s);
                        }
                        self.ret_ty = saved;
                        want_ret.clone().unwrap_or(Ty::Void)
                    }
                };
                self.pop_scope();
                let ret = declared_ret.or(want_ret).unwrap_or(actual_ret);
                let ret = if *is_async_arrow && self.unwrap_promise(&ret).is_none() {
                    self.promise_of(ret)
                } else {
                    ret
                };
                Ty::Fn(Rc::new(FnTy { tparams: vec![], params: ptys, ret }))
            }
            Expr::Unary { op, expr, pos } => {
                let t = self.check_expr(expr, None);
                match op {
                    UnaryOp::Not => {
                        match strip_narrow_helpers(&t) {
                            Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Any | Ty::Err => {}
                            other => self.error(
                                Code::BadOperand,
                                format!("`!` needs bool or numeric, got `{}`", self.show(&other)),
                                *pos,
                            ),
                        }
                        Ty::Bool
                    }
                    UnaryOp::Plus | UnaryOp::Neg => match strip_narrow_helpers(&t) {
                        n @ (Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::BigInt | Ty::BigDec) => n,
                        Ty::Any | Ty::Err => Ty::Err,
                        other => {
                            self.error(
                                Code::BadOperand,
                                format!("unary needs a number, got `{}`", self.show(&other)),
                                *pos,
                            );
                            Ty::Err
                        }
                    },
                    UnaryOp::BitNot => match strip_narrow_helpers(&t) {
                        n @ Ty::Int(_) => n,
                        Ty::Any | Ty::Err => Ty::Err,
                        other => {
                            self.error(
                                Code::BadOperand,
                                format!("`~` needs an integer, got `{}`", self.show(&other)),
                                *pos,
                            );
                            Ty::Err
                        }
                    },
                    UnaryOp::Await => match self.unwrap_promise(&t) {
                        Some(inner) => inner,
                        None => match strip_narrow_helpers(&t) {
                            // A host object may be a JS thenable; a plain
                            // value awaits to itself (as in JS).
                            Ty::Any | Ty::Err | Ty::Iface(..) => Ty::Any,
                            other => other,
                        },
                    },
                }
            }
            Expr::Update { expr, .. } => {
                let t = self.check_expr(expr, None);
                self.check_assignable_target(expr);
                match strip_narrow_helpers(&t) {
                    n @ (Ty::Int(_) | Ty::F32 | Ty::F64) => n,
                    Ty::Any | Ty::Err => Ty::Err,
                    other => {
                        self.error(
                            Code::BadOperand,
                            format!("`++`/`--` need a number, got `{}`", self.show(&other)),
                            pos_of(expr),
                        );
                        Ty::Err
                    }
                }
            }
            Expr::Binary { op, l, r } => self.check_binary(*op, l, r),
            Expr::Assign { op, target, value } => {
                // Assignment targets check against the *declared* type, not
                // a narrowed one (the assignment may widen back).
                let tt = match target.as_ref() {
                    Expr::Ident(n) => {
                        self.lookup_scope(&n.text).map(|v| v.ty).unwrap_or(Ty::Err)
                    }
                    _ => self.check_expr(target, None),
                };
                self.check_assignable_target(target);
                let vt = self.check_expr(value, Some(&tt));
                if *op == "=" {
                    self.require_assignable(&vt, &tt, pos_of(value), "assignment");
                } else if !matches!(tt, Ty::Any | Ty::Err) {
                    // Compound assignment: the operation must be valid; the
                    // result converts back to the target type with wrapping,
                    // as in C (`a += 1` on an int16 stays int16).
                    let res = self.check_binary_types(compound_op(op), &tt, &vt, pos_of(value));
                    let numeric = |t: &Ty| matches!(t, Ty::Int(_) | Ty::F32 | Ty::F64);
                    if !(numeric(&res) && numeric(&tt)) {
                        self.require_assignable(&res, &tt, pos_of(value), "compound assignment");
                    }
                }
                if let Expr::Ident(n) = target.as_ref() {
                    self.kill_narrow(&n.text);
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
            Expr::Call { callee, type_args, args, optional } => {
                self.check_call(callee, type_args, args, *optional)
            }
            Expr::New { ty, args } => self.check_new(ty, args),
            Expr::Member { obj, name, optional } => {
                let ot = self.check_expr(obj, None);
                self.member_access(&ot, name, *optional, pos_of(obj))
            }
            Expr::Index { obj, index, optional } => {
                let ot = self.check_expr(obj, None);
                let it = self.check_expr(index, None);
                let base = if *optional { strip_null(&ot) } else { ot.clone() };
                let host_obj = matches!(base, Ty::Iface(..));
                if !matches!(strip_narrow_helpers(&it), Ty::Int(_) | Ty::Any | Ty::Err)
                    && !(host_obj && matches!(strip_narrow_helpers(&it), Ty::Str))
                {
                    self.error(
                        Code::TypeMismatch,
                        format!("index must be an integer, got `{}`", self.show(&it)),
                        pos_of(index),
                    );
                }
                let out = match &base {
                    Ty::Array(e) => e.as_ref().clone(),
                    Ty::Str => Ty::Char,
                    Ty::Any | Ty::Err => Ty::Any,
                    // Host objects are indexable (`nodeList[0]`, `obj[key]`);
                    // the element type is not knowable from IDL.
                    Ty::Iface(..) => Ty::Any,
                    Ty::Nullable(_) => {
                        self.error(
                            Code::NullableMisuse,
                            "value may be null; use `?.[…]` or narrow first",
                            pos_of(obj),
                        );
                        Ty::Err
                    }
                    other => {
                        self.error(
                            Code::TypeMismatch,
                            format!("`{}` is not indexable", self.show(other)),
                            pos_of(obj),
                        );
                        Ty::Err
                    }
                };
                if *optional {
                    nullable(out)
                } else {
                    out
                }
            }
            Expr::SuperMember { name, pos } => {
                let Some(id) = self.current_class else { return Ty::Err };
                let Some((pid, pargs)) = self.classes[id].parent.clone() else {
                    return Ty::Err;
                };
                let parent_ty = Ty::Class(pid, Rc::new(pargs));
                self.member_access(&parent_ty, name, false, *pos)
            }
            Expr::SuperCall { args, pos } => {
                let Some(id) = self.current_class else { return Ty::Err };
                let Some((pid, pargs)) = self.classes[id].parent.clone() else {
                    return Ty::Err;
                };
                let map = self.subst_map(pid, &pargs);
                let params = self
                    .ctor_params(pid)
                    .into_iter()
                    .map(|p| ParamTy { ty: subst(&p.ty, &map), ..p })
                    .collect();
                let sig = FnTy { tparams: vec![], params, ret: Ty::Void };
                self.check_args_against(&sig, &[], args, *pos);
                Ty::Void
            }
            Expr::ImportCall(inner) => {
                self.check_expr(inner, Some(&Ty::Str));
                Ty::Any
            }
        }
    }

    fn ctor_params(&self, id: ClassId) -> Vec<ParamTy> {
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some((params, _)) = &self.classes[cid].ctor {
                return params.clone();
            }
            cur = self.classes[cid].parent.as_ref().map(|(p, _)| *p);
        }
        Vec::new()
    }

    fn literal_ty(&mut self, kind: LitKind, text: &str) -> Ty {
        match kind {
            LitKind::Null => Ty::Null,
            LitKind::Bool => Ty::Bool,
            LitKind::Str => Ty::Str,
            LitKind::Char => Ty::Char,
            LitKind::BigInt => Ty::BigInt,
            LitKind::BigDec => Ty::BigDec,
            LitKind::Float => {
                if text.ends_with('f') {
                    Ty::F32
                } else {
                    Ty::F64
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
                Ty::Int(kind)
            }
        }
    }

    // ---- operators ----------------------------------------------------------------

    fn check_binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                self.check_condition(l);
                self.check_condition(r);
                Ty::Bool
            }
            BinOp::Coalesce => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                match &lt {
                    Ty::Nullable(inner) => self.unify_pair(inner.as_ref().clone(), rt),
                    Ty::Null => rt,
                    Ty::Any | Ty::Err => Ty::Err,
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
                if !matches!(rt, Ty::ClassMeta(_) | Ty::Any | Ty::Err) {
                    self.error(
                        Code::BadOperand,
                        "right side of `instanceof` must be a class",
                        pos_of(r),
                    );
                }
                Ty::Bool
            }
            BinOp::Eq | BinOp::Ne => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, Some(&lt));
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
                Ty::Bool
            }
            _ => {
                let lt = self.check_expr(l, None);
                let rt = self.check_expr(r, None);
                self.check_binary_types(op, &lt, &rt, pos_of(r))
            }
        }
    }

    fn check_binary_types(&mut self, op: BinOp, lt: &Ty, rt: &Ty, pos: Pos) -> Ty {
        use BinOp::*;
        let l = strip_narrow_helpers(lt);
        let r = strip_narrow_helpers(rt);
        if matches!(l, Ty::Any | Ty::Err) || matches!(r, Ty::Any | Ty::Err) {
            return Ty::Err;
        }
        // string / char comparisons and concatenation
        match (&l, &r, op) {
            (Ty::Str, Ty::Str, Add) => return Ty::Str,
            (Ty::Str, Ty::Str, Lt | Gt | Le | Ge) => return Ty::Bool,
            (Ty::Char, Ty::Char, Lt | Gt | Le | Ge) => return Ty::Bool,
            // bigint/bigdec: exact arithmetic among themselves (§3.7); they
            // never mix implicitly with fixed-size numerics (§3.3).
            (Ty::BigInt, Ty::BigInt, Add | Sub | Mul | Div | Rem | Pow) => return Ty::BigInt,
            (Ty::BigDec, Ty::BigDec, Add | Sub | Mul | Div) => return Ty::BigDec,
            (Ty::BigInt, Ty::BigInt, Lt | Gt | Le | Ge) => return Ty::Bool,
            (Ty::BigDec, Ty::BigDec, Lt | Gt | Le | Ge) => return Ty::Bool,
            _ => {}
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
            return Ty::Err;
        };
        match op {
            Lt | Gt | Le | Ge => Ty::Bool,
            Shl | Shr | BitAnd | BitOr | BitXor | Rem => {
                if matches!(common, Ty::F32 | Ty::F64)
                    && matches!(op, Shl | Shr | BitAnd | BitOr | BitXor)
                {
                    self.error(
                        Code::BadOperand,
                        "bitwise operators need integer operands",
                        pos,
                    );
                    Ty::Err
                } else {
                    common
                }
            }
            _ => common,
        }
    }

    /// Usual arithmetic conversions (§3.3): promote small ints to int32,
    /// then wider rank wins, float wins, unsigned wins at equal rank.
    fn numeric_common(&self, a: &Ty, b: &Ty) -> Option<Ty> {
        let promote = |t: &Ty| -> Option<Ty> {
            Some(match t {
                Ty::Int(k) if k.bits() < 32 => Ty::Int(IntKind::I32),
                Ty::Int(k) => Ty::Int(*k),
                Ty::F32 => Ty::F32,
                Ty::F64 => Ty::F64,
                _ => return None,
            })
        };
        let (a, b) = (promote(a)?, promote(b)?);
        let rank = |t: &Ty| match t {
            Ty::Int(IntKind::I32) => 0,
            Ty::Int(IntKind::U32) => 1,
            Ty::Int(IntKind::I64) => 2,
            Ty::Int(IntKind::U64) => 3,
            Ty::F32 => 4,
            _ => 5,
        };
        Some(if rank(&a) >= rank(&b) { a } else { b })
    }

    fn comparable(&self, a: &Ty, b: &Ty) -> bool {
        let (a, b) = (strip_narrow_helpers(a), strip_narrow_helpers(b));
        if matches!(a, Ty::Any | Ty::Err) || matches!(b, Ty::Any | Ty::Err) {
            return true;
        }
        if self.numeric_common(&a, &b).is_some() {
            return true;
        }
        // null against nullable / null
        if matches!(a, Ty::Null) || matches!(b, Ty::Null) {
            return true; // nullability of the other side is the binder's TDZ story; == null is always meaningful
        }
        self.assignable(&a, &b) || self.assignable(&b, &a)
    }

    // ---- calls ------------------------------------------------------------------------

    fn check_call(
        &mut self,
        callee: &Expr,
        type_args: &[ast::Type],
        args: &[ArrayElem],
        optional: bool,
    ) -> Ty {
        // console/document natives get bespoke signatures.
        if let Expr::Member { obj, name, .. } = callee {
            let ot = self.check_expr(obj, None);
            match (&strip_null(&ot), name.as_str()) {
                (Ty::Namespace(Ns::Console), "log") => {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    return Ty::Void;
                }
                (Ty::Namespace(Ns::Document), "getElementById" | "createElement") => {
                    if let Some(a) = args.first() {
                        let t = self.check_expr(&a.expr, Some(&Ty::Str));
                        self.require_assignable(&t, &Ty::Str, pos_of(&a.expr), "argument");
                    }
                    return Ty::Class(self.element_id, Rc::new(vec![]));
                }
                (Ty::Namespace(Ns::Opaque), _) | (Ty::Any, _) => {
                    for a in args {
                        self.check_expr(&a.expr, None);
                    }
                    return Ty::Any;
                }
                _ => {
                    let fty = self.member_access(&ot, name, optional, pos_of(obj));
                    return self.invoke(&fty, type_args, args, pos_of(callee), optional);
                }
            }
        }
        let fty = self.check_expr(callee, None);
        self.invoke(&fty, type_args, args, pos_of(callee), optional)
    }

    fn invoke(
        &mut self,
        fty: &Ty,
        type_args: &[ast::Type],
        args: &[ArrayElem],
        pos: Pos,
        optional: bool,
    ) -> Ty {
        let base = if optional { strip_null(fty) } else { fty.clone() };
        match &base {
            Ty::Fn(f) => {
                let f = f.clone();
                let ret = self.check_args_against(&f, type_args, args, pos);
                if optional {
                    nullable(ret)
                } else {
                    ret
                }
            }
            Ty::Any | Ty::Err => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                Ty::Err
            }
            Ty::Nullable(_) => {
                self.error(
                    Code::NullableMisuse,
                    "value may be null; use `?.()` or narrow first",
                    pos,
                );
                Ty::Err
            }
            other => {
                self.error(
                    Code::BadCall,
                    format!("`{}` is not callable", self.show(other)),
                    pos,
                );
                Ty::Err
            }
        }
    }

    /// Check arguments against a signature; handles explicit type arguments
    /// and simple inference. Returns the (substituted) return type.
    fn check_args_against(
        &mut self,
        f: &FnTy,
        type_args: &[ast::Type],
        args: &[ArrayElem],
        pos: Pos,
    ) -> Ty {
        let mut map: HashMap<TvId, Ty> = HashMap::new();
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
        let positional: Vec<&ParamTy> = f.params.iter().filter(|p| !p.rest).collect();
        let required = positional.iter().filter(|p| !p.optional).count();
        let max = if rest.is_some() { usize::MAX } else { positional.len() };
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
                .unwrap_or(Ty::Any);
            let want_spread = if a.spread { Ty::Array(Rc::new(want_raw.clone())) } else { want_raw };
            // Infer type params not yet fixed, from this argument.
            let hint = subst(&want_spread, &map);
            let at = self.check_expr(&a.expr, Some(&hint));
            if !f.tparams.is_empty() {
                unify_infer(&want_spread, &at, &f.tparams, &mut map);
            }
            let want = subst(&want_spread, &map);
            self.require_assignable(&at, &want, pos_of(&a.expr), "argument");
        }
        // Any unfixed type params default to Any (checker v1).
        for tv in &f.tparams {
            map.entry(*tv).or_insert(Ty::Any);
        }
        subst(&f.ret, &map)
    }

    fn check_new(&mut self, ty: &ast::Type, args: &[ArrayElem]) -> Ty {
        let t = self.resolve_type(ty);
        let (id, targs) = match &t {
            Ty::Class(id, args) => (*id, args.as_ref().clone()),
            // `new WebSocket(url)`, `new Uint8Array(4)`: web-platform
            // constructors are interfaces, built through the bridge.
            Ty::Iface(..) => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                return t;
            }
            Ty::Any | Ty::Err => {
                for a in args {
                    self.check_expr(&a.expr, None);
                }
                return Ty::Err;
            }
            other => {
                self.error(
                    Code::BadCall,
                    format!("`new` needs a class, got `{}`", self.show(other)),
                    type_pos(ty),
                );
                return Ty::Err;
            }
        };
        if self.classes[id].is_abstract {
            self.error(
                Code::BadCall,
                format!("cannot instantiate abstract class `{}`", self.classes[id].name),
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
            .map(|p| ParamTy { ty: subst(&p.ty, &map), ..p })
            .collect();
        let sig = FnTy { tparams: vec![], params, ret: Ty::Void };
        self.check_args_against(&sig, &[], args, type_pos(ty));
        t
    }

    // ---- members & access control -----------------------------------------------------

    fn subst_map(&self, id: ClassId, args: &[Ty]) -> HashMap<TvId, Ty> {
        self.classes[id].tparams.iter().copied().zip(args.iter().cloned()).collect()
    }

    fn field_info(&self, id: ClassId, name: &str) -> Option<(Ty, Access, bool, ClassId)> {
        let mut cur = Some((id, Vec::new()));
        let mut acc_map: HashMap<TvId, Ty> = HashMap::new();
        while let Some((cid, _)) = cur {
            if let Some(f) = self.classes[cid].fields.iter().find(|f| f.name == name) {
                return Some((subst(&f.ty, &acc_map), f.access, f.readonly, cid));
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Ty> =
                        pargs.iter().map(|t| subst(t, &acc_map)).collect();
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
    ) -> Option<(Ty, Access, ClassId)> {
        let mut cur = Some(id);
        let mut acc_map: HashMap<TvId, Ty> = HashMap::new();
        while let Some(cid) = cur {
            let list = if setter { &self.classes[cid].setters } else { &self.classes[cid].getters };
            if let Some(a) = list.iter().find(|a| a.name == name) {
                return Some((subst(&a.ty, &acc_map), a.access, cid));
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Ty> =
                        pargs.iter().map(|t| subst(t, &acc_map)).collect();
                    acc_map = self.subst_map(*pid, &substituted);
                    cur = Some(*pid);
                }
                None => cur = None,
            }
        }
        None
    }

    fn method_sig(&self, id: ClassId, name: &str, want_static: bool) -> Option<FnTy> {
        let mut cur = Some(id);
        let mut acc_map: HashMap<TvId, Ty> = HashMap::new();
        while let Some(cid) = cur {
            if let Some(m) = self.classes[cid]
                .methods
                .iter()
                .find(|m| m.name == name && m.is_static == want_static)
            {
                return Some(FnTy {
                    tparams: m.sig.tparams.clone(),
                    params: m
                        .sig
                        .params
                        .iter()
                        .map(|p| ParamTy { ty: subst(&p.ty, &acc_map), ..p.clone() })
                        .collect(),
                    ret: subst(&m.sig.ret, &acc_map),
                });
            }
            match &self.classes[cid].parent {
                Some((pid, pargs)) => {
                    let substituted: Vec<Ty> =
                        pargs.iter().map(|t| subst(t, &acc_map)).collect();
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
    fn member_type_quiet(&mut self, t: &Ty, name: &str) -> Option<Ty> {
        match t {
            Ty::Record(fs) => fs.iter().find(|f| f.name == name).map(|f| f.ty.clone()),
            Ty::Class(id, args) => {
                let map = self.subst_map(*id, args);
                self.field_info(*id, name)
                    .map(|(t, ..)| subst(&t, &map))
                    .or_else(|| {
                        self.method_sig(*id, name, false)
                            .map(|s| Ty::Fn(Rc::new(FnTy {
                                tparams: s.tparams.clone(),
                                params: s
                                    .params
                                    .iter()
                                    .map(|p| ParamTy { ty: subst(&p.ty, &map), ..p.clone() })
                                    .collect(),
                                ret: subst(&s.ret, &map),
                            })))
                    })
            }
            Ty::Any | Ty::Err => Some(Ty::Any),
            _ => None,
        }
    }

    fn member_access(&mut self, ot: &Ty, name: &str, optional: bool, pos: Pos) -> Ty {
        let base = if optional { strip_null(ot) } else { ot.clone() };
        let out = match &base {
            Ty::Nullable(_) => {
                self.error(
                    Code::NullableMisuse,
                    format!("value may be null; use `?.{name}` or narrow first"),
                    pos,
                );
                Ty::Err
            }
            Ty::Null => {
                self.error(Code::NullableMisuse, "value is null here", pos);
                Ty::Err
            }
            Ty::Any | Ty::Err => Ty::Any,
            Ty::Str => match name {
                "length" => Ty::Int(IntKind::I32),
                "toString" => to_string_fn(),
                _ => self.no_member("string", name, pos),
            },
            Ty::Char | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::BigInt | Ty::BigDec
            | Ty::Enum(_) => match name {
                "toString" => to_string_fn(),
                _ => self.no_member(&self.show(&base), name, pos),
            },
            Ty::Array(elem) => match name {
                "length" => Ty::Int(IntKind::I32),
                "push" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![ParamTy {
                        ty: elem.as_ref().clone(),
                        optional: false,
                        rest: true,
                    }],
                    ret: Ty::Void,
                })),
                "pop" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![],
                    ret: nullable(elem.as_ref().clone()),
                })),
                "keys" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![],
                    ret: Ty::Array(Rc::new(Ty::Int(IntKind::I32))),
                })),
                "join" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![ParamTy { ty: Ty::Str, optional: true, rest: false }],
                    ret: Ty::Str,
                })),
                "toString" => to_string_fn(),
                _ => self.no_member("array", name, pos),
            },
            Ty::Record(fs) => match fs.iter().find(|f| f.name == name) {
                Some(f) => {
                    if f.optional {
                        nullable(f.ty.clone())
                    } else {
                        f.ty.clone()
                    }
                }
                None => self.no_member("record", name, pos),
            },
            Ty::Class(id, args) => {
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
                    Ty::Fn(Rc::new(FnTy {
                        tparams: sig.tparams.clone(),
                        params: sig
                            .params
                            .iter()
                            .map(|p| ParamTy { ty: subst(&p.ty, &map), ..p.clone() })
                            .collect(),
                        ret: subst(&sig.ret, &map),
                    }))
                } else {
                    self.no_member(&self.classes[id].name.clone(), name, pos)
                }
            }
            Ty::Iface(id, args) => {
                let id = *id;
                let imap: HashMap<TvId, Ty> = self.ifaces[id]
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
            Ty::ClassMeta(id) => {
                let id = *id;
                if let Some(f) =
                    self.classes[id].fields.iter().find(|f| f.name == name && f.is_static)
                {
                    let (t, access) = (f.ty.clone(), f.access);
                    self.check_access(access, id, pos, &format!("static field `{name}`"));
                    t
                } else if let Some(sig) = self.method_sig(id, name, true) {
                    let access = self.method_access(id, name);
                    self.check_access(access, id, pos, &format!("static method `{name}`"));
                    Ty::Fn(Rc::new(sig))
                } else {
                    self.no_member(&format!("class {}", self.classes[id].name), name, pos)
                }
            }
            Ty::EnumMeta(id) => {
                let id = *id;
                if self.enums[id].members.iter().any(|m| m == name) {
                    Ty::Enum(id)
                } else {
                    self.no_member(&self.enums[id].name.clone(), name, pos)
                }
            }
            Ty::Namespace(Ns::Console) => match name {
                "log" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![ParamTy { ty: Ty::Any, optional: false, rest: true }],
                    ret: Ty::Void,
                })),
                _ => self.no_member("console", name, pos),
            },
            Ty::Namespace(Ns::Document) => match name {
                "getElementById" | "createElement" => Ty::Fn(Rc::new(FnTy {
                    tparams: vec![],
                    params: vec![ParamTy { ty: Ty::Str, optional: false, rest: false }],
                    ret: Ty::Class(self.element_id, Rc::new(vec![])),
                })),
                _ => self.no_member("document", name, pos),
            },
            Ty::Namespace(Ns::Opaque) => Ty::Any,
            _ => self.no_member(&self.show(&base), name, pos),
        };
        if optional && matches!(ot, Ty::Nullable(_)) {
            nullable(out)
        } else {
            out
        }
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

    fn iface_member(&self, id: IfaceId, name: &str) -> Option<(Ty, bool)> {
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

    fn no_member(&mut self, on: &str, name: &str, pos: Pos) -> Ty {
        let msg = match name {
            "prototype" | "__proto__" => {
                format!("`{name}` does not exist: Mersey has no prototypes (§1.1, §4.1)")
            }
            "constructor" => "the constructor is not a reachable member (§4.1)".to_string(),
            _ => format!("no member `{name}` on `{on}`"),
        };
        self.error(Code::UnknownMember, msg, pos);
        Ty::Err
    }

    fn check_assignable_target(&mut self, target: &Expr) {
        match target {
            Expr::Ident(_) | Expr::Index { .. } => {} // const-ness is the binder's E0304
            Expr::Member { obj, name, .. } => {
                // readonly + setter access checks.
                let ot = self.check_expr(obj, None);
                match strip_null(&ot) {
                    Ty::Class(id, _) => {
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
                        } else if let Some((_, access, owner)) =
                            self.accessor_info(id, name, true)
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
                    Ty::Record(_) | Ty::Any | Ty::Err | Ty::ClassMeta(_) => {}
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---- casts ---------------------------------------------------------------------

    fn check_cast(&mut self, from: &Ty, to: &Ty, wrapping: bool, pos: Pos) {
        // A cast to a non-nullable type is also the null assertion
        // (`document.body as Element`); nullability is checked at runtime.
        let f = match (strip_narrow_helpers(from), to) {
            (Ty::Nullable(inner), t) if !matches!(t, Ty::Nullable(_)) => inner.as_ref().clone(),
            (f, _) => f,
        };
        let t = strip_narrow_helpers(to);
        let numeric =
            |x: &Ty| matches!(x, Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Char);
        if matches!(f, Ty::Any | Ty::Err) || matches!(t, Ty::Any | Ty::Err) {
            return;
        }
        if wrapping {
            if !(numeric(&f) && matches!(t, Ty::Int(_))) {
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

    fn require_assignable(&mut self, from: &Ty, to: &Ty, pos: Pos, what: &str) {
        if !self.assignable(from, to) {
            self.error(
                Code::TypeMismatch,
                format!("{what}: `{}` is not assignable to `{}`", self.show(from), self.show(to)),
                pos,
            );
        }
    }

    fn assignable(&self, from: &Ty, to: &Ty) -> bool {
        use Ty::*;
        match (from, to) {
            (Err | Any, _) | (_, Err | Any) => true,
            (Void, Void) => true,
            (Null, Null | Nullable(_)) => true,
            (Nullable(a), Nullable(b)) => self.assignable(a, b),
            (t, Nullable(b)) => self.assignable(t, b),
            (Bool, Bool) | (Char, Char) | (Str, Str) | (BigInt, BigInt) | (BigDec, BigDec) => {
                true
            }
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
            (Array(a), Array(b)) => self.ty_eq(a, b) || matches!(b.as_ref(), Ty::Any),
            (Tuple(a), Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.assignable(x, y))
            }
            (Record(a), Record(b)) => b.iter().all(|bf| {
                match a.iter().find(|af| af.name == bf.name) {
                    Some(af) => self.assignable(&af.ty, &bf.ty),
                    None => bf.optional,
                }
            }),
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
                    && (matches!(b.ret, Void)
                        || matches!(a.ret, Void) && matches!(b.ret, Void)
                        || self.assignable(&a.ret, &b.ret))
            }
            (Class(a, aargs), Class(b, bargs)) => {
                let mut cur = Some((*a, aargs.as_ref().clone()));
                while let Some((cid, cargs)) = cur {
                    if cid == *b {
                        return cargs.len() == bargs.len()
                            && cargs.iter().zip(bargs.iter()).all(|(x, y)| self.ty_eq(x, y));
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
                let mut cur = Some((*a, aargs.as_ref().clone()));
                while let Some((cid, cargs)) = cur {
                    let map = self.subst_map(cid, &cargs);
                    for (iid, iargs) in &self.classes[cid].ifaces {
                        let ia: Vec<Ty> = iargs.iter().map(|t| subst(t, &map)).collect();
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

    fn iface_extends(&self, a: IfaceId, aargs: &[Ty], b: IfaceId, bargs: &[Ty]) -> bool {
        if a == b {
            return aargs.len() == bargs.len()
                && aargs.iter().zip(bargs.iter()).all(|(x, y)| self.ty_eq(x, y));
        }
        let map: HashMap<TvId, Ty> =
            self.ifaces[a].tparams.iter().copied().zip(aargs.iter().cloned()).collect();
        for (pid, pargs) in &self.ifaces[a].extends {
            let pa: Vec<Ty> = pargs.iter().map(|t| subst(t, &map)).collect();
            if self.iface_extends(*pid, &pa, b, bargs) {
                return true;
            }
        }
        false
    }

    fn ty_eq(&self, a: &Ty, b: &Ty) -> bool {
        self.assignable(a, b) && self.assignable(b, a)
    }

    fn unify_pair(&mut self, a: Ty, b: Ty) -> Ty {
        if self.assignable(&b, &a) {
            return a;
        }
        if self.assignable(&a, &b) {
            return b;
        }
        if matches!(a, Ty::Null) {
            return nullable(b);
        }
        if matches!(b, Ty::Null) {
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
        Expr::Lit { kind: LitKind::Int, text, .. } => {
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
        Expr::Unary { op: UnaryOp::Neg, expr, .. } => unsuffixed_int_value(expr).map(|v| -v),
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

fn nullable(t: Ty) -> Ty {
    match t {
        Ty::Nullable(_) | Ty::Null | Ty::Any | Ty::Err => t,
        other => Ty::Nullable(Rc::new(other)),
    }
}

fn strip_null(t: &Ty) -> Ty {
    match t {
        Ty::Nullable(inner) => inner.as_ref().clone(),
        other => other.clone(),
    }
}

fn strip_narrow_helpers(t: &Ty) -> Ty {
    t.clone()
}

fn fold_union(tys: Vec<Ty>) -> Ty {
    let mut has_null = false;
    let mut arms: Vec<Ty> = Vec::new();
    for t in tys {
        match t {
            Ty::Null => has_null = true,
            Ty::Nullable(inner) => {
                has_null = true;
                arms.push(inner.as_ref().clone());
            }
            other => arms.push(other),
        }
    }
    let base = if arms.len() == 1 { arms.pop().unwrap() } else { Ty::Union(Rc::new(arms)) };
    if has_null {
        nullable(base)
    } else {
        base
    }
}

fn to_string_fn() -> Ty {
    Ty::Fn(Rc::new(FnTy { tparams: vec![], params: vec![], ret: Ty::Str }))
}

fn subst(t: &Ty, map: &HashMap<TvId, Ty>) -> Ty {
    if map.is_empty() {
        return t.clone();
    }
    match t {
        Ty::Var(tv) => map.get(tv).cloned().unwrap_or(Ty::Var(*tv)),
        Ty::Nullable(inner) => nullable(subst(inner, map)),
        Ty::Array(e) => Ty::Array(Rc::new(subst(e, map))),
        Ty::Tuple(ts) => Ty::Tuple(Rc::new(ts.iter().map(|t| subst(t, map)).collect())),
        Ty::Record(fs) => Ty::Record(Rc::new(
            fs.iter()
                .map(|f| RecField { name: f.name.clone(), ty: subst(&f.ty, map), optional: f.optional })
                .collect(),
        )),
        Ty::Fn(f) => Ty::Fn(Rc::new(FnTy {
            tparams: f.tparams.clone(),
            params: f
                .params
                .iter()
                .map(|p| ParamTy { ty: subst(&p.ty, map), ..p.clone() })
                .collect(),
            ret: subst(&f.ret, map),
        })),
        Ty::Class(id, args) => {
            Ty::Class(*id, Rc::new(args.iter().map(|a| subst(a, map)).collect()))
        }
        Ty::Iface(id, args) => {
            Ty::Iface(*id, Rc::new(args.iter().map(|a| subst(a, map)).collect()))
        }
        Ty::Union(arms) => fold_union(arms.iter().map(|a| subst(a, map)).collect()),
        other => other.clone(),
    }
}

/// One-pass inference: bind free `tparams` in `want` from `got`.
fn unify_infer(want: &Ty, got: &Ty, tparams: &[TvId], out: &mut HashMap<TvId, Ty>) {
    match (want, got) {
        (Ty::Var(tv), g) if tparams.contains(tv) => {
            out.entry(*tv).or_insert_with(|| g.clone());
        }
        (Ty::Nullable(a), Ty::Nullable(b)) => unify_infer(a, b, tparams, out),
        (Ty::Nullable(a), b) => unify_infer(a, b, tparams, out),
        (Ty::Array(a), Ty::Array(b)) => unify_infer(a, b, tparams, out),
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            for (x, y) in a.iter().zip(b.iter()) {
                unify_infer(x, y, tparams, out);
            }
        }
        (Ty::Fn(a), Ty::Fn(b)) => {
            for (x, y) in a.params.iter().zip(b.params.iter()) {
                unify_infer(&x.ty, &y.ty, tparams, out);
            }
            unify_infer(&a.ret, &b.ret, tparams, out);
        }
        (Ty::Class(i, a), Ty::Class(j, b)) | (Ty::Iface(i, a), Ty::Iface(j, b)) if i == j => {
            for (x, y) in a.iter().zip(b.iter()) {
                unify_infer(x, y, tparams, out);
            }
        }
        _ => {}
    }
}

// ---- position helpers ------------------------------------------------------------

fn pos_of(e: &Expr) -> Pos {
    match e {
        Expr::Ident(n) => n.pos,
        Expr::This(p) => *p,
        Expr::Unary { pos, .. } => *pos,
        Expr::SuperMember { pos, .. } | Expr::SuperCall { pos, .. } => *pos,
        Expr::Paren(inner) | Expr::ImportCall(inner) => pos_of(inner),
        Expr::Update { expr, .. } | Expr::Cast { expr, .. } => pos_of(expr),
        Expr::Binary { l, .. } | Expr::Assign { target: l, .. } | Expr::Cond { cond: l, .. } => {
            pos_of(l)
        }
        Expr::Call { callee, .. } => pos_of(callee),
        Expr::Member { obj, .. } | Expr::Index { obj, .. } => pos_of(obj),
        Expr::New { ty, .. } => type_pos(ty),
        Expr::Template(parts) =>

            parts
                .iter()
                .find_map(|p| match p {
                    TplPart::Expr(e) => Some(pos_of(e)),
                    _ => None,
                })
                .unwrap_or(Pos { line: 0, col: 0 }),
        Expr::Array(elems) => {
            elems.first().map(|e| pos_of(&e.expr)).unwrap_or(Pos { line: 0, col: 0 })
        }
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
        Pattern::Record(fs) => fs.first().map(|f| f.name.pos).unwrap_or(Pos { line: 0, col: 0 }),
    }
}

fn type_pos(t: &ast::Type) -> Pos {
    match t {
        ast::Type::Named { pos, .. } => *pos,
        ast::Type::Nullable(inner) | ast::Type::ArrayOf(inner) => type_pos(inner),
        ast::Type::Union(arms) => arms.first().map(type_pos).unwrap_or(Pos { line: 0, col: 0 }),
        ast::Type::Tuple(ts) => ts.first().map(type_pos).unwrap_or(Pos { line: 0, col: 0 }),
        ast::Type::Record(_) => Pos { line: 0, col: 0 },
        ast::Type::Function { ret, .. } => type_pos(ret),
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
