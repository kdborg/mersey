//! MVP tree-walking interpreter. Executes a bound module directly from the
//! AST. This is the Phase-"MVP" execution engine; the typed-bytecode VM and
//! JIT (ROADMAP Phases 2/4) replace it without changing observable behavior
//! — the runtime conformance suite is the contract.
//!
//! Semantics honored from the spec: UTF-32 strings with O(1) code-point
//! indexing (§3.4), C-style numeric promotion with defined wrapping (§3.3,
//! §3.6: division by zero and `INT_MIN / -1` throw `RangeError`, shift
//! counts masked), checked vs `wrapping` casts, sealed class shapes (§4.1:
//! assigning an undeclared field throws), class-chain method dispatch with
//! `super`, and module-level declaration hoisting (§4.5).
//!
//! Deliberately out of MVP scope (clean `TypeError` at runtime, tracked in
//! ROADMAP): `bigint`/`bigdec` arithmetic, `async`/`await`, dynamic
//! `import()`, multi-module graphs.
//!
//! The AST is borrowed with `&'static` lifetime; drivers leak one parsed
//! module per program (bounded, lives for the process/page lifetime).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mersey_front::ast::*;

// ---- host interface ---------------------------------------------------------

/// Everything the interpreter can ask of the outside world. The CLI wires
/// this to stdout; the browser build wires it to `console`/DOM via the
/// loader (docs/architecture/browser-integration.md, Stage A).
pub trait Host {
    fn print(&mut self, s: &str);
    fn dom_set_text(&mut self, id: &str, text: &str);
    fn dom_get_text(&mut self, id: &str) -> Option<String>;
    /// Register callback `cb` (an index the driver passes back to
    /// `Interp::invoke_callback`) for click events on element `id`.
    fn dom_on_click(&mut self, id: &str, cb: u32);
}

// ---- values -------------------------------------------------------------------

type Str = Rc<Vec<char>>;

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    I32(i32),
    I64(i64),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Char(char),
    Str(Str),
    Array(Rc<RefCell<Vec<Value>>>),
    Record(Rc<RefCell<HashMap<String, Value>>>),
    Closure(Rc<Closure>),
    Class(Rc<ClassDef>),
    Instance(Rc<RefCell<Instance>>),
    /// `console`, `document`, enum objects: named bags of values.
    Namespace(Rc<Namespace>),
    /// A DOM element handle (Stage A: identified by element id).
    Dom(Rc<String>),
    Native(&'static str),
}

pub struct Namespace {
    pub name: String,
    pub entries: HashMap<String, Value>,
}

pub struct Closure {
    data: Rc<FnData>,
    env: Env,
    this: Option<Value>,
    /// Class that lexically contains the function (for `super`).
    cls: Option<Rc<ClassDef>>,
}

struct FnData {
    #[allow(dead_code)] // future stack traces
    name: String,
    is_async: bool,
    params: &'static [Param],
    body: FnBody,
}

enum FnBody {
    Block(&'static [Stmt]),
    Expr(&'static Expr),
}

pub struct ClassDef {
    name: String,
    parent: Option<Rc<ClassDef>>,
    /// Instance fields in initialization order (base-class fields first).
    fields: Vec<(String, Option<&'static Expr>)>,
    methods: HashMap<String, Rc<FnData>>,
    getters: HashMap<String, Rc<FnData>>,
    setters: HashMap<String, Rc<FnData>>,
    ctor: Option<Rc<FnData>>,
    statics: RefCell<HashMap<String, Value>>,
    static_methods: HashMap<String, Rc<FnData>>,
    /// Built-in error classes construct without an AST ctor.
    is_builtin_error: bool,
    env: Option<Env>,
}

pub struct Instance {
    class: Rc<ClassDef>,
    fields: HashMap<String, Value>,
}

// ---- environments ----------------------------------------------------------------

type Env = Rc<RefCell<Scope>>;

struct Scope {
    vars: HashMap<String, Value>,
    parent: Option<Env>,
}

fn child_env(parent: &Env) -> Env {
    Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: Some(parent.clone()) }))
}

fn env_get(env: &Env, name: &str) -> Option<Value> {
    let scope = env.borrow();
    if let Some(v) = scope.vars.get(name) {
        return Some(v.clone());
    }
    scope.parent.as_ref().and_then(|p| env_get(p, name))
}

fn env_set(env: &Env, name: &str, value: Value) -> bool {
    let mut scope = env.borrow_mut();
    if let Some(slot) = scope.vars.get_mut(name) {
        *slot = value;
        return true;
    }
    match &scope.parent {
        Some(p) => env_set(p, name, value),
        None => false,
    }
}

fn env_define(env: &Env, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// ---- control flow / errors ---------------------------------------------------------

enum Sig {
    Normal,
    Return(Value),
    Break(Option<String>),
    Continue(Option<String>),
}

enum LoopCtl {
    BreakLoop,
    NextIter,
    Out(Sig),
}

fn loop_ctl(sig: Sig, label: Option<&str>) -> LoopCtl {
    match sig {
        Sig::Normal | Sig::Continue(None) => LoopCtl::NextIter,
        Sig::Break(None) => LoopCtl::BreakLoop,
        Sig::Continue(Some(l)) if Some(l.as_str()) == label => LoopCtl::NextIter,
        Sig::Break(Some(l)) if Some(l.as_str()) == label => LoopCtl::BreakLoop,
        other => LoopCtl::Out(other),
    }
}

/// A runtime error is always a thrown value (built-in errors are instances
/// of the built-in `Error` classes, so `catch (e: RangeError)` works).
pub struct Thrown(pub Value);

type VResult = Result<Value, Thrown>;
type SResult = Result<Sig, Thrown>;

// ---- interpreter ------------------------------------------------------------------

pub struct Interp {
    host: Box<dyn Host>,
    globals: Env,
    callbacks: Vec<Value>,
    error_classes: HashMap<&'static str, Rc<ClassDef>>,
    /// Class whose method is currently executing (innermost last), for `super`.
    class_stack: Vec<Rc<ClassDef>>,
}

pub fn new_interp(host: Box<dyn Host>) -> Interp {
    let globals = Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: None }));
    let mut error_classes = HashMap::new();
    let base = Rc::new(builtin_error_class("Error", None));
    for name in ["RangeError", "TypeError"] {
        error_classes.insert(name, Rc::new(builtin_error_class(name, Some(base.clone()))));
    }
    error_classes.insert("Error", base);
    for (name, cls) in &error_classes {
        env_define(&globals, name, Value::Class(cls.clone()));
    }
    Interp { host, globals, callbacks: Vec::new(), error_classes, class_stack: Vec::new() }
}

fn builtin_error_class(name: &'static str, parent: Option<Rc<ClassDef>>) -> ClassDef {
    ClassDef {
        name: name.to_string(),
        parent,
        fields: vec![("message".to_string(), None)],
        methods: HashMap::new(),
        getters: HashMap::new(),
        setters: HashMap::new(),
        ctor: None,
        statics: RefCell::new(HashMap::new()),
        static_methods: HashMap::new(),
        is_builtin_error: true,
        env: None,
    }
}

impl Interp {
    fn throw(&self, class: &'static str, msg: impl Into<String>) -> Thrown {
        let cls = self.error_classes[class].clone();
        let mut fields = HashMap::new();
        fields.insert("message".to_string(), Value::Str(Rc::new(msg.into().chars().collect())));
        Thrown(Value::Instance(Rc::new(RefCell::new(Instance { class: cls, fields }))))
    }

