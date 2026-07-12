//! AST dump for `mersey parse` and the parser conformance goldens.
//! Tree nodes print lisp-style, one child per line, closing parens on the
//! last child; expressions and types render on a single line.

use crate::ast::*;

pub fn dump(m: &Module) -> String {
    let doc = Doc::Node(
        "module".to_string(),
        m.items.iter().map(item_doc).collect(),
    );
    let mut out = String::new();
    render(&doc, 0, &mut out);
    out.push('\n');
    out
}

enum Doc {
    Line(String),
    Node(String, Vec<Doc>),
}

fn render(d: &Doc, ind: usize, out: &mut String) {
    let pad = "  ".repeat(ind);
    match d {
        Doc::Line(s) => {
            out.push_str(&pad);
            out.push_str(s);
        }
        Doc::Node(head, kids) if kids.is_empty() => {
            out.push_str(&format!("{pad}({head})"));
        }
        Doc::Node(head, kids) => {
            out.push_str(&format!("{pad}({head}\n"));
            for (i, k) in kids.iter().enumerate() {
                render(k, ind + 1, out);
                if i + 1 < kids.len() {
                    out.push('\n');
                }
            }
            out.push(')');
        }
    }
}

fn flatten(d: &Doc) -> String {
    match d {
        Doc::Line(s) => s.clone(),
        Doc::Node(head, kids) => {
            let mut s = format!("({head}");
            for k in kids {
                s.push(' ');
                s.push_str(&flatten(k));
            }
            s.push(')');
            s
        }
    }
}

// ---- items ------------------------------------------------------------------

fn item_doc(it: &Item) -> Doc {
    match it {
        Item::Import(im) => {
            let clause = match &im.clause {
                None => "side-effect".to_string(),
                Some(ImportClause::Namespace(n)) => format!("namespace {}", n.text),
                Some(ImportClause::Named(specs)) => format!("names {}", aliases(specs)),
            };
            Doc::Line(format!("(import \"{}\" {clause})", im.from))
        }
        Item::Export(ex) => {
            let head = if ex.is_extern { "export extern" } else { "export" };
            match &ex.kind {
                ExportKind::Decl(d) => Doc::Node(head.to_string(), vec![decl_doc(d)]),
                ExportKind::Var(v) => Doc::Node(head.to_string(), vec![var_doc(v)]),
                ExportKind::Named { specs, from } => {
                    let from = match from {
                        Some(f) => format!(" from \"{f}\""),
                        None => String::new(),
                    };
                    Doc::Line(format!("({head} {}{from})", aliases(specs)))
                }
            }
        }
        Item::Decl(d) => decl_doc(d),
        Item::Stmt(s) => stmt_doc(s),
    }
}

