//! Abstract syntax tree, grammar §6.3–§6.8. Literals keep their lexeme text;
//! value parsing (and range checking, E0110) happens in the checker, which
//! knows the context (`-2147483648` is a unary minus around a literal).
//!
//! Nodes that declare or reference a binding carry a `Name` (text + source
//! position) so the binder and checker can point diagnostics at them.
//! Member names stay plain `String`s — member resolution is type-directed
//! and reports at the whole-expression level.

use crate::diag::Pos;

#[derive(Clone)]
pub struct Name {
    pub text: String,
    pub pos: Pos,
}

pub struct Module {
    pub items: Vec<Item>,
}

pub enum Item {
    Import(ImportDecl),
    Export(ExportDecl),
    Decl(Decl),
    Stmt(Stmt),
}

pub struct ImportDecl {
    /// `None` for a side-effect-only `import "…";`
    pub clause: Option<ImportClause>,
    pub from: String,
}

pub enum ImportClause {
    Named(Vec<NameAlias>),
    Namespace(Name),
}

pub struct NameAlias {
    pub name: Name,
    pub alias: Option<Name>,
}

pub struct ExportDecl {
    pub is_extern: bool,
    pub kind: ExportKind,
}

pub enum ExportKind {
    Decl(Decl),
    Var(VarStmt),
    Named {
        specs: Vec<NameAlias>,
        from: Option<String>,
    },
}

pub enum Decl {
    Function(FnDecl),
    Class(ClassDecl),
    Interface(InterfaceDecl),
    Enum(EnumDecl),
    TypeAlias(TypeAliasDecl),
}

pub struct FnDecl {
    pub is_async: bool,
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
}

pub struct TypeParam {
    pub name: Name,
    pub constraint: Option<TypeExpr>,
}

pub struct Param {
    pub rest: bool,
    pub target: Pattern,
    pub optional: bool,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
}

pub struct ClassDecl {
    pub is_abstract: bool,
    pub is_final: bool,
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub extends: Option<TypeExpr>,
    pub implements: Vec<TypeExpr>,
    pub members: Vec<ClassMember>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Protected,
    Private,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Protected => "protected",
            Access::Private => "private",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Virt {
    Abstract,
    Final,
    Override,
}

impl Virt {
    pub fn as_str(self) -> &'static str {
        match self {
            Virt::Abstract => "abstract",
            Virt::Final => "final",
            Virt::Override => "override",
        }
    }
}

pub struct MemberMods {
    pub access: Option<Access>,
    pub is_static: bool,
    pub virt: Option<Virt>,
}

pub enum ClassMember {
    Field {
        mods: MemberMods,
        readonly: bool,
        name: String,
        ty: TypeExpr,
        init: Option<Expr>,
    },
    Method {
        mods: MemberMods,
        is_async: bool,
        name: String,
        type_params: Vec<TypeParam>,
        params: Vec<Param>,
        ret: TypeExpr,
        /// `None` = `;` body (abstract)
        body: Option<Vec<Stmt>>,
    },
    Getter {
        mods: MemberMods,
        name: String,
        ret: TypeExpr,
        body: Vec<Stmt>,
    },
    Setter {
        mods: MemberMods,
        name: String,
        param: Param,
        body: Vec<Stmt>,
    },
    Ctor {
        access: Option<Access>,
        params: Vec<Param>,
        body: Vec<Stmt>,
    },
}

pub struct InterfaceDecl {
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub extends: Vec<TypeExpr>,
    pub members: Vec<InterfaceMember>,
}

pub enum InterfaceMember {
    Prop {
        readonly: bool,
        name: String,
        optional: bool,
        ty: TypeExpr,
    },
    Method {
        name: String,
        type_params: Vec<TypeParam>,
        params: Vec<Param>,
        ret: TypeExpr,
    },
}

pub struct EnumDecl {
    pub name: Name,
    pub backing: Option<Name>,
    pub members: Vec<(Name, Option<Expr>)>,
}

pub struct TypeAliasDecl {
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub ty: TypeExpr,
}

// ---- statements -----------------------------------------------------------