    fn type_error<T>(&self, msg: impl Into<String>) -> Result<T, Thrown> {
        Err(self.throw("TypeError", msg))
    }

    /// Render a thrown value for host error reporting.
    pub fn describe_thrown(&self, t: &Thrown) -> String {
        match &t.0 {
            Value::Instance(i) => {
                let i = i.borrow();
                let msg = i
                    .fields
                    .get("message")
                    .map(|m| to_display(m))
                    .unwrap_or_default();
                format!("{}: {}", i.class.name, msg)
            }
            other => format!("uncaught: {}", to_display(other)),
        }
    }

    // ---- module execution ------------------------------------------------------

    pub fn run_module(&mut self, module: &'static Module) -> Result<(), Thrown> {
        let mut decls: Vec<&'static Decl> = Vec::new();
        for item in &module.items {
            match item {
                Item::Import(im) => self.bind_import(im)?,
                Item::Decl(d) => decls.push(d),
                Item::Export(ex) => match &ex.kind {
                    ExportKind::Decl(d) => decls.push(d),
                    // Exported variables execute with the other statements
                    // (second walk below); named re-exports are inert here.
                    ExportKind::Var(_) | ExportKind::Named { .. } => {}
                },
                Item::Stmt(_) => {}
            }
        }

        // Hoist declarations (order-independent, §4.5). Classes may extend
        // classes declared later, so define in dependency order.
        for d in &decls {
            if let Decl::Function(f) = d {
                let data = Rc::new(FnData {
                    name: f.name.text.clone(),
                    is_async: f.is_async,
                    params: &f.params,
                    body: FnBody::Block(&f.body),
                });
                let c = Closure { data, env: self.globals.clone(), this: None, cls: None };
                env_define(&self.globals, &f.name.text, Value::Closure(Rc::new(c)));
            }
        }
        let mut pending: Vec<&'static ClassDecl> = decls
            .iter()
            .filter_map(|d| match d {
                Decl::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        while !pending.is_empty() {
            let mut still = Vec::new();
            let mut progressed = false;
            for c in pending {
                if self.try_define_class(c)? {
                    progressed = true;
                } else {
                    still.push(c);
                }
            }
            pending = still;
            if !pending.is_empty() && !progressed {
                let name = &pending[0].name.text;
                return Err(
                    self.throw("TypeError", format!("cannot resolve base class of `{name}`"))
                );
            }
        }
        for d in &decls {
            if let Decl::Enum(e) = d {
                self.define_enum(e)?;
            }
        }

        // Execute remaining top-level statements in order (including
        // exported variable statements).
        for item in &module.items {
            match item {
                Item::Stmt(s) => {
                    self.exec_stmt(s, &self.globals.clone())?;
                }
                Item::Export(ExportDecl { kind: ExportKind::Var(v), .. }) => {
                    self.exec_var(v, &self.globals.clone())?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn bind_import(&mut self, im: &'static ImportDecl) -> Result<(), Thrown> {
        let names: Vec<&Name> = match &im.clause {
            None => return Ok(()),
            Some(ImportClause::Namespace(n)) => {
                return self
                    .type_error(format!("namespace imports are not in the MVP (`{}`)", n.text));
            }
            Some(ImportClause::Named(specs)) => {
                specs.iter().map(|s| s.alias.as_ref().unwrap_or(&s.name)).collect()
            }
        };
        match im.from.as_str() {
            "std:console" => {
                let mut entries = HashMap::new();
                entries.insert("log".to_string(), Value::Native("console.log"));
                let console = Value::Namespace(Rc::new(Namespace {
                    name: "console".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, console.clone());
                }
                Ok(())
            }
            "browser:dom" => {
                let mut entries = HashMap::new();
                entries.insert("getElementById".to_string(), Value::Native("dom.getElementById"));
                let document = Value::Namespace(Rc::new(Namespace {
                    name: "document".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, document.clone());
                }
                Ok(())
            }
            other => self.type_error(format!(
                "module `{other}` is not available in the MVP (only `std:console` and \
                 `browser:dom`)"
            )),
        }
    }

    fn try_define_class(&mut self, c: &'static ClassDecl) -> Result<bool, Thrown> {
        let parent = match &c.extends {
            None => None,
            Some(Type::Named { name, .. }) => match env_get(&self.globals, name) {
                Some(Value::Class(p)) => Some(p),
                _ => return Ok(false), // base not defined yet
            },
            Some(_) => return self.type_error("invalid extends clause"),
        };

        let mut fields: Vec<(String, Option<&'static Expr>)> = Vec::new();
        if let Some(p) = &parent {
            fields.extend(p.fields.iter().map(|(n, e)| (n.clone(), *e)));
        }
        let mut methods = HashMap::new();
        let mut getters = HashMap::new();
        let mut setters = HashMap::new();
        let mut static_methods = HashMap::new();
        let mut ctor = None;
        let statics: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());

        for m in &c.members {
            match m {
                ClassMember::Field { mods, name, init, .. } => {
                    if mods.is_static {
                        let v = match init {
                            Some(e) => self.eval(e, &self.globals.clone())?,
                            None => Value::Null,
                        };
                        statics.borrow_mut().insert(name.clone(), v);
                    } else {
                        fields.push((name.clone(), init.as_ref()));
                    }
                }
                ClassMember::Method { mods, is_async, name, params, body, .. } => {
                    if let Some(body) = body {
                        let data = Rc::new(FnData {
                            name: name.clone(),
                            is_async: *is_async,
                            params,
                            body: FnBody::Block(body),
                        });
                        if mods.is_static {
                            static_methods.insert(name.clone(), data);
                        } else {
                            methods.insert(name.clone(), data);
                        }
                    }
                }
                ClassMember::Getter { name, body, .. } => {
                    getters.insert(
                        name.clone(),
                        Rc::new(FnData {
                            name: name.clone(),
                            is_async: false,
                            params: &[],
                            body: FnBody::Block(body),
                        }),
                    );
                }
                ClassMember::Setter { name, param, body, .. } => {
                    setters.insert(
                        name.clone(),
                        Rc::new(FnData {
                            name: name.clone(),
                            is_async: false,
                            params: std::slice::from_ref(param),
                            body: FnBody::Block(body),
                        }),
                    );
                }
                ClassMember::Ctor { params, body, .. } => {
                    ctor = Some(Rc::new(FnData {
                        name: format!("{}.constructor", c.name.text),
                        is_async: false,
                        params,
                        body: FnBody::Block(body),
                    }));
                }
            }
        }

        let def = Rc::new(ClassDef {
            name: c.name.text.clone(),
            parent,
            fields,
            methods,
            getters,
            setters,
            ctor,
            statics,
            static_methods,
            is_builtin_error: false,
            env: Some(self.globals.clone()),
        });
        env_define(&self.globals, &c.name.text, Value::Class(def));
        Ok(true)
    }

    fn define_enum(&mut self, e: &'static EnumDecl) -> Result<(), Thrown> {
        let mut entries = HashMap::new();
        let mut next: i64 = 0;
        for (name, init) in &e.members {
            let v = match init {
                Some(expr) => {
                    let v = self.eval(expr, &self.globals.clone())?;
                    as_i64(&v).ok_or_else(|| {
                        self.throw("TypeError", "enum member value must be an integer")
                    })?
                }
                None => next,
            };
            next = v + 1;
            entries.insert(name.text.clone(), Value::I64(v));
        }
        let ns = Value::Namespace(Rc::new(Namespace { name: e.name.text.clone(), entries }));
        env_define(&self.globals, &e.name.text, ns);
        Ok(())
    }

    /// Driver entry point for host event callbacks (Stage A DOM events).
    pub fn invoke_callback(&mut self, id: u32) -> Result<(), Thrown> {
        let cb = match self.callbacks.get(id as usize) {
            Some(v) => v.clone(),
            None => return self.type_error(format!("unknown callback #{id}")),
        };
        self.call_value(&cb, Vec::new()).map(|_| ())
    }

    // ---- statements -----------------------------------------------------------

    fn exec_block(&mut self, stmts: &'static [Stmt], env: &Env) -> SResult {
        let scope = child_env(env);
        for s in stmts {
            match self.exec_stmt(s, &scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    fn exec_stmt(&mut self, s: &'static Stmt, env: &Env) -> SResult {
        self.exec_stmt_l(s, env, None)
    }

    /// `label` is the label attached to this statement, if it is a loop —
    /// `break label`/`continue label` signals matching it are consumed here.
    fn exec_stmt_l(&mut self, s: &'static Stmt, env: &Env, label: Option<&str>) -> SResult {
        match s {
            Stmt::Block(b) => self.exec_block(b, env),
            Stmt::Var(v) => {
                self.exec_var(v, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Empty => Ok(Sig::Normal),
            Stmt::If { cond, then, els } => {
                if self.truthy(cond, env)? {
                    self.exec_stmt(then, env)
                } else if let Some(e) = els {
                    self.exec_stmt(e, env)
                } else {
                    Ok(Sig::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.truthy(cond, env)? {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    if !self.truthy(cond, env)? {
                        break;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::For { init, cond, step, body } => {
                let scope = child_env(env);
                match init {
                    Some(ForInit::Var(v)) => {
                        self.exec_var(v, &scope)?;
                    }
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.eval(e, &scope)?;
                        }
                    }
                    None => {}
                }
                loop {
                    if let Some(c) = cond {
                        if !self.truthy(c, &scope)? {
                            break;
                        }
                    }
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    for e in step {
                        self.eval(e, &scope)?;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::ForOf { target, iter, body, .. } => {
                let iterable = self.eval(iter, env)?;
                let items: Vec<Value> = match &iterable {
                    Value::Array(a) => a.borrow().clone(),
                    Value::Str(s) => s.iter().map(|c| Value::Char(*c)).collect(),
                    _ => return self.type_error("`for of` needs an array or string"),
                };
                for item in items {
                    let scope = child_env(env);
                    self.bind_pattern(target, item, &scope)?;
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Switch { scrutinee, clauses } => {
                let v = self.eval(scrutinee, env)?;
                let scope = child_env(env);
                let mut matched = clauses.len();
                for (i, c) in clauses.iter().enumerate() {
                    if let Some(t) = &c.test {
                        let tv = self.eval(t, &scope)?;
                        if self.values_equal(&v, &tv)? {
                            matched = i;
                            break;
                        }
                    }
                }
                if matched == clauses.len() {
                    if let Some(i) = clauses.iter().position(|c| c.test.is_none()) {
                        matched = i;
                    }
                }
                'clauses: for c in clauses.iter().skip(matched) {
                    for s in &c.body {
                        match self.exec_stmt(s, &scope)? {
                            Sig::Normal => {}
                            Sig::Break(None) => break 'clauses,
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Break { label, .. } => Ok(Sig::Break(label.as_ref().map(|l| l.text.clone()))),
            Stmt::Continue { label, .. } => {
                Ok(Sig::Continue(label.as_ref().map(|l| l.text.clone())))
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                Ok(Sig::Return(v))
            }
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                Err(Thrown(v))
            }
            Stmt::Try { block, catches, finally } => {
                let result = self.exec_block(block, env);
                let result = match result {
                    Err(thrown) => {
                        let mut handled = None;
                        for c in catches {
                            if self.catch_matches(&c.ty, &thrown.0) {
                                let scope = child_env(env);
                                env_define(&scope, &c.name.text, thrown.0.clone());
                                handled = Some(self.exec_block_in(&c.block, &scope));
                                break;
                            }
                        }
                        handled.unwrap_or(Err(thrown))
                    }
                    ok => ok,
                };
                if let Some(f) = finally {
                    match self.exec_block(f, env)? {
                        Sig::Normal => {}
                        other => return Ok(other), // finally overrides
                    }
                }
                result
            }
            Stmt::Labeled { label: l, body } => {
                // The loop consumes matching break/continue signals itself.
                self.exec_stmt_l(body, env, Some(&l.text))
            }
        }
    }

    fn exec_block_in(&mut self, stmts: &'static [Stmt], scope: &Env) -> SResult {
        for s in stmts {
            match self.exec_stmt(s, scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    fn catch_matches(&self, ty: &Type, thrown: &Value) -> bool {
        let want = match ty {
            Type::Named { name, .. } => name.as_str(),
            _ => return false,
        };
        if want == "Error" {
            return true;
        }
        if let Value::Instance(i) = thrown {
            let mut cls = Some(i.borrow().class.clone());
            while let Some(c) = cls {
                if c.name == want {
                    return true;
                }
                cls = c.parent.clone();
            }
        }
        false
    }

    fn exec_var(&mut self, v: &'static VarStmt, env: &Env) -> Result<(), Thrown> {
        for b in &v.bindings {
            let value = match &b.init {
                Some(e) => self.eval(e, env)?,
                None => Value::Null,
            };
            self.bind_pattern(&b.target, value, env)?;
        }
        Ok(())
    }

    fn bind_pattern(&mut self, p: &'static Pattern, value: Value, env: &Env) -> Result<(), Thrown> {
        match p {
            Pattern::Name(n) => {
                env_define(env, &n.text, value);
                Ok(())
            }
            Pattern::Array { elems, rest } => {
                let items: Vec<Value> = match &value {
                    Value::Array(a) => a.borrow().clone(),
                    Value::Str(s) => s.iter().map(|c| Value::Char(*c)).collect(),
                    _ => return self.type_error("cannot destructure a non-array"),
                };
                for (i, e) in elems.iter().enumerate() {
                    let mut v = items.get(i).cloned().unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &e.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    self.bind_pattern(&e.target, v, env)?;
                }
                if let Some(r) = rest {
                    let tail: Vec<Value> = items.iter().skip(elems.len()).cloned().collect();
                    self.bind_pattern(r, Value::Array(Rc::new(RefCell::new(tail))), env)?;
                }
                Ok(())
            }
            Pattern::Record(fields) => {
                for f in fields {
                    let mut v = self.get_member(&value, &f.name.text)?.unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &f.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    match &f.target {
                        Some(t) => self.bind_pattern(t, v, env)?,
                        None => env_define(env, &f.name.text, v),
                    }
                }
                Ok(())
            }
        }
    }

    // ---- calls --------------------------------------------------------------------

    fn call_closure(&mut self, c: &Closure, args: Vec<Value>) -> VResult {
        if c.data.is_async {
            return self.type_error("async functions are not in the MVP");
        }
        let scope = child_env(&c.env);
        self.bind_params(c.data.params, args, &scope)?;
        if let Some(this) = &c.this {
            env_define(&scope, "this", this.clone());
        }
        match &c.data.body {
            FnBody::Expr(e) => {
                let frame = Frame::enter(self, c);
                frame.i.eval(e, &scope)
            }
            FnBody::Block(stmts) => {
                let frame = Frame::enter(self, c);
                match frame.i.exec_block_in(stmts, &scope)? {
                    Sig::Return(v) => Ok(v),
                    _ => Ok(Value::Null),
                }
            }
        }
    }

    fn bind_params(
        &mut self,
        params: &'static [Param],
        mut args: Vec<Value>,
        scope: &Env,
    ) -> Result<(), Thrown> {
        let mut rest_param: Option<&'static Param> = None;
        let positional: Vec<&'static Param> = params
            .iter()
            .filter(|p| {
                if p.rest {
                    rest_param = Some(p);
                    false
                } else {
                    true
                }
            })
            .collect();
        let n = positional.len().min(args.len());
        let rest_args: Vec<Value> = args.split_off(n);
        for (i, p) in positional.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or(Value::Null);
            if matches!(v, Value::Null) {
                if let Some(d) = &p.default {
                    v = self.eval(d, scope)?;
                }
            }
            self.bind_pattern(&p.target, v, scope)?;
        }
        if let Some(r) = rest_param {
            self.bind_pattern(
                &r.target,
                Value::Array(Rc::new(RefCell::new(rest_args))),
                scope,
            )?;
        }
        Ok(())
    }

    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> VResult {
        match callee {
            Value::Closure(c) => self.call_closure(c, args),
            Value::Native(name) => self.call_native(name, None, args),
            _ => self.type_error("value is not callable"),
        }
    }

    fn call_native(&mut self, name: &str, recv: Option<&Value>, args: Vec<Value>) -> VResult {
        match name {
            "console.log" => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.host.print(&line);
                Ok(Value::Null)
            }
            "dom.getElementById" => {
                let id = self.want_string(args.first())?;
                Ok(Value::Dom(Rc::new(id)))
            }
            "dom.addEventListener" => {
                let Some(Value::Dom(id)) = recv else {
                    return self.type_error("addEventListener needs an element");
                };
                let event = self.want_string(args.first())?;
                if event != "click" {
                    return self.type_error("MVP supports `click` events only");
                }
                let cb = args.get(1).cloned().unwrap_or(Value::Null);
                let cb_id = self.callbacks.len() as u32;
                self.callbacks.push(cb);
                self.host.dom_on_click(id, cb_id);
                Ok(Value::Null)
            }
            _ => self.type_error(format!("unknown native `{name}`")),
        }
    }

    fn want_string(&self, v: Option<&Value>) -> Result<String, Thrown> {
        match v {
            Some(Value::Str(s)) => Ok(s.iter().collect()),
            _ => Err(self.throw("TypeError", "expected a string argument")),
        }
    }

    fn instantiate(&mut self, cls: &Rc<ClassDef>, args: Vec<Value>) -> VResult {
        if cls.is_builtin_error {
            let mut fields = HashMap::new();
            fields.insert(
                "message".to_string(),
                args.into_iter().next().unwrap_or(Value::Null),
            );
            return Ok(Value::Instance(Rc::new(RefCell::new(Instance {
                class: cls.clone(),
                fields,
            }))));
        }
        let inst = Rc::new(RefCell::new(Instance {
            class: cls.clone(),
            fields: HashMap::new(),
        }));
        let this = Value::Instance(inst.clone());
        let env = cls.env.clone().unwrap_or_else(|| self.globals.clone());

        // Field initializers, base-first, with `this` in scope.
        for (name, init) in &cls.fields {
            let v = match init {
                Some(e) => {
                    let scope = child_env(&env);
                    env_define(&scope, "this", this.clone());
                    self.eval(e, &scope)?
                }
                None => Value::Null,
            };
            inst.borrow_mut().fields.insert(name.clone(), v);
        }

        // Nearest constructor up the chain; implicit pass-through otherwise.
        let mut search = Some(cls.clone());
        while let Some(c) = search {
            if let Some(ctor) = &c.ctor {
                let closure = Closure {
                    data: ctor.clone(),
                    env,
                    this: Some(this.clone()),
                    cls: Some(c.clone()),
                };
                self.call_closure(&closure, args)?;
                break;
            }
            search = c.parent.clone();
        }
        Ok(this)
    }

    // ---- member access -----------------------------------------------------------

    fn get_member(&mut self, obj: &Value, name: &str) -> Result<Option<Value>, Thrown> {
        match obj {
            Value::Str(s) => Ok(match name {
                "length" => Some(Value::I32(s.len() as i32)),
                _ => None,
            }),
            Value::Array(a) => Ok(match name {
                "length" => Some(Value::I32(a.borrow().len() as i32)),
                _ => None,
            }),
            Value::Record(r) => Ok(r.borrow().get(name).cloned()),
            Value::Namespace(ns) => Ok(ns.entries.get(name).cloned()),
            Value::Dom(id) => match name {
                "textContent" => Ok(Some(Value::Str(Rc::new(
                    self.host.dom_get_text(id).unwrap_or_default().chars().collect(),
                )))),
                _ => Ok(None),
            },
            Value::Class(c) => {
                if let Some(v) = c.statics.borrow().get(name) {
                    return Ok(Some(v.clone()));
                }
                if let Some(m) = c.static_methods.get(name) {
                    let env = c.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m.clone(),
                        env,
                        this: None,
                        cls: Some(c.clone()),
                    }))));
                }
                Ok(None)
            }
            Value::Instance(inst) => {
                {
                    let i = inst.borrow();
                    if let Some(v) = i.fields.get(name) {
                        return Ok(Some(v.clone()));
                    }
                }
                let class = inst.borrow().class.clone();
                if let Some((getter, defining)) = find_in_chain(&class, |c| {
                    c.getters.get(name).map(|g| (g.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: getter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    return self.call_closure(&closure, Vec::new()).map(Some);
                }
                if let Some((m, defining)) = find_in_chain(&class, |c| {
                    c.methods.get(name).map(|m| (m.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    }))));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn set_member(&mut self, obj: &Value, name: &str, value: Value) -> Result<(), Thrown> {
        match obj {
            Value::Record(r) => {
                r.borrow_mut().insert(name.to_string(), value);
                Ok(())
            }
            Value::Dom(id) => {
                if name == "textContent" {
                    let id = id.to_string();
                    self.host.dom_set_text(&id, &to_display(&value));
                    Ok(())
                } else {
                    self.type_error(format!("MVP DOM elements have no settable `{name}`"))
                }
            }
            Value::Class(c) => {
                if c.statics.borrow().contains_key(name) {
                    c.statics.borrow_mut().insert(name.to_string(), value);
                    Ok(())
                } else {
                    self.type_error(format!("no static field `{name}` on class `{}`", c.name))
                }
            }
            Value::Instance(inst) => {
                let class = inst.borrow().class.clone();
                if let Some((setter, defining)) = find_in_chain(&class, |c| {
                    c.setters.get(name).map(|s| (s.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: setter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    self.call_closure(&closure, vec![value])?;
                    return Ok(());
                }
                // Sealed shapes (§4.1): the field must be declared.
                if class_has_field(&class, name) || inst.borrow().fields.contains_key(name) {
                    inst.borrow_mut().fields.insert(name.to_string(), value);
                    Ok(())
                } else {
                    self.type_error(format!(
                        "class `{}` has no field `{name}` (shapes are sealed)",
                        class.name
                    ))
                }
            }
            _ => self.type_error("cannot assign to a member of this value"),
        }
    }

    // ---- expressions -----------------------------------------------------------------

    fn truthy(&mut self, e: &'static Expr, env: &Env) -> Result<bool, Thrown> {
        let v = self.eval(e, env)?;
        self.value_truthy(&v)
    }

    /// Conditions accept `bool` or numeric (`!= 0`), per §3.3 — nothing else.
    fn value_truthy(&self, v: &Value) -> Result<bool, Thrown> {
        Ok(match v {
            Value::Bool(b) => *b,
            Value::I32(n) => *n != 0,
            Value::I64(n) => *n != 0,
            Value::U32(n) => *n != 0,
            Value::U64(n) => *n != 0,
            Value::F32(f) => *f != 0.0,
            Value::F64(f) => *f != 0.0,
            _ => {
                return Err(self.throw(
                    "TypeError",
                    "condition must be bool or numeric (§3.3); write the comparison",
                ))
            }
        })
    }

    fn eval(&mut self, e: &'static Expr, env: &Env) -> VResult {
        match e {
            Expr::Ident(n) => env_get(env, &n.text)
                .ok_or_else(|| self.throw("TypeError", format!("`{}` is not defined", n.text))),
            Expr::This(_) => env_get(env, "this")
                .ok_or_else(|| self.throw("TypeError", "`this` is not available here")),
            Expr::Lit { kind, text } => self.eval_literal(*kind, text),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TplPart::Text(t) => out.push_str(&unescape(t)),
                        TplPart::Expr(e) => {
                            let v = self.eval(e, env)?;
                            out.push_str(&to_display(&v));
                        }
                    }
                }
                Ok(Value::Str(Rc::new(out.chars().collect())))
            }
            Expr::Array(elems) => {
                let mut items = Vec::new();
                for el in elems {
                    let v = self.eval(&el.expr, env)?;
                    if el.spread {
                        match v {
                            Value::Array(a) => items.extend(a.borrow().iter().cloned()),
                            _ => return self.type_error("can only spread arrays"),
                        }
                    } else {
                        items.push(v);
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(items))))
            }
            Expr::Record(fields) => {
                let mut map = HashMap::new();
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            let v = match value {
                                Some(e) => self.eval(e, env)?,
                                None => env_get(env, &name.text).ok_or_else(|| {
                                    self.throw("TypeError", format!("`{}` is not defined", name.text))
                                })?,
                            };
                            map.insert(name.text.clone(), v);
                        }
                        RecordField::Spread(e) => {
                            let v = self.eval(e, env)?;
                            match v {
                                Value::Record(r) => {
                                    for (k, val) in r.borrow().iter() {
                                        map.insert(k.clone(), val.clone());
                                    }
                                }
                                _ => return self.type_error("can only spread records"),
                            }
                        }
                    }
                }
                Ok(Value::Record(Rc::new(RefCell::new(map))))
            }
            Expr::Paren(e) => self.eval(e, env),
            Expr::Arrow { is_async, params, body, .. } => {
                let data = Rc::new(FnData {
                    name: "<arrow>".to_string(),
                    is_async: *is_async,
                    params,
                    body: match body {
                        ArrowBody::Expr(e) => FnBody::Expr(e),
                        ArrowBody::Block(b) => FnBody::Block(b),
                    },
                });
                // Arrows capture `this` lexically.
                let this = env_get(env, "this");
                Ok(Value::Closure(Rc::new(Closure {
                    data,
                    env: env.clone(),
                    this,
                    cls: None,
                })))
            }
            Expr::Unary { op, expr, .. } => {
                if *op == UnaryOp::Await {
                    return self.type_error("`await` is not in the MVP");
                }
                let v = self.eval(expr, env)?;
                self.eval_unary(*op, v)
            }
            Expr::Update { prefix, inc, expr } => {
                let old = self.eval(expr, env)?;
                let one = Value::I32(1);
                let new = self.numeric_binop(
                    if *inc { BinOp::Add } else { BinOp::Sub },
                    old.clone(),
                    one,
                )?;
                self.assign_to(expr, new.clone(), env)?;
                Ok(if *prefix { new } else { old })
            }
            Expr::Binary { op, l, r } => match op {
                BinOp::And => {
                    let lv = self.eval(l, env)?;
                    if !self.value_truthy(&lv)? {
                        return Ok(Value::Bool(false));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Or => {
                    let lv = self.eval(l, env)?;
                    if self.value_truthy(&lv)? {
                        return Ok(Value::Bool(true));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Coalesce => {
                    let lv = self.eval(l, env)?;
                    if matches!(lv, Value::Null) {
                        self.eval(r, env)
                    } else {
                        Ok(lv)
                    }
                }
                BinOp::Instanceof => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    let Value::Class(want) = rv else {
                        return self.type_error("right side of instanceof must be a class");
                    };
                    let mut ok = false;
                    if let Value::Instance(i) = &lv {
                        let mut cls = Some(i.borrow().class.clone());
                        while let Some(c) = cls {
                            if Rc::ptr_eq(&c, &want) {
                                ok = true;
                                break;
                            }
                            cls = c.parent.clone();
                        }
                    }
                    Ok(Value::Bool(ok))
                }
                BinOp::Eq | BinOp::Ne => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    let eq = self.values_equal(&lv, &rv)?;
                    Ok(Value::Bool(if *op == BinOp::Eq { eq } else { !eq }))
                }
                _ => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    self.numeric_binop(*op, lv, rv)
                }
            },
            Expr::Assign { op, target, value } => {
                let rhs = self.eval(value, env)?;
                let new = if *op == "=" {
                    rhs
                } else {
                    let old = self.eval(target, env)?;
                    match *op {
                        "&&=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep { rhs } else { old }
                        }
                        "||=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep { old } else { rhs }
                        }
                        "??=" => {
                            if matches!(old, Value::Null) { rhs } else { old }
                        }
                        _ => {
                            let bin = match *op {
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
                                _ => return self.type_error("unknown assignment operator"),
                            };
                            self.numeric_binop(bin, old, rhs)?
                        }
                    }
                };
                self.assign_to(target, new.clone(), env)?;
                Ok(new)
            }
            Expr::Cond { cond, then, els } => {
                let c = self.eval(cond, env)?;
                if self.value_truthy(&c)? {
                    self.eval(then, env)
                } else {
                    self.eval(els, env)
                }
            }
            Expr::Cast { expr, wrapping, ty } => {
                let v = self.eval(expr, env)?;
                self.eval_cast(v, *wrapping, ty)
            }
            Expr::Call { callee, args, optional, .. } => {
                let argv = self.eval_args(args, env)?;
                // Method-call fast path: dispatch on the receiver.
                if let Expr::Member { obj, name, optional: mopt } = callee.as_ref() {
                    let recv = self.eval(obj, env)?;
                    if (*mopt || *optional) && matches!(recv, Value::Null) {
                        return Ok(Value::Null);
                    }
                    return self.call_member(&recv, name, argv);
                }
                if let Expr::SuperMember { name, .. } = callee.as_ref() {
                    return self.call_super_method(name, argv, env);
                }
                let f = self.eval(callee, env)?;
                if *optional && matches!(f, Value::Null) {
                    return Ok(Value::Null);
                }
                self.call_value(&f, argv)
            }
            Expr::New { ty, args } => {
                let Type::Named { name, .. } = ty else {
                    return self.type_error("`new` needs a class");
                };
                let head = name.split('.').next().unwrap_or(name);
                let v = env_get(env, head)
                    .ok_or_else(|| self.throw("TypeError", format!("`{head}` is not defined")))?;
                let Value::Class(cls) = v else {
                    return self.type_error(format!("`{name}` is not a class"));
                };
                let argv = self.eval_args(args, env)?;
                self.instantiate(&cls, argv)
            }
            Expr::Member { obj, name, optional } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                match self.get_member(&o, name)? {
                    Some(v) => Ok(v),
                    None => self.type_error(format!("no member `{name}` on {}", kind_of(&o))),
                }
            }
            Expr::Index { obj, index, optional } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                let i = self.eval(index, env)?;
                match (&o, as_i64(&i)) {
                    (Value::Array(a), Some(ix)) => {
                        let a = a.borrow();
                        if ix < 0 || ix as usize >= a.len() {
                            Err(self.throw(
                                "RangeError",
                                format!("index {ix} out of bounds (length {})", a.len()),
                            ))
                        } else {
                            Ok(a[ix as usize].clone())
                        }
                    }
                    (Value::Str(s), Some(ix)) => {
                        if ix < 0 || ix as usize >= s.len() {
                            Err(self.throw(
                                "RangeError",
                                format!("index {ix} out of bounds (length {})", s.len()),
                            ))
                        } else {
                            Ok(Value::Char(s[ix as usize]))
                        }
                    }
                    _ => self.type_error("only arrays and strings are indexable"),
                }
            }
            Expr::SuperMember { name, .. } => {
                // Non-call super member: resolve to a bound closure.
                self.super_lookup(name, env)
            }
            Expr::SuperCall { args, .. } => {
                let argv = self.eval_args(args, env)?;
                let this = env_get(env, "this")
                    .ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
                let cls = self.current_class()?;
                let parent = cls
                    .parent
                    .clone()
                    .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
                let mut search = Some(parent);
                while let Some(c) = search {
                    if let Some(ctor) = &c.ctor {
                        let env2 = c.env.clone().unwrap_or_else(|| self.globals.clone());
                        let closure = Closure {
                            data: ctor.clone(),
                            env: env2,
                            this: Some(this.clone()),
                            cls: Some(c.clone()),
                        };
                        return self.call_closure(&closure, argv);
                    }
                    search = c.parent.clone();
                }
                Ok(Value::Null) // no ctor anywhere up the chain: nothing to do
            }
            Expr::ImportCall(_) => self.type_error("dynamic import() is not in the MVP"),
        }
    }

    fn eval_args(&mut self, args: &'static [ArrayElem], env: &Env) -> Result<Vec<Value>, Thrown> {
        let mut out = Vec::new();
        for a in args {
            let v = self.eval(&a.expr, env)?;
            if a.spread {
                match v {
                    Value::Array(arr) => out.extend(arr.borrow().iter().cloned()),
                    _ => return self.type_error("can only spread arrays"),
                }
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn call_member(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> VResult {
        match recv {
            Value::Array(a) => match name {
                "push" => {
                    for v in args {
                        a.borrow_mut().push(v);
                    }
                    Ok(Value::Null)
                }
                "pop" => Ok(a.borrow_mut().pop().unwrap_or(Value::Null)),
                "keys" => {
                    let n = a.borrow().len();
                    Ok(Value::Array(Rc::new(RefCell::new(
                        (0..n).map(|i| Value::I32(i as i32)).collect(),
                    ))))
                }
                "toString" => Ok(Value::Str(Rc::new(to_display(recv).chars().collect()))),
                _ => self.type_error(format!("arrays have no method `{name}` in the MVP")),
            },
            Value::Str(_) | Value::Char(_) | Value::I32(_) | Value::I64(_) | Value::U32(_)
            | Value::U64(_) | Value::F32(_) | Value::F64(_) | Value::Bool(_)
                if name == "toString" =>
            {
                Ok(Value::Str(Rc::new(to_display(recv).chars().collect())))
            }
            Value::Dom(_) if name == "addEventListener" => {
                self.call_native("dom.addEventListener", Some(recv), args)
            }
            Value::Namespace(ns) => match ns.entries.get(name) {
                Some(Value::Native(n)) => {
                    let n = *n;
                    self.call_native(n, Some(recv), args)
                }
                Some(v @ Value::Closure(_)) => {
                    let v = v.clone();
                    self.call_value(&v, args)
                }
                _ => self.type_error(format!("no member `{name}` on `{}`", ns.name)),
            },
            _ => {
                let member = self.get_member(recv, name)?;
                match member {
                    Some(f) => self.call_value(&f, args),
                    None => self.type_error(format!("no method `{name}` on {}", kind_of(recv))),
                }
            }
        }
    }

    fn current_class(&self) -> Result<Rc<ClassDef>, Thrown> {
        self.class_stack
            .last()
            .cloned()
            .ok_or_else(|| self.throw("TypeError", "`super` outside a class"))
    }

    fn super_lookup(&mut self, name: &str, env: &Env) -> VResult {
        let this = env_get(env, "this")
            .ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
        let cls = self.current_class()?;
        let parent = cls
            .parent
            .clone()
            .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
        if let Some((m, defining)) =
            find_in_chain(&parent, |c| c.methods.get(name).map(|m| (m.clone(), c.clone())))
        {
            let env2 = defining.env.clone().unwrap_or_else(|| self.globals.clone());
            return Ok(Value::Closure(Rc::new(Closure {
                data: m,
                env: env2,
                this: Some(this),
                cls: Some(defining),
            })));
        }
        self.type_error(format!("no method `{name}` on the base class"))
    }

    fn call_super_method(&mut self, name: &str, args: Vec<Value>, env: &Env) -> VResult {
        let f = self.super_lookup(name, env)?;
        self.call_value(&f, args)
    }

    fn assign_to(&mut self, target: &'static Expr, value: Value, env: &Env) -> Result<(), Thrown> {
        match target {
            Expr::Ident(n) => {
                if env_set(env, &n.text, value) {
                    Ok(())
                } else {
                    self.type_error(format!("`{}` is not defined", n.text))
                }
            }
            Expr::Member { obj, name, .. } => {
                let o = self.eval(obj, env)?;
                self.set_member(&o, name, value)
            }
            Expr::Index { obj, index, .. } => {
                let o = self.eval(obj, env)?;
                let i = self.eval(index, env)?;
                match (&o, as_i64(&i)) {
                    (Value::Array(a), Some(ix)) => {
                        let mut a = a.borrow_mut();
                        if ix < 0 || ix as usize >= a.len() {
                            Err(self.throw(
                                "RangeError",
                                format!("index {ix} out of bounds (length {})", a.len()),
                            ))
                        } else {
                            a[ix as usize] = value;
                            Ok(())
                        }
                    }
                    _ => self.type_error("only array elements can be assigned by index"),
                }
            }
            _ => self.type_error("invalid assignment target"),
        }
    }

    // ---- literals, numerics, casts ------------------------------------------------

    fn eval_literal(&self, kind: LitKind, text: &str) -> VResult {
        match kind {
            LitKind::Null => Ok(Value::Null),
            LitKind::Bool => Ok(Value::Bool(text == "true")),
            LitKind::Str => {
                let inner = &text[1..text.len() - 1];
                Ok(Value::Str(Rc::new(unescape(inner).chars().collect())))
            }
            LitKind::Char => {
                let inner = &text[2..text.len() - 1]; // strip c' and '
                let s = unescape(inner);
                s.chars()
                    .next()
                    .map(Value::Char)
                    .ok_or_else(|| self.throw("TypeError", "empty char literal"))
            }
            LitKind::Int => self.parse_int(text),
            LitKind::Float => {
                let is_f32 = text.ends_with('f');
                let core: String = text.trim_end_matches('f').replace('_', "");
                let v: f64 = core
                    .parse()
                    .map_err(|_| self.throw("TypeError", format!("bad float literal `{text}`")))?;
                Ok(if is_f32 { Value::F32(v as f32) } else { Value::F64(v) })
            }
            LitKind::BigInt | LitKind::BigDec => {
                self.type_error("bigint/bigdec literals are not in the MVP")
            }
        }
    }

    fn parse_int(&self, text: &str) -> VResult {
        let t = text.replace('_', "");
        // Longest suffix first; hex digits can't collide with any of these.
        const SUFFIXES: &[&str] =
            &["u64", "u32", "u16", "ul", "u8", "i64", "i32", "i16", "i8", "l", "u"];
        let suffix = SUFFIXES.iter().find(|s| t.ends_with(**s)).copied().unwrap_or("");
        let digits = &t[..t.len() - suffix.len()];
        let (radix, body) = if let Some(b) = digits.strip_prefix("0x") {
            (16, b)
        } else if let Some(b) = digits.strip_prefix("0o") {
            (8, b)
        } else if let Some(b) = digits.strip_prefix("0b") {
            (2, b)
        } else {
            (10, digits)
        };
        let raw = u64::from_str_radix(body, radix)
            .map_err(|_| self.throw("RangeError", format!("integer literal `{text}` overflows")))?;
        let out_of = || self.throw("RangeError", format!("literal `{text}` does not fit its type"));
        Ok(match suffix {
            "" | "i32" => Value::I32(i32::try_from(raw).map_err(|_| out_of())?),
            "u" | "u32" => Value::U32(u32::try_from(raw).map_err(|_| out_of())?),
            "l" | "i64" => Value::I64(i64::try_from(raw).map_err(|_| out_of())?),
            "ul" | "u64" => Value::U64(raw),
            // Small types promote to int32 immediately (§3.3 rule 1).
            "i8" => Value::I32(i8::try_from(raw).map_err(|_| out_of())? as i32),
            "i16" => Value::I32(i16::try_from(raw).map_err(|_| out_of())? as i32),
            "u8" => Value::I32(u8::try_from(raw).map_err(|_| out_of())? as i32),
            "u16" => Value::I32(u16::try_from(raw).map_err(|_| out_of())? as i32),
            _ => return self.type_error(format!("unsupported suffix on `{text}`")),
        })
    }

    fn eval_unary(&mut self, op: UnaryOp, v: Value) -> VResult {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!self.value_truthy(&v)?)),
            UnaryOp::Plus => match v {
                Value::I32(_) | Value::I64(_) | Value::U32(_) | Value::U64(_) | Value::F32(_)
                | Value::F64(_) => Ok(v),
                _ => self.type_error("unary `+` needs a number"),
            },
            UnaryOp::Neg => match v {
                Value::I32(n) => Ok(Value::I32(n.wrapping_neg())),
                Value::I64(n) => Ok(Value::I64(n.wrapping_neg())),
                Value::U32(n) => Ok(Value::U32(n.wrapping_neg())),
                Value::U64(n) => Ok(Value::U64(n.wrapping_neg())),
                Value::F32(f) => Ok(Value::F32(-f)),
                Value::F64(f) => Ok(Value::F64(-f)),
                _ => self.type_error("unary `-` needs a number"),
            },
            UnaryOp::BitNot => match v {
                Value::I32(n) => Ok(Value::I32(!n)),
                Value::I64(n) => Ok(Value::I64(!n)),
                Value::U32(n) => Ok(Value::U32(!n)),
                Value::U64(n) => Ok(Value::U64(!n)),
                _ => self.type_error("`~` needs an integer"),
            },
            UnaryOp::Await => self.type_error("`await` is not in the MVP"),
        }
    }

    fn values_equal(&self, a: &Value, b: &Value) -> Result<bool, Thrown> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Char(x), Value::Char(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Array(x), Value::Array(y)) => Rc::ptr_eq(x, y),
            (Value::Record(x), Value::Record(y)) => Rc::ptr_eq(x, y),
            (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(x, y),
            (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
            _ => {
                if let (Some(_), Some(_)) = (as_num(a), as_num(b)) {
                    let (x, y) = promote_pair(a, b)
                        .ok_or_else(|| self.throw("TypeError", "cannot compare these values"))?;
                    return Ok(num_eq(&x, &y));
                }
                return Err(self.throw(
                    "TypeError",
                    format!("`==` between {} and {} (no coercion, §3.3)", kind_of(a), kind_of(b)),
                ));
            }
        })
    }

    fn numeric_binop(&mut self, op: BinOp, l: Value, r: Value) -> VResult {
        // String / char concatenation and comparisons first.
        match (&l, &r, op) {
            (Value::Str(a), Value::Str(b), BinOp::Add) => {
                let mut s: Vec<char> = a.as_ref().clone();
                s.extend(b.iter());
                return Ok(Value::Str(Rc::new(s)));
            }
            (Value::Str(a), Value::Str(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            (Value::Char(a), Value::Char(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            _ => {}
        }
        let (a, b) = promote_pair(&l, &r).ok_or_else(|| {
            self.throw(
                "TypeError",
                format!("`{}` needs numeric operands, got {} and {}", op.as_str(), kind_of(&l), kind_of(&r)),
            )
        })?;
        self.promoted_binop(op, a, b)
    }

    fn promoted_binop(&mut self, op: BinOp, a: Value, b: Value) -> VResult {
        use BinOp::*;
        macro_rules! int_ops {
            ($x:expr, $y:expr, $wrap:ident, $t:ty, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x.wrapping_add(y)),
                    Sub => $mk(x.wrapping_sub(y)),
                    Mul => $mk(x.wrapping_mul(y)),
                    Div => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_div(y) {
                            Some(q) => $mk(q),
                            None => return Err(self.throw("RangeError", "integer overflow in division")),
                        }
                    }
                    Rem => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_rem(y) {
                            Some(q) => $mk(q),
                            None => $mk(0 as $t),
                        }
                    }
                    Pow => {
                        let mut acc: $t = 1 as $t;
                        let mut i = 0i64;
                        let n = y as i64;
                        if n < 0 {
                            return Err(self.throw("RangeError", "negative integer exponent"));
                        }
                        while i < n {
                            acc = acc.wrapping_mul(x);
                            i += 1;
                        }
                        $mk(acc)
                    }
                    Shl => $mk(x.wrapping_shl(y as u32)),
                    Shr => $mk(x.wrapping_shr(y as u32)),
                    BitAnd => $mk(x & y),
                    BitOr => $mk(x | y),
                    BitXor => $mk(x ^ y),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("bad operator"),
                }
            }};
        }
        macro_rules! float_ops {
            ($x:expr, $y:expr, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x + y),
                    Sub => $mk(x - y),
                    Mul => $mk(x * y),
                    Div => $mk(x / y),
                    Rem => $mk(x % y),
                    Pow => $mk(x.powf(y)),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("floats do not support this operator"),
                }
            }};
        }
        Ok(match (a, b) {
            (Value::I32(x), Value::I32(y)) => int_ops!(x, y, wrapping, i32, Value::I32),
            (Value::U32(x), Value::U32(y)) => int_ops!(x, y, wrapping, u32, Value::U32),
            (Value::I64(x), Value::I64(y)) => int_ops!(x, y, wrapping, i64, Value::I64),
            (Value::U64(x), Value::U64(y)) => int_ops!(x, y, wrapping, u64, Value::U64),
            (Value::F32(x), Value::F32(y)) => float_ops!(x, y, Value::F32),
            (Value::F64(x), Value::F64(y)) => float_ops!(x, y, Value::F64),
            _ => return self.type_error("operands did not promote to a common type"),
        })
    }

    fn eval_cast(&mut self, v: Value, wrapping: bool, ty: &Type) -> VResult {
        let Type::Named { name, .. } = ty else {
            return Ok(v); // casts to complex types: checker's concern
        };
        let out_of_range =
            || self.throw("RangeError", format!("value does not fit `{name}` (use `as wrapping`)"));
        let as_f = match as_num(&v) {
            Some(f) => f,
            None => return Ok(v), // non-numeric cast: reference cast, pass through
        };
        let as_i = as_i64(&v);
        macro_rules! to_int {
            ($t:ty, $mk:expr) => {{
                if wrapping {
                    match as_i {
                        Some(i) => $mk(i as $t),
                        None => $mk(as_f as $t), // saturating from float, defined
                    }
                } else {
                    match as_i {
                        Some(i) => match <$t>::try_from(i) {
                            Ok(x) => $mk(x),
                            Err(_) => return Err(out_of_range()),
                        },
                        None => {
                            let t = as_f.trunc();
                            if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 && t == as_f {
                                $mk(t as $t)
                            } else if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 {
                                $mk(t as $t) // fractional part dropped, in range
                            } else {
                                return Err(out_of_range());
                            }
                        }
                    }
                }
            }};
        }
        Ok(match name.as_str() {
            "int32" | "int" => to_int!(i32, Value::I32),
            "uint32" | "uint" => to_int!(u32, Value::U32),
            "int64" => to_int!(i64, Value::I64),
            "uint64" => {
                if wrapping {
                    match &v {
                        Value::I64(i) => Value::U64(*i as u64),
                        Value::I32(i) => Value::U64(*i as i64 as u64),
                        _ => Value::U64(as_f as u64),
                    }
                } else {
                    match as_i {
                        Some(i) if i >= 0 => Value::U64(i as u64),
                        Some(_) => return Err(out_of_range()),
                        None => match &v {
                            Value::U64(u) => Value::U64(*u),
                            _ => Value::U64(as_f as u64),
                        },
                    }
                }
            }
            "int8" => to_int!(i8, |x: i8| Value::I32(x as i32)),
            "int16" => to_int!(i16, |x: i16| Value::I32(x as i32)),
            "uint8" => to_int!(u8, |x: u8| Value::I32(x as i32)),
            "uint16" => to_int!(u16, |x: u16| Value::I32(x as i32)),
            "float64" | "float" => Value::F64(as_f),
            "float32" => Value::F32(as_f as f32),
            _ => v, // class/interface cast: dynamic checks arrive with the checker
        })
    }
}

/// RAII-ish frame for tracking the class whose method is executing (for
/// `super`). Kept tiny: a manual stack with a guard.
struct Frame<'a> {
    i: &'a mut Interp,
    pushed: bool,
}

impl<'a> Frame<'a> {
    fn enter(i: &'a mut Interp, c: &Closure) -> Frame<'a> {
        let pushed = if let Some(cls) = &c.cls {
            i.class_stack.push(cls.clone());
            true
        } else {
            false
        };
        Frame { i, pushed }
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if self.pushed {
            self.i.class_stack.pop();
        }
    }
}

// ---- helpers ------------------------------------------------------------------------

fn find_in_chain<T>(
    class: &Rc<ClassDef>,
    f: impl Fn(&Rc<ClassDef>) -> Option<T>,
) -> Option<T> {
    let mut cls = Some(class.clone());
    while let Some(c) = cls {
        if let Some(t) = f(&c) {
            return Some(t);
        }
        cls = c.parent.clone();
    }
    None
}

fn class_has_field(class: &Rc<ClassDef>, name: &str) -> bool {
    find_in_chain(class, |c| c.fields.iter().any(|(n, _)| n == name).then_some(())).is_some()
}

fn as_num(v: &Value) -> Option<f64> {
    Some(match v {
        Value::I32(n) => *n as f64,
        Value::I64(n) => *n as f64,
        Value::U32(n) => *n as f64,
        Value::U64(n) => *n as f64,
        Value::F32(f) => *f as f64,
        Value::F64(f) => *f,
        _ => return None,
    })
}

fn as_i64(v: &Value) -> Option<i64> {
    Some(match v {
        Value::I32(n) => *n as i64,
        Value::I64(n) => *n,
        Value::U32(n) => *n as i64,
        Value::U64(n) => *n as i64,
        _ => return None,
    })
}

/// Usual arithmetic conversions (§3.3): float wins; wider rank wins;
/// unsigned wins at equal rank.
fn promote_pair(a: &Value, b: &Value) -> Option<(Value, Value)> {
    use Value::*;
    let rank = |v: &Value| match v {
        I32(_) => Some(0),
        U32(_) => Some(1),
        I64(_) => Some(2),
        U64(_) => Some(3),
        F32(_) => Some(4),
        F64(_) => Some(5),
        _ => None,
    };
    let (ra, rb) = (rank(a)?, rank(b)?);
    let target = ra.max(rb);
    let conv = |v: &Value| -> Value {
        match (v, target) {
            (I32(x), 0) => I32(*x),
            (v, 1) => U32(match v {
                I32(x) => *x as u32,
                U32(x) => *x,
                _ => unreachable!(),
            }),
            (v, 2) => I64(match v {
                I32(x) => *x as i64,
                U32(x) => *x as i64,
                I64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 3) => U64(match v {
                I32(x) => *x as i64 as u64,
                U32(x) => *x as u64,
                I64(x) => *x as u64,
                U64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 4) => F32(as_num(v).unwrap() as f32),
            (v, _) => F64(as_num(v).unwrap()),
        }
    };
    Some((conv(a), conv(b)))
}

fn num_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (I32(x), I32(y)) => x == y,
        (U32(x), U32(y)) => x == y,
        (I64(x), I64(y)) => x == y,
        (U64(x), U64(y)) => x == y,
        (F32(x), F32(y)) => x == y,
        (F64(x), F64(y)) => x == y,
        _ => false,
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::I32(_) => "int32",
        Value::I64(_) => "int64",
        Value::U32(_) => "uint32",
        Value::U64(_) => "uint64",
        Value::F32(_) => "float32",
        Value::F64(_) => "float64",
        Value::Char(_) => "char",
        Value::Str(_) => "string",
        Value::Array(_) => "array",
        Value::Record(_) => "record",
        Value::Closure(_) => "function",
        Value::Class(_) => "class",
        Value::Instance(_) => "object",
        Value::Namespace(_) => "namespace",
        Value::Dom(_) => "dom element",
        Value::Native(_) => "native function",
    }
}

pub fn to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::I32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F32(f) => f.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Char(c) => c.to_string(),
        Value::Str(s) => s.iter().collect(),
        Value::Array(a) => {
            let items: Vec<String> = a.borrow().iter().map(to_display).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Record(r) => {
            let b = r.borrow();
            let mut ks: Vec<String> = b.keys().cloned().collect();
            ks.sort();
            let fields: Vec<String> =
                ks.iter().map(|k| format!("{k}: {}", to_display(&b[k]))).collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Closure(_) | Value::Native(_) => "<function>".to_string(),
        Value::Class(c) => format!("<class {}>", c.name),
        Value::Instance(i) => format!("<{}>", i.borrow().class.name),
        Value::Namespace(ns) => format!("<{}>", ns.name),
        Value::Dom(id) => format!("<#{id}>"),
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                // \u{XXXX}
                let mut val = 0u32;
                for c in chars.by_ref() {
                    if c == '{' {
                        continue;
                    }
                    if c == '}' {
                        break;
                    }
                    val = val * 16 + c.to_digit(16).unwrap_or(0);
                }
                if let Some(c) = char::from_u32(val) {
                    out.push(c);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}