fn aliases(specs: &[NameAlias]) -> String {
    specs
        .iter()
        .map(|s| match &s.alias {
            Some(a) => format!("({} as {})", s.name.text, a.text),
            None => s.name.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- declarations -------------------------------------------------------------

fn decl_doc(d: &Decl) -> Doc {
    match d {
        Decl::Function(f) => {
            let mut head = String::from("function ");
            if f.is_async {
                head = "async function ".to_string();
            }
            head.push_str(&f.name.text);
            head.push_str(&tparams(&f.type_params));
            head.push_str(&format!(" {}", params_str(&f.params)));
            if let Some(r) = &f.ret {
                head.push_str(&format!(" : {}", ty(r)));
            }
            Doc::Node(head, f.body.iter().map(stmt_doc).collect())
        }
        Decl::Class(c) => {
            let mut head = String::new();
            if c.is_abstract {
                head.push_str("abstract ");
            }
            if c.is_final {
                head.push_str("final ");
            }
            head.push_str(&format!("class {}{}", c.name.text, tparams(&c.type_params)));
            if let Some(e) = &c.extends {
                head.push_str(&format!(" extends {}", ty(e)));
            }
            if !c.implements.is_empty() {
                let list: Vec<String> = c.implements.iter().map(ty).collect();
                head.push_str(&format!(" implements {}", list.join(", ")));
            }
            Doc::Node(head, c.members.iter().map(member_doc).collect())
        }
        Decl::Interface(i) => {
            let mut head = format!("interface {}{}", i.name.text, tparams(&i.type_params));
            if !i.extends.is_empty() {
                let list: Vec<String> = i.extends.iter().map(ty).collect();
                head.push_str(&format!(" extends {}", list.join(", ")));
            }
            let kids = i
                .members
                .iter()
                .map(|m| match m {
                    InterfaceMember::Prop { readonly, name, optional, ty: t } => {
                        let ro = if *readonly { "readonly " } else { "" };
                        let opt = if *optional { "?" } else { "" };
                        Doc::Line(format!("(prop {ro}{name}{opt} {})", ty(t)))
                    }
                    InterfaceMember::Method { name, type_params, params, ret } => Doc::Line(
                        format!(
                            "(method {name}{} {} : {})",
                            tparams(type_params),
                            params_str(params),
                            ty(ret)
                        ),
                    ),
                })
                .collect();
            Doc::Node(head, kids)
        }
        Decl::Enum(e) => {
            let backing = match &e.backing {
                Some(b) => format!(" : {}", b.text),
                None => String::new(),
            };
            let kids = e
                .members
                .iter()
                .map(|(n, v)| match v {
                    Some(v) => Doc::Line(format!("(member {} = {})", n.text, expr(v))),
                    None => Doc::Line(format!("(member {})", n.text)),
                })
                .collect();
            Doc::Node(format!("enum {}{backing}", e.name.text), kids)
        }
        Decl::TypeAlias(t) => Doc::Line(format!(
            "(type {}{} = {})",
            t.name.text,
            tparams(&t.type_params),
            ty(&t.ty)
        )),
    }
}

fn mods_str(m: &MemberMods) -> String {
    let mut s = String::new();
    if let Some(a) = m.access {
        s.push_str(a.as_str());
        s.push(' ');
    }
    if m.is_static {
        s.push_str("static ");
    }
    if let Some(v) = m.virt {
        s.push_str(v.as_str());
        s.push(' ');
    }
    s
}

fn member_doc(m: &ClassMember) -> Doc {
    match m {
        ClassMember::Field { mods, readonly, name, ty: t, init } => {
            let ro = if *readonly { "readonly " } else { "" };
            let init = match init {
                Some(e) => format!(" = {}", expr(e)),
                None => String::new(),
            };
            Doc::Line(format!("(field {}{ro}{name} {}{init})", mods_str(mods), ty(t)))
        }
        ClassMember::Method { mods, is_async, name, type_params, params, ret, body } => {
            let a = if *is_async { "async " } else { "" };
            let head = format!(
                "method {}{a}{name}{} {} : {}",
                mods_str(mods),
                tparams(type_params),
                params_str(params),
                ty(ret)
            );
            match body {
                Some(b) => Doc::Node(head, b.iter().map(stmt_doc).collect()),
                None => Doc::Line(format!("({head} ;)")),
            }
        }
        ClassMember::Getter { mods, name, ret, body } => Doc::Node(
            format!("get {}{name} : {}", mods_str(mods), ty(ret)),
            body.iter().map(stmt_doc).collect(),
        ),
        ClassMember::Setter { mods, name, param, body } => Doc::Node(
            format!("set {}{name} {}", mods_str(mods), param_str(param)),
            body.iter().map(stmt_doc).collect(),
        ),
        ClassMember::Ctor { access, params, body } => {
            let a = match access {
                Some(a) => format!("{} ", a.as_str()),
                None => String::new(),
            };
            Doc::Node(
                format!("constructor {a}{}", params_str(params)),
                body.iter().map(stmt_doc).collect(),
            )
        }
    }
}

// ---- statements ----------------------------------------------------------------

fn var_doc(v: &VarStmt) -> Doc {
    Doc::Line(var_str(v))
}

fn var_str(v: &VarStmt) -> String {
    let bs: Vec<String> = v
        .bindings
        .iter()
        .map(|b| {
            let mut s = format!("({}", pattern(&b.target));
            if let Some(t) = &b.ty {
                s.push_str(&format!(" {}", ty(t)));
            }
            if let Some(e) = &b.init {
                s.push_str(&format!(" = {}", expr(e)));
            }
            s.push(')');
            s
        })
        .collect();
    format!("({} {})", v.kind.as_str(), bs.join(" "))
}

fn stmt_doc(s: &Stmt) -> Doc {
    match s {
        Stmt::Block(b) => Doc::Node("block".to_string(), b.iter().map(stmt_doc).collect()),
        Stmt::Var(v) => var_doc(v),
        Stmt::Expr(e) => Doc::Line(format!("(expr {})", expr(e))),
        Stmt::Empty => Doc::Line("(empty)".to_string()),
        Stmt::If { cond, then, els } => {
            let mut kids = vec![stmt_doc(then)];
            if let Some(e) = els {
                kids.push(Doc::Node("else".to_string(), vec![stmt_doc(e)]));
            }
            Doc::Node(format!("if {}", expr(cond)), kids)
        }
        Stmt::While { cond, body } => {
            Doc::Node(format!("while {}", expr(cond)), vec![stmt_doc(body)])
        }
        Stmt::DoWhile { body, cond } => {
            Doc::Node(format!("do-while {}", expr(cond)), vec![stmt_doc(body)])
        }
        Stmt::For { init, cond, step, body } => {
            let init = match init {
                None => String::new(),
                Some(ForInit::Var(v)) => var_str(v),
                Some(ForInit::Exprs(es)) => {
                    es.iter().map(expr).collect::<Vec<_>>().join(", ")
                }
            };
            let cond = cond.as_ref().map(expr).unwrap_or_default();
            let step = step.iter().map(expr).collect::<Vec<_>>().join(", ");
            Doc::Node(format!("for [{init} ; {cond} ; {step}]"), vec![stmt_doc(body)])
        }
        Stmt::ForOf { is_await, kind, target, ty: t, iter, body } => {
            let aw = if *is_await { "await " } else { "" };
            let ann = match t {
                Some(t) => format!(" {}", ty(t)),
                None => String::new(),
            };
            Doc::Node(
                format!("for-of {aw}{} {}{ann} of {}", kind.as_str(), pattern(target), expr(iter)),
                vec![stmt_doc(body)],
            )
        }
        Stmt::Switch { scrutinee, clauses } => {
            let kids = clauses
                .iter()
                .map(|c| {
                    let head = match &c.test {
                        Some(e) => format!("case {}", expr(e)),
                        None => "default".to_string(),
                    };
                    Doc::Node(head, c.body.iter().map(stmt_doc).collect())
                })
                .collect();
            Doc::Node(format!("switch {}", expr(scrutinee)), kids)
        }
        Stmt::Break { label, .. } => Doc::Line(match label {
            Some(l) => format!("(break {})", l.text),
            None => "(break)".to_string(),
        }),
        Stmt::Continue { label, .. } => Doc::Line(match label {
            Some(l) => format!("(continue {})", l.text),
            None => "(continue)".to_string(),
        }),
        Stmt::Return { value, .. } => Doc::Line(match value {
            Some(e) => format!("(return {})", expr(e)),
            None => "(return)".to_string(),
        }),
        Stmt::Throw(e) => Doc::Line(format!("(throw {})", expr(e))),
        Stmt::Try { block, catches, finally } => {
            let mut kids = vec![Doc::Node(
                "block".to_string(),
                block.iter().map(stmt_doc).collect(),
            )];
            for c in catches {
                kids.push(Doc::Node(
                    format!("catch ({} {})", c.name.text, ty(&c.ty)),
                    c.block.iter().map(stmt_doc).collect(),
                ));
            }
            if let Some(f) = finally {
                kids.push(Doc::Node("finally".to_string(), f.iter().map(stmt_doc).collect()));
            }
            Doc::Node("try".to_string(), kids)
        }
        Stmt::Labeled { label, body } => {
            Doc::Node(format!("label {}", label.text), vec![stmt_doc(body)])
        }
    }
}

// ---- expressions -----------------------------------------------------------------

fn expr(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => n.text.clone(),
        Expr::This(_) => "this".to_string(),
        Expr::Lit { text, .. } => text.clone(),
        Expr::Template(parts) => {
            let inner: Vec<String> = parts
                .iter()
                .map(|p| match p {
                    TplPart::Text(t) => format!("{t:?}"),
                    TplPart::Expr(e) => expr(e),
                })
                .collect();
            format!("(template {})", inner.join(" "))
        }
        Expr::Array(elems) => format!("(array {})", elems_str(elems)),
        Expr::Record(fields) => {
            let fs: Vec<String> = fields
                .iter()
                .map(|f| match f {
                    RecordField::Named { name, value: Some(v) } => {
                        format!("({} {})", name.text, expr(v))
                    }
                    RecordField::Named { name, value: None } => format!("({})", name.text),
                    RecordField::Spread(e) => format!("(... {})", expr(e)),
                })
                .collect();
            format!("(record {})", fs.join(" "))
        }
        Expr::Paren(e) => format!("(paren {})", expr(e)),
        Expr::Arrow { is_async, params, ret, body } => {
            let a = if *is_async { "async-" } else { "" };
            let r = match ret {
                Some(t) => format!(" : {}", ty(t)),
                None => String::new(),
            };
            let b = match body {
                ArrowBody::Expr(e) => expr(e),
                ArrowBody::Block(stmts) => flatten(&Doc::Node(
                    "block".to_string(),
                    stmts.iter().map(stmt_doc).collect(),
                )),
            };
            format!("({a}arrow {}{r} {b})", params_str(params))
        }
        Expr::Unary { op, expr: e, .. } => format!("({} {})", op.as_str(), expr(e)),
        Expr::Update { prefix, inc, expr: e } => {
            let op = if *inc { "++" } else { "--" };
            let side = if *prefix { "pre" } else { "post" };
            format!("({op}{side} {})", expr(e))
        }
        Expr::Binary { op, l, r } => format!("({} {} {})", op.as_str(), expr(l), expr(r)),
        Expr::Assign { op, target, value } => {
            format!("({op} {} {})", expr(target), expr(value))
        }
        Expr::Cond { cond, then, els } => {
            format!("(?: {} {} {})", expr(cond), expr(then), expr(els))
        }
        Expr::Cast { expr: e, wrapping, ty: t } => {
            let kw = if *wrapping { "as-wrapping" } else { "as" };
            format!("({kw} {} {})", expr(e), ty(t))
        }
        Expr::Call { callee, type_args, args, optional } => {
            let q = if *optional { "?." } else { "" };
            let ta = if type_args.is_empty() {
                String::new()
            } else {
                let list: Vec<String> = type_args.iter().map(ty).collect();
                format!(" <{}>", list.join(", "))
            };
            format!("({q}call {}{ta} {})", expr(callee), elems_str(args))
        }
        Expr::New { ty: t, args } => format!("(new {} {})", ty(t), elems_str(args)),
        Expr::Member { obj, name, optional } => {
            let op = if *optional { "?." } else { "." };
            format!("({op} {} {name})", expr(obj))
        }
        Expr::Index { obj, index, optional } => {
            let op = if *optional { "?.[]" } else { "[]" };
            format!("({op} {} {})", expr(obj), expr(index))
        }
        Expr::SuperMember { name, .. } => format!("(. super {name})"),
        Expr::SuperCall { args, .. } => format!("(call super {})", elems_str(args)),
        Expr::ImportCall(e) => format!("(import-call {})", expr(e)),
    }
}

fn elems_str(elems: &[ArrayElem]) -> String {
    elems
        .iter()
        .map(|a| {
            if a.spread {
                format!("(... {})", expr(&a.expr))
            } else {
                expr(&a.expr)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- params, patterns, types ---------------------------------------------------

fn param_str(p: &Param) -> String {
    let mut s = String::from("(");
    if p.rest {
        s.push_str("...");
    }
    s.push_str(&pattern(&p.target));
    if p.optional {
        s.push('?');
    }
    if let Some(t) = &p.ty {
        s.push_str(&format!(" {}", ty(t)));
    }
    if let Some(d) = &p.default {
        s.push_str(&format!(" = {}", expr(d)));
    }
    s.push(')');
    s
}

fn params_str(params: &[Param]) -> String {
    let ps: Vec<String> = params.iter().map(param_str).collect();
    format!("(params {})", ps.join(" "))
}

fn tparams(tps: &[TypeParam]) -> String {
    if tps.is_empty() {
        return String::new();
    }
    let list: Vec<String> = tps
        .iter()
        .map(|t| match &t.constraint {
            Some(c) => format!("{} extends {}", t.name.text, ty(c)),
            None => t.name.text.clone(),
        })
        .collect();
    format!("<{}>", list.join(", "))
}

fn pattern(p: &Pattern) -> String {
    match p {
        Pattern::Name(n) => n.text.clone(),
        Pattern::Array { elems, rest } => {
            let mut parts: Vec<String> = elems
                .iter()
                .map(|e| match &e.default {
                    Some(d) => format!("{} = {}", pattern(&e.target), expr(d)),
                    None => pattern(&e.target),
                })
                .collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", pattern(r)));
            }
            format!("[{}]", parts.join(", "))
        }
        Pattern::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    let mut s = f.name.text.clone();
                    if let Some(t) = &f.target {
                        s.push_str(&format!(": {}", pattern(t)));
                    }
                    if let Some(d) = &f.default {
                        s.push_str(&format!(" = {}", expr(d)));
                    }
                    s
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn ty(t: &Type) -> String {
    match t {
        Type::Named { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                let list: Vec<String> = args.iter().map(ty).collect();
                format!("{name}<{}>", list.join(", "))
            }
        }
        Type::Nullable(t) => format!("{}?", ty(t)),
        Type::ArrayOf(t) => format!("{}[]", ty(t)),
        Type::Union(arms) => {
            let list: Vec<String> = arms.iter().map(ty).collect();
            format!("({})", list.join(" | "))
        }
        Type::Tuple(ts) => {
            let list: Vec<String> = ts.iter().map(ty).collect();
            format!("[{}]", list.join(", "))
        }
        Type::Record(members) => {
            let list: Vec<String> = members
                .iter()
                .map(|m| {
                    let ro = if m.readonly { "readonly " } else { "" };
                    let opt = if m.optional { "?" } else { "" };
                    format!("{ro}{}{opt}: {}", m.name, ty(&m.ty))
                })
                .collect();
            format!("{{{}}}", list.join(", "))
        }
        Type::Function { type_params, params, ret } => {
            let list: Vec<String> = params
                .iter()
                .map(|p| {
                    let rest = if p.rest { "..." } else { "" };
                    let opt = if p.optional { "?" } else { "" };
                    format!("{rest}{}{opt}: {}", p.name, ty(&p.ty))
                })
                .collect();
            format!("({}({}) => {})", tparams(type_params), list.join(", "), ty(ret))
        }
    }
}