pub enum Stmt {
    Block(Vec<Stmt>),
    Var(VarStmt),
    Expr(Expr),
    Empty,
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<ForInit>,
        cond: Option<Expr>,
        step: Vec<Expr>,
        body: Box<Stmt>,
    },
    ForOf {
        is_await: bool,
        kind: VarKind,
        target: Pattern,
        ty: Option<TypeExpr>,
        iter: Expr,
        body: Box<Stmt>,
    },
    Switch {
        scrutinee: Expr,
        clauses: Vec<SwitchClause>,
    },
    Break {
        label: Option<Name>,
        pos: Pos,
    },
    Continue {
        label: Option<Name>,
        pos: Pos,
    },
    Return {
        value: Option<Expr>,
        pos: Pos,
    },
    Throw(Expr),
    Try {
        block: Vec<Stmt>,
        catches: Vec<Catch>,
        finally: Option<Vec<Stmt>>,
    },
    Labeled {
        label: Name,
        body: Box<Stmt>,
    },
}

pub enum ForInit {
    Var(VarStmt),
    Exprs(Vec<Expr>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Let,
    Const,
}

impl VarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VarKind::Let => "let",
            VarKind::Const => "const",
        }
    }
}

pub struct VarStmt {
    pub kind: VarKind,
    pub bindings: Vec<Binding>,
}

pub struct Binding {
    pub target: Pattern,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
}

pub struct SwitchClause {
    /// `None` = `default:`
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

pub struct Catch {
    pub name: Name,
    pub ty: TypeExpr,
    pub block: Vec<Stmt>,
}

pub enum Pattern {
    Name(Name),
    Array {
        elems: Vec<PatternElem>,
        rest: Option<Box<Pattern>>,
    },
    Record(Vec<PatternField>),
}

pub struct PatternElem {
    pub target: Pattern,
    pub default: Option<Expr>,
}

pub struct PatternField {
    pub name: Name,
    pub target: Option<Pattern>,
    pub default: Option<Expr>,
}

// ---- expressions ----------------------------------------------------------

pub enum Expr {
    Ident(Name),
    This(Pos),
    Lit {
        kind: LitKind,
        text: String,
        pos: Pos,
    },
    Template(Vec<TplPart>),
    Array(Vec<ArrayElem>),
    Record(Vec<RecordField>),
    /// Preserved so the `??`-mixing rule can see explicit parentheses.
    Paren(Box<Expr>),
    Arrow {
        is_async: bool,
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        body: ArrowBody,
    },
    Unary {
        op: UnaryOp,
        pos: Pos,
        expr: Box<Expr>,
    },
    Update {
        prefix: bool,
        inc: bool,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
    },
    Assign {
        op: &'static str,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Cast {
        expr: Box<Expr>,
        wrapping: bool,
        ty: TypeExpr,
    },
    /// `x is T` — does this value hold a `T`? A `bool`, and it narrows.
    ///
    /// Not `typeof` (§1.2 has no runtime type reflection: there is nothing here
    /// that hands you a type as a value to compute with). It is a *value* test —
    /// the same question the checked cast `x as T` already asks — except it
    /// answers instead of throwing.
    Is {
        expr: Box<Expr>,
        ty: TypeExpr,
    },
    Call {
        callee: Box<Expr>,
        type_args: Vec<TypeExpr>,
        args: Vec<ArrayElem>,
        optional: bool,
    },
    New {
        ty: TypeExpr,
        args: Vec<ArrayElem>,
    },
    Member {
        obj: Box<Expr>,
        name: String,
        optional: bool,
    },
    Index {
        obj: Box<Expr>,
        index: Box<Expr>,
        optional: bool,
    },
    SuperMember {
        name: String,
        pos: Pos,
    },
    SuperCall {
        args: Vec<ArrayElem>,
        pos: Pos,
    },
    ImportCall(Box<Expr>),
    /// `yield expr` — suspends a generator, handing the value to the caller.
    Yield {
        value: Option<Box<Expr>>,
        pos: Pos,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LitKind {
    Int,
    Float,
    BigInt,
    BigDec,
    Str,
    Char,
    Bool,
    Null,
}

pub enum TplPart {
    /// Raw text chunk with the delimiters stripped.
    Text(String),
    Expr(Expr),
}

pub struct ArrayElem {
    pub spread: bool,
    pub expr: Expr,
}

pub enum RecordField {
    Named { name: Name, value: Option<Expr> },
    Spread(Expr),
}

pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
    Plus,
    Neg,
    BitNot,
    Not,
    Await,
}

impl UnaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Plus => "+",
            UnaryOp::Neg => "-",
            UnaryOp::BitNot => "~",
            UnaryOp::Not => "!",
            UnaryOp::Await => "await",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Coalesce,
    Or,
    And,
    BitOr,
    BitXor,
    BitAnd,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Instanceof,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        use BinOp::*;
        match self {
            Coalesce => "??",
            Or => "||",
            And => "&&",
            BitOr => "|",
            BitXor => "^",
            BitAnd => "&",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Instanceof => "instanceof",
            Shl => "<<",
            Shr => ">>",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Pow => "**",
        }
    }
}

