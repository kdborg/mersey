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
    Named { specs: Vec<NameAlias>, from: Option<String> },
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
    pub ret: Option<Type>,
    pub body: Vec<Stmt>,
}

pub struct TypeParam {
    pub name: Name,
    pub constraint: Option<Type>,
}

pub struct Param {
    pub rest: bool,
    pub target: Pattern,
    pub optional: bool,
    pub ty: Option<Type>,
    pub default: Option<Expr>,
}

pub struct ClassDecl {
    pub is_abstract: bool,
    pub is_final: bool,
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub extends: Option<Type>,
    pub implements: Vec<Type>,
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
        ty: Type,
        init: Option<Expr>,
    },
    Method {
        mods: MemberMods,
        is_async: bool,
        name: String,
        type_params: Vec<TypeParam>,
        params: Vec<Param>,
        ret: Type,
        /// `None` = `;` body (abstract)
        body: Option<Vec<Stmt>>,
    },
    Getter { mods: MemberMods, name: String, ret: Type, body: Vec<Stmt> },
    Setter { mods: MemberMods, name: String, param: Param, body: Vec<Stmt> },
    Ctor { access: Option<Access>, params: Vec<Param>, body: Vec<Stmt> },
}

pub struct InterfaceDecl {
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub extends: Vec<Type>,
    pub members: Vec<InterfaceMember>,
}

pub enum InterfaceMember {
    Prop { readonly: bool, name: String, optional: bool, ty: Type },
    Method { name: String, type_params: Vec<TypeParam>, params: Vec<Param>, ret: Type },
}

pub struct EnumDecl {
    pub name: Name,
    pub backing: Option<Name>,
    pub members: Vec<(Name, Option<Expr>)>,
}

pub struct TypeAliasDecl {
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub ty: Type,
}

// ---- statements -----------------------------------------------------------

pub enum Stmt {
    Block(Vec<Stmt>),
    Var(VarStmt),
    Expr(Expr),
    Empty,
    If { cond: Expr, then: Box<Stmt>, els: Option<Box<Stmt>> },
    While { cond: Expr, body: Box<Stmt> },
    DoWhile { body: Box<Stmt>, cond: Expr },
    For { init: Option<ForInit>, cond: Option<Expr>, step: Vec<Expr>, body: Box<Stmt> },
    ForOf {
        is_await: bool,
        kind: VarKind,
        target: Pattern,
        ty: Option<Type>,
        iter: Expr,
        body: Box<Stmt>,
    },
    Switch { scrutinee: Expr, clauses: Vec<SwitchClause> },
    Break { label: Option<Name>, pos: Pos },
    Continue { label: Option<Name>, pos: Pos },
    Return { value: Option<Expr>, pos: Pos },
    Throw(Expr),
    Try { block: Vec<Stmt>, catches: Vec<Catch>, finally: Option<Vec<Stmt>> },
    Labeled { label: Name, body: Box<Stmt> },
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
    pub ty: Option<Type>,
    pub init: Option<Expr>,
}

pub struct SwitchClause {
    /// `None` = `default:`
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

pub struct Catch {
    pub name: Name,
    pub ty: Type,
    pub block: Vec<Stmt>,
}

pub enum Pattern {
    Name(Name),
    Array { elems: Vec<PatternElem>, rest: Option<Box<Pattern>> },
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
    Lit { kind: LitKind, text: String },
    Template(Vec<TplPart>),
    Array(Vec<ArrayElem>),
    Record(Vec<RecordField>),
    /// Preserved so the `??`-mixing rule can see explicit parentheses.
    Paren(Box<Expr>),
    Arrow { is_async: bool, params: Vec<Param>, ret: Option<Type>, body: ArrowBody },
    Unary { op: UnaryOp, pos: Pos, expr: Box<Expr> },
    Update { prefix: bool, inc: bool, expr: Box<Expr> },
    Binary { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    Assign { op: &'static str, target: Box<Expr>, value: Box<Expr> },
    Cond { cond: Box<Expr>, then: Box<Expr>, els: Box<Expr> },
    Cast { expr: Box<Expr>, wrapping: bool, ty: Type },
    Call { callee: Box<Expr>, type_args: Vec<Type>, args: Vec<ArrayElem>, optional: bool },
    New { ty: Type, args: Vec<ArrayElem> },
    Member { obj: Box<Expr>, name: String, optional: bool },
    Index { obj: Box<Expr>, index: Box<Expr>, optional: bool },
    SuperMember { name: String, pos: Pos },
    SuperCall { args: Vec<ArrayElem>, pos: Pos },
    ImportCall(Box<Expr>),
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

#[derive(Clone, Copy, PartialEq, Eq)]
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Coalesce, Or, And,
    BitOr, BitXor, BitAnd,
    Eq, Ne, Lt, Gt, Le, Ge, Instanceof,
    Shl, Shr,
    Add, Sub, Mul, Div, Rem, Pow,
}

impl BinOp {
    pub fn as_str(self) -> &'static str {
        use BinOp::*;
        match self {
            Coalesce => "??", Or => "||", And => "&&",
            BitOr => "|", BitXor => "^", BitAnd => "&",
            Eq => "==", Ne => "!=", Lt => "<", Gt => ">", Le => "<=", Ge => ">=",
            Instanceof => "instanceof",
            Shl => "<<", Shr => ">>",
            Add => "+", Sub => "-", Mul => "*", Div => "/", Rem => "%", Pow => "**",
        }
    }
}

// ---- types ----------------------------------------------------------------

pub enum Type {
    /// Qualified name (`a.B`) with optional type arguments; predefined
    /// type names and `void` land here too.
    Named { name: String, pos: Pos, args: Vec<Type> },
    Nullable(Box<Type>),
    ArrayOf(Box<Type>),
    Union(Vec<Type>),
    Tuple(Vec<Type>),
    Record(Vec<RecordTypeMember>),
    Function { type_params: Vec<TypeParam>, params: Vec<FnTypeParam>, ret: Box<Type> },
}

pub struct RecordTypeMember {
    pub readonly: bool,
    pub name: String,
    pub optional: bool,
    pub ty: Type,
}

pub struct FnTypeParam {
    pub rest: bool,
    pub name: String,
    pub optional: bool,
    pub ty: Type,
}