// ---- types ----------------------------------------------------------------

/// A type as *written*: the syntax the programmer typed (`int32[]`, `Foo<T>?`).
///
/// Not to be confused with `check::Type`, which is what a type *means* once it
/// has been resolved. `int32[]` and an alias for it are two different
/// `TypeExpr`s and one `Type`.
pub enum TypeExpr {
    /// Qualified name (`a.B`) with optional type arguments; predefined
    /// type names and `void` land here too.
    Named {
        name: String,
        pos: Pos,
        args: Vec<TypeExpr>,
    },
    Nullable(Box<TypeExpr>),
    ArrayOf(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Record(Vec<RecordTypeMember>),
    Function {
        type_params: Vec<TypeParam>,
        params: Vec<FnTypeParam>,
        ret: Box<TypeExpr>,
    },
}

pub struct RecordTypeMember {
    pub readonly: bool,
    pub name: String,
    pub optional: bool,
    pub ty: TypeExpr,
}

pub struct FnTypeParam {
    pub rest: bool,
    pub name: String,
    pub optional: bool,
    pub ty: TypeExpr,
}

/// The *value* of a string literal, whose `text` is the raw source — quotes and
/// all. Used wherever a literal has to become a real string before the engine
/// runs (an `import(…)` specifier, most importantly).
pub fn string_value(text: &str) -> String {
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
        .unwrap_or(text);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

// ---- walking ------------------------------------------------------------------

/// Visit every expression in a module, including inside function bodies,
/// class members and arrow bodies.
///
/// One walker, so a pass that needs to find something in the tree (a dynamic
/// `import`, a closure, a `yield`) does not each grow its own copy that can
/// fall behind the AST when a node is added.
pub fn for_each_expr(module: &Module, f: &mut impl FnMut(&Expr)) {
    for item in &module.items {
        match item {
            Item::Import(_) => {}
            Item::Stmt(s) => walk_stmt(s, f),
            Item::Decl(d) => walk_decl(d, f),
            Item::Export(e) => match &e.kind {
                ExportKind::Decl(d) => walk_decl(d, f),
                ExportKind::Var(v) => walk_var(v, f),
                ExportKind::Named { .. } => {}
            },
        }
    }
}

pub fn walk_decl(d: &Decl, f: &mut impl FnMut(&Expr)) {
    match d {
        Decl::Function(fd) => {
            for p in &fd.params {
                if let Some(dflt) = &p.default {
                    walk_expr(dflt, f);
                }
            }
            for s in &fd.body {
                walk_stmt(s, f);
            }
        }
        Decl::Class(c) => {
            for m in &c.members {
                match m {
                    ClassMember::Field { init: Some(e), .. } => walk_expr(e, f),
                    ClassMember::Field { .. } => {}
                    ClassMember::Method { params, body, .. } => {
                        for p in params {
                            if let Some(dflt) = &p.default {
                                walk_expr(dflt, f);
                            }
                        }
                        for s in body.iter().flatten() {
                            walk_stmt(s, f);
                        }
                    }
                    ClassMember::Ctor { params, body, .. } => {
                        for p in params {
                            if let Some(dflt) = &p.default {
                                walk_expr(dflt, f);
                            }
                        }
                        for s in body {
                            walk_stmt(s, f);
                        }
                    }
                    ClassMember::Getter { body, .. } | ClassMember::Setter { body, .. } => {
                        for s in body {
                            walk_stmt(s, f);
                        }
                    }
                }
            }
        }
        Decl::Interface(_) | Decl::Enum(_) | Decl::TypeAlias(_) => {}
    }
}

fn walk_var(v: &VarStmt, f: &mut impl FnMut(&Expr)) {
    for b in &v.bindings {
        if let Some(e) = &b.init {
            walk_expr(e, f);
        }
    }
}

pub fn walk_stmt(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match s {
        Stmt::Block(b) => b.iter().for_each(|s| walk_stmt(s, f)),
        Stmt::Var(v) => walk_var(v, f),
        Stmt::Expr(e) | Stmt::Throw(e) => walk_expr(e, f),
        Stmt::Empty | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                walk_expr(e, f);
            }
        }
        Stmt::If { cond, then, els } => {
            walk_expr(cond, f);
            walk_stmt(then, f);
            if let Some(e) = els {
                walk_stmt(e, f);
            }
        }
        Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
            walk_expr(cond, f);
            walk_stmt(body, f);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
        } => {
            match init {
                Some(ForInit::Var(v)) => walk_var(v, f),
                Some(ForInit::Exprs(es)) => es.iter().for_each(|e| walk_expr(e, f)),
                None => {}
            }
            if let Some(c) = cond {
                walk_expr(c, f);
            }
            step.iter().for_each(|e| walk_expr(e, f));
            walk_stmt(body, f);
        }
        Stmt::ForOf { iter, body, .. } => {
            walk_expr(iter, f);
            walk_stmt(body, f);
        }
        Stmt::Switch { scrutinee, clauses } => {
            walk_expr(scrutinee, f);
            for c in clauses {
                if let Some(t) = &c.test {
                    walk_expr(t, f);
                }
                c.body.iter().for_each(|s| walk_stmt(s, f));
            }
        }
        Stmt::Try {
            block,
            catches,
            finally,
        } => {
            block.iter().for_each(|s| walk_stmt(s, f));
            for c in catches {
                c.block.iter().for_each(|s| walk_stmt(s, f));
            }
            if let Some(fin) = finally {
                fin.iter().for_each(|s| walk_stmt(s, f));
            }
        }
        Stmt::Labeled { body, .. } => walk_stmt(body, f),
    }
}

pub fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Ident(_) | Expr::This(_) | Expr::Lit { .. } => {}
        Expr::Template(parts) => {
            for p in parts {
                if let TplPart::Expr(e) = p {
                    walk_expr(e, f);
                }
            }
        }
        Expr::Array(items) => items.iter().for_each(|a| walk_expr(&a.expr, f)),
        Expr::Record(fields) => {
            for field in fields {
                match field {
                    RecordField::Named { value: Some(v), .. } => walk_expr(v, f),
                    RecordField::Spread(v) => walk_expr(v, f),
                    RecordField::Named { .. } => {}
                }
            }
        }
        Expr::Paren(i)
        | Expr::Unary { expr: i, .. }
        | Expr::Update { expr: i, .. }
        | Expr::Cast { expr: i, .. }
        | Expr::Is { expr: i, .. }
        | Expr::ImportCall(i) => walk_expr(i, f),
        Expr::Arrow { params, body, .. } => {
            for p in params {
                if let Some(d) = &p.default {
                    walk_expr(d, f);
                }
            }
            match body {
                ArrowBody::Expr(e) => walk_expr(e, f),
                ArrowBody::Block(stmts) => stmts.iter().for_each(|s| walk_stmt(s, f)),
            }
        }
        Expr::Binary { l, r, .. }
        | Expr::Assign {
            target: l,
            value: r,
            ..
        } => {
            walk_expr(l, f);
            walk_expr(r, f);
        }
        Expr::Cond { cond, then, els } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        Expr::Call { callee, args, .. } => {
            walk_expr(callee, f);
            args.iter().for_each(|a| walk_expr(&a.expr, f));
        }
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            args.iter().for_each(|a| walk_expr(&a.expr, f))
        }
        Expr::Member { obj, .. } => walk_expr(obj, f),
        Expr::Index { obj, index, .. } => {
            walk_expr(obj, f);
            walk_expr(index, f);
        }
        Expr::Yield { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, f);
            }
        }
        _ => {}
    }
}
