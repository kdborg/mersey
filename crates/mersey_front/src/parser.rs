//! Recursive-descent parser, grammar §6.3–§6.8, disambiguations §6.9.
//!
//! Error style: `Err(())` means a diagnostic was already pushed. Recovery
//! happens at statement/member boundaries (`sync_stmt`/`sync_member`).
//! Speculative parses (`try_parse`) roll back the cursor and diagnostics.
//!
//! `>>`/`>>=`/`>=` splitting when closing type-argument lists (§6.9 note 4)
//! is done with a "carry": the current token is virtually replaced by its
//! remainder after a leading `>` is consumed, without mutating the token
//! buffer, so speculation can roll it back.

use crate::ast::*;
use crate::diag::{Code, Diagnostic, Pos};
use crate::lexer::{self, LexOutput};
use crate::source::SourceFile;
use crate::token::{Keyword as Kw, Punct as P, Token, TokenKind as TK};

pub struct ParseOutput {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(source: &SourceFile) -> ParseOutput {
    let LexOutput { tokens, diagnostics } = lexer::lex(source);
    let mut p = Parser { text: &source.text, tokens, idx: 0, carry: None, diags: diagnostics };
    let module = p.parse_module();
    ParseOutput { module, diagnostics: p.diags }
}

struct Parser<'s> {
    text: &'s str,
    tokens: Vec<Token>,
    idx: usize,
    carry: Option<(usize, P)>,
    diags: Vec<Diagnostic>,
}

type PResult<T> = Result<T, ()>;

/// Left-associative binary chain per grammar §6.4, one method per level.
macro_rules! left_chain {
    ($name:ident, $next:ident, $( $p:ident => $op:ident ),+ ) => {
        fn $name(&mut self) -> PResult<Expr> {
            let mut e = self.$next()?;
            loop {
                let op = match self.kind() {
                    $( TK::Punct(P::$p) => BinOp::$op, )+
                    _ => break,
                };
                self.advance();
                let r = self.$next()?;
                e = Expr::Binary { op, l: Box::new(e), r: Box::new(r) };
            }
            Ok(e)
        }
    };
}

impl<'s> Parser<'s> {
    // ---- cursor ----------------------------------------------------------

    fn kind(&self) -> TK {
        if let Some((i, p)) = self.carry {
            if i == self.idx {
                return TK::Punct(p);
            }
        }
        self.tokens[self.idx].kind
    }

    /// Lookahead. The carry only ever sits on the current token, so plain
    /// indexing is correct for `n > 0`.
    fn kind_at(&self, n: usize) -> TK {
        if n == 0 {
            return self.kind();
        }
        let i = (self.idx + n).min(self.tokens.len() - 1);
        self.tokens[i].kind
    }

    fn pos(&self) -> Pos {
        self.tokens[self.idx].span.pos
    }

    fn text_of(&self, n: usize) -> &'s str {
        let i = (self.idx + n).min(self.tokens.len() - 1);
        let t = &self.tokens[i];
        &self.text[t.span.start..t.span.end]
    }

    fn advance(&mut self) {
        if let Some((i, _)) = self.carry {
            if i == self.idx {
                self.carry = None;
            }
        }
        if self.idx < self.tokens.len() - 1 {
            self.idx += 1;
        }
    }

    fn at_punct(&self, p: P) -> bool {
        self.kind() == TK::Punct(p)
    }

    fn at_kw(&self, k: Kw) -> bool {
        self.kind() == TK::Keyword(k)
    }

    fn eat_punct(&mut self, p: P) -> bool {
        if self.at_punct(p) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.at_kw(k) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn describe(&self) -> String {
        match self.kind() {
            TK::Eof => "end of file".to_string(),
            TK::Punct(p) => format!("`{}`", p.as_str()),
            TK::Keyword(k) => format!("`{}`", k.as_str()),
            _ => format!("`{}`", self.text_of(0)),
        }
    }

    fn report(&mut self, code: Code, msg: impl Into<String>) {
        let pos = self.pos();
        self.diags.push(Diagnostic::error(code, msg, pos));
    }

    fn err<T>(&mut self, code: Code, msg: impl Into<String>) -> PResult<T> {
        self.report(code, msg);
        Err(())
    }

    fn expected<T>(&mut self, what: &str) -> PResult<T> {
        let found = self.describe();
        self.err(Code::UnexpectedToken, format!("expected {what}, found {found}"))
    }

    fn expect_punct(&mut self, p: P) -> PResult<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.expected(&format!("`{}`", p.as_str()))
        }
    }

    fn expect_kw(&mut self, k: Kw) -> PResult<()> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            self.expected(&format!("`{}`", k.as_str()))
        }
    }

    /// Identifier in binding position: reserved words rejected (§2.5).
    fn expect_ident(&mut self, what: &str) -> PResult<Name> {
        match self.kind() {
            TK::Ident => {
                let name = Name { text: self.text_of(0).to_string(), pos: self.pos() };
                self.advance();
                Ok(name)
            }
            TK::Keyword(k) => {
                let msg =
                    format!("`{}` is a reserved word and cannot be used as {what}", k.as_str());
                self.err(Code::ReservedBinding, msg)
            }
            _ => self.expected(what),
        }
    }

    /// IdentifierName: member positions admit reserved words (§6.9 note 1).
    fn expect_member_name(&mut self) -> PResult<String> {
        match self.kind() {
            TK::Ident => {
                let s = self.text_of(0).to_string();
                self.advance();
                Ok(s)
            }
            TK::Keyword(k) => {
                self.advance();
                Ok(k.as_str().to_string())
            }
            _ => self.expected("a member name"),
        }
    }

    fn at_member_name(&self, n: usize) -> bool {
        matches!(self.kind_at(n), TK::Ident | TK::Keyword(_))
    }

    /// Close a type-argument list, splitting `>>`, `>>=`, `>=` (§6.9 note 4).
    fn expect_gt(&mut self) -> PResult<()> {
        match self.kind() {
            TK::Punct(P::Gt) => {
                self.advance();
                Ok(())
            }
            TK::Punct(P::Shr) => {
                self.carry = Some((self.idx, P::Gt));
                Ok(())
            }
            TK::Punct(P::ShrEq) => {
                self.carry = Some((self.idx, P::GtEq));
                Ok(())
            }
            TK::Punct(P::GtEq) => {
                self.carry = Some((self.idx, P::Eq));
                Ok(())
            }
            _ => self.expected("`>`"),
        }
    }

    fn try_parse<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> Option<T> {
        let idx = self.idx;
        let carry = self.carry;
        let ndiags = self.diags.len();
        match f(self) {
            Ok(v) => Some(v),
            Err(()) => {
                self.idx = idx;
                self.carry = carry;
                self.diags.truncate(ndiags);
                None
            }
        }
    }

    // ---- recovery ----------------------------------------------------------

    fn sync_stmt(&mut self) {
        loop {
            match self.kind() {
                TK::Eof | TK::Punct(P::RBrace) => return,
                TK::Punct(P::Semi) => {
                    self.advance();
                    return;
                }
                TK::Keyword(
                    Kw::Let
                    | Kw::Const
                    | Kw::If
                    | Kw::For
                    | Kw::While
                    | Kw::Do
                    | Kw::Switch
                    | Kw::Return
                    | Kw::Throw
                    | Kw::Try
                    | Kw::Break
                    | Kw::Continue
                    | Kw::Import
                    | Kw::Export
                    | Kw::Function
                    | Kw::Class,
                ) => return,
                _ => self.advance(),
            }
        }
    }

    fn sync_member(&mut self) {
        loop {
            match self.kind() {
                TK::Eof | TK::Punct(P::RBrace) => return,
                TK::Punct(P::Semi) => {
                    self.advance();
                    return;
                }
                _ => self.advance(),
            }
        }
    }

    // ---- module ------------------------------------------------------------

    fn parse_module(&mut self) -> Module {
        let mut items = Vec::new();
        while self.kind() != TK::Eof {
            let before = self.idx;
            match self.parse_module_item() {
                Ok(item) => items.push(item),
                Err(()) => self.sync_stmt(),
            }
            if self.idx == before && self.kind() != TK::Eof {
                self.advance(); // guarantee progress
            }
        }
        Module { items }
    }

    fn parse_module_item(&mut self) -> PResult<Item> {
        match self.kind() {
            TK::Keyword(Kw::Import) if self.kind_at(1) != TK::Punct(P::LParen) => {
                Ok(Item::Import(self.parse_import()?))
            }
            TK::Keyword(Kw::Export) => Ok(Item::Export(self.parse_export()?)),
            _ if self.at_decl_start() => Ok(Item::Decl(self.parse_decl()?)),
            _ => Ok(Item::Stmt(self.parse_stmt()?)),
        }
    }

    fn at_decl_start(&self) -> bool {
        match self.kind() {
            TK::Keyword(Kw::Function | Kw::Class | Kw::Interface | Kw::Enum | Kw::Type) => true,
            TK::Keyword(Kw::Async) => self.kind_at(1) == TK::Keyword(Kw::Function),
            TK::Keyword(Kw::Abstract | Kw::Final) => matches!(
                self.kind_at(1),
                TK::Keyword(Kw::Class | Kw::Abstract | Kw::Final)
            ),
            _ => false,
        }
    }

    fn string_text(&self) -> String {
        let raw = self.text_of(0);
        raw[1..raw.len().saturating_sub(1)].to_string()
    }

    fn expect_string(&mut self, what: &str) -> PResult<String> {
        if self.kind() == TK::Str {
            let s = self.string_text();
            self.advance();
            Ok(s)
        } else {
            self.expected(what)
        }
    }

    fn parse_import(&mut self) -> PResult<ImportDecl> {
        self.expect_kw(Kw::Import)?;
        if self.kind() == TK::Str {
            let from = self.string_text();
            self.advance();
            self.expect_punct(P::Semi)?;
            return Ok(ImportDecl { clause: None, from });
        }
        let clause = if self.eat_punct(P::Star) {
            self.expect_kw(Kw::As)?;
            ImportClause::Namespace(self.expect_ident("a namespace name")?)
        } else {
            self.expect_punct(P::LBrace)?;
            let mut specs = Vec::new();
            while !self.at_punct(P::RBrace) {
                let name = self.expect_ident("an import name")?;
                let alias = if self.eat_kw(Kw::As) {
                    Some(self.expect_ident("an import alias")?)
                } else {
                    None
                };
                specs.push(NameAlias { name, alias });
                if !self.eat_punct(P::Comma) {
                    break;
                }
            }
            self.expect_punct(P::RBrace)?;
            ImportClause::Named(specs)
        };
        self.expect_kw(Kw::From)?;
        let from = self.expect_string("a module specifier")?;
        self.expect_punct(P::Semi)?;
        Ok(ImportDecl { clause: Some(clause), from })
    }

    fn parse_export(&mut self) -> PResult<ExportDecl> {
        self.expect_kw(Kw::Export)?;
        let is_extern = self.eat_kw(Kw::Extern);
        if self.at_punct(P::LBrace) {
            if is_extern {
                return self.err(
                    Code::UnexpectedToken,
                    "`export extern` applies to declarations, not re-export lists",
                );
            }
            self.advance();
            let mut specs = Vec::new();
            while !self.at_punct(P::RBrace) {
                let name = self.expect_ident("an export name")?;
                let alias = if self.eat_kw(Kw::As) {
                    Some(self.expect_ident("an export alias")?)
                } else {
                    None
                };
                specs.push(NameAlias { name, alias });
                if !self.eat_punct(P::Comma) {
                    break;
                }
            }
            self.expect_punct(P::RBrace)?;
            let from = if self.eat_kw(Kw::From) {
                Some(self.expect_string("a module specifier")?)
            } else {
                None
            };
            self.expect_punct(P::Semi)?;
            return Ok(ExportDecl { is_extern: false, kind: ExportKind::Named { specs, from } });
        }
        if matches!(self.kind(), TK::Keyword(Kw::Let | Kw::Const)) {
            let var = self.parse_var_stmt(true)?;
            return Ok(ExportDecl { is_extern, kind: ExportKind::Var(var) });
        }
        if self.at_decl_start() {
            let decl = self.parse_decl()?;
            return Ok(ExportDecl { is_extern, kind: ExportKind::Decl(decl) });
        }
        self.expected("a declaration, a variable statement, or `{` after `export`")
    }

    fn parse_decl(&mut self) -> PResult<Decl> {
        match self.kind() {
            TK::Keyword(Kw::Async | Kw::Function) => Ok(Decl::Function(self.parse_fn_decl()?)),
            TK::Keyword(Kw::Abstract | Kw::Final | Kw::Class) => {
                Ok(Decl::Class(self.parse_class_decl()?))
            }
            TK::Keyword(Kw::Interface) => Ok(Decl::Interface(self.parse_interface_decl()?)),
            TK::Keyword(Kw::Enum) => Ok(Decl::Enum(self.parse_enum_decl()?)),
            TK::Keyword(Kw::Type) => Ok(Decl::TypeAlias(self.parse_type_alias()?)),
            _ => self.expected("a declaration"),
        }
    }

    // ---- declarations --------------------------------------------------------

    fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
        let is_async = self.eat_kw(Kw::Async);
        self.expect_kw(Kw::Function)?;
        let name = self.expect_ident("a function name")?;
        let type_params = self.parse_type_params_opt()?;
        let params = self.parse_param_clause()?;
        let ret = if self.eat_punct(P::Colon) { Some(self.parse_type()?) } else { None };
        let body = self.parse_block()?;
        Ok(FnDecl { is_async, name, type_params, params, ret, body })
    }

    fn parse_type_params_opt(&mut self) -> PResult<Vec<TypeParam>> {
        let mut out = Vec::new();
        if !self.eat_punct(P::Lt) {
            return Ok(out);
        }
        loop {
            let name = self.expect_ident("a type parameter name")?;
            let constraint = if self.eat_kw(Kw::Extends) { Some(self.parse_type()?) } else { None };
            out.push(TypeParam { name, constraint });
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        self.expect_gt()?;
        Ok(out)
    }

    fn parse_param_clause(&mut self) -> PResult<Vec<Param>> {
        self.expect_punct(P::LParen)?;
        let mut params = Vec::new();
        while !self.at_punct(P::RParen) {
            params.push(self.parse_param()?);
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        self.expect_punct(P::RParen)?;
        Ok(params)
    }

    fn parse_param(&mut self) -> PResult<Param> {
        if self.eat_punct(P::DotDotDot) {
            let name = self.expect_ident("a rest parameter name")?;
            self.expect_punct(P::Colon)?;
            let ty = self.parse_type()?;
            return Ok(Param {
                rest: true,
                target: Pattern::Name(name),
                optional: false,
                ty: Some(ty),
                default: None,
            });
        }
        let target = self.parse_pattern()?;
        let optional = self.eat_punct(P::Question);
        let ty = if self.eat_punct(P::Colon) { Some(self.parse_type()?) } else { None };
        let default = if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
        Ok(Param { rest: false, target, optional, ty, default })
    }

    fn parse_class_decl(&mut self) -> PResult<ClassDecl> {
        let mut is_abstract = false;
        let mut is_final = false;
        loop {
            if !is_abstract && self.at_kw(Kw::Abstract) {
                self.advance();
                is_abstract = true;
            } else if !is_final && self.at_kw(Kw::Final) {
                self.advance();
                is_final = true;
            } else {
                break;
            }
        }
        self.expect_kw(Kw::Class)?;
        let name = self.expect_ident("a class name")?;
        let type_params = self.parse_type_params_opt()?;
        let extends = if self.eat_kw(Kw::Extends) { Some(self.parse_type_reference()?) } else { None };
        let mut implements = Vec::new();
        if self.eat_kw(Kw::Implements) {
            loop {
                implements.push(self.parse_type_reference()?);
                if !self.eat_punct(P::Comma) {
                    break;
                }
            }
        }
        self.expect_punct(P::LBrace)?;
        let mut members = Vec::new();
        while !self.at_punct(P::RBrace) && self.kind() != TK::Eof {
            let before = self.idx;
            match self.parse_class_member() {
                Ok(m) => members.push(m),
                Err(()) => self.sync_member(),
            }
            if self.idx == before && !self.at_punct(P::RBrace) {
                self.advance();
            }
        }
        self.expect_punct(P::RBrace)?;
        Ok(ClassDecl { is_abstract, is_final, name, type_params, extends, implements, members })
    }

    /// A modifier keyword counts as a modifier only when it is not itself
    /// the member name (`static(): void` is a method named `static`).
    fn is_modifier_here(&self, k: Kw) -> bool {
        self.at_kw(k) && !matches!(self.kind_at(1), TK::Punct(P::LParen | P::Lt | P::Colon))
    }

    fn parse_member_mods(&mut self) -> MemberMods {
        let access = match self.kind() {
            TK::Keyword(Kw::Public) if self.is_modifier_here(Kw::Public) => {
                self.advance();
                Some(Access::Public)
            }
            TK::Keyword(Kw::Protected) if self.is_modifier_here(Kw::Protected) => {
                self.advance();
                Some(Access::Protected)
            }
            TK::Keyword(Kw::Private) if self.is_modifier_here(Kw::Private) => {
                self.advance();
                Some(Access::Private)
            }
            _ => None,
        };
        let is_static = if self.is_modifier_here(Kw::Static) {
            self.advance();
            true
        } else {
            false
        };
        let virt = if self.is_modifier_here(Kw::Abstract) {
            self.advance();
            Some(Virt::Abstract)
        } else if self.is_modifier_here(Kw::Final) {
            self.advance();
            Some(Virt::Final)
        } else if self.is_modifier_here(Kw::Override) {
            self.advance();
            Some(Virt::Override)
        } else {
            None
        };
        MemberMods { access, is_static, virt }
    }

    fn parse_class_member(&mut self) -> PResult<ClassMember> {
        let mods = self.parse_member_mods();

        // Constructor: `constructor` is an ordinary identifier (§6.6).
        if self.kind() == TK::Ident
            && self.text_of(0) == "constructor"
            && self.kind_at(1) == TK::Punct(P::LParen)
        {
            if mods.is_static || mods.virt.is_some() {
                return self
                    .err(Code::UnexpectedToken, "a constructor takes only an access modifier");
            }
            self.advance();
            let params = self.parse_param_clause()?;
            let body = self.parse_block()?;
            return Ok(ClassMember::Ctor { access: mods.access, params, body });
        }

        // Accessors: `get name(` / `set name(`; `get(` is a method named
        // `get` (§6.9 note 2).
        if self.at_kw(Kw::Get)
            && self.at_member_name(1)
            && self.kind_at(2) == TK::Punct(P::LParen)
        {
            self.advance();
            let name = self.expect_member_name()?;
            self.expect_punct(P::LParen)?;
            self.expect_punct(P::RParen)?;
            self.expect_punct(P::Colon)?;
            let ret = self.parse_type()?;
            let body = self.parse_block()?;
            return Ok(ClassMember::Getter { mods, name, ret, body });
        }
        if self.at_kw(Kw::Set)
            && self.at_member_name(1)
            && self.kind_at(2) == TK::Punct(P::LParen)
        {
            self.advance();
            let name = self.expect_member_name()?;
            self.expect_punct(P::LParen)?;
            let param = self.parse_param()?;
            self.expect_punct(P::RParen)?;
            let body = self.parse_block()?;
            return Ok(ClassMember::Setter { mods, name, param, body });
        }

        let is_async = self.is_modifier_here(Kw::Async);
        if is_async {
            self.advance();
        }
        let readonly = self.is_modifier_here(Kw::Readonly);
        if readonly {
            self.advance();
        }

        let name = self.expect_member_name()?;
        if matches!(self.kind(), TK::Punct(P::LParen | P::Lt)) {
            if readonly {
                self.report(Code::UnexpectedToken, "`readonly` applies only to fields");
            }
            let type_params = self.parse_type_params_opt()?;
            let params = self.parse_param_clause()?;
            self.expect_punct(P::Colon)?;
            let ret = self.parse_type()?;
            let body = if self.eat_punct(P::Semi) { None } else { Some(self.parse_block()?) };
            return Ok(ClassMember::Method { mods, is_async, name, type_params, params, ret, body });
        }
        if is_async {
            return self.err(Code::UnexpectedToken, "`async` applies only to methods");
        }
        self.expect_punct(P::Colon)?;
        let ty = self.parse_type()?;
        let init = if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
        self.expect_punct(P::Semi)?;
        Ok(ClassMember::Field { mods, readonly, name, ty, init })
    }

    fn parse_interface_decl(&mut self) -> PResult<InterfaceDecl> {
        self.expect_kw(Kw::Interface)?;
        let name = self.expect_ident("an interface name")?;
        let type_params = self.parse_type_params_opt()?;
        let mut extends = Vec::new();
        if self.eat_kw(Kw::Extends) {
            loop {
                extends.push(self.parse_type_reference()?);
                if !self.eat_punct(P::Comma) {
                    break;
                }
            }
        }
        self.expect_punct(P::LBrace)?;
        let mut members = Vec::new();
        while !self.at_punct(P::RBrace) && self.kind() != TK::Eof {
            let readonly = self.is_modifier_here(Kw::Readonly);
            if readonly {
                self.advance();
            }
            let name = self.expect_member_name()?;
            if !readonly && matches!(self.kind(), TK::Punct(P::LParen | P::Lt)) {
                let type_params = self.parse_type_params_opt()?;
                let params = self.parse_param_clause()?;
                self.expect_punct(P::Colon)?;
                let ret = self.parse_type()?;
                members.push(InterfaceMember::Method { name, type_params, params, ret });
            } else {
                let optional = self.eat_punct(P::Question);
                self.expect_punct(P::Colon)?;
                let ty = self.parse_type()?;
                members.push(InterfaceMember::Prop { readonly, name, optional, ty });
            }
            if !self.eat_punct(P::Semi) && !self.eat_punct(P::Comma) && !self.at_punct(P::RBrace) {
                return self.expected("`;` or `,` after an interface member");
            }
        }
        self.expect_punct(P::RBrace)?;
        Ok(InterfaceDecl { name, type_params, extends, members })
    }

    fn parse_enum_decl(&mut self) -> PResult<EnumDecl> {
        self.expect_kw(Kw::Enum)?;
        let name = self.expect_ident("an enum name")?;
        let backing = if self.eat_punct(P::Colon) {
            match self.kind() {
                TK::Keyword(k) => {
                    let pos = self.pos();
                    self.advance();
                    Some(Name { text: k.as_str().to_string(), pos })
                }
                _ => return self.expected("an integer type"),
            }
        } else {
            None
        };
        self.expect_punct(P::LBrace)?;
        let mut members = Vec::new();
        while !self.at_punct(P::RBrace) {
            let mname = self.expect_ident("an enum member name")?;
            let value = if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
            members.push((mname, value));
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        self.expect_punct(P::RBrace)?;
        Ok(EnumDecl { name, backing, members })
    }

    fn parse_type_alias(&mut self) -> PResult<TypeAliasDecl> {
        self.expect_kw(Kw::Type)?;
        let name = self.expect_ident("a type alias name")?;
        let type_params = self.parse_type_params_opt()?;
        self.expect_punct(P::Eq)?;
        let ty = self.parse_type()?;
        self.expect_punct(P::Semi)?;
        Ok(TypeAliasDecl { name, type_params, ty })
    }

    // ---- statements -----------------------------------------------------------

    fn parse_block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_punct(P::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at_punct(P::RBrace) && self.kind() != TK::Eof {
            let before = self.idx;
            match self.parse_stmt() {
                Ok(s) => stmts.push(s),
                Err(()) => self.sync_stmt(),
            }
            if self.idx == before && !self.at_punct(P::RBrace) {
                self.advance();
            }
        }
        self.expect_punct(P::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        match self.kind() {
            TK::Punct(P::LBrace) => Ok(Stmt::Block(self.parse_block()?)),
            TK::Punct(P::Semi) => {
                self.advance();
                Ok(Stmt::Empty)
            }
            TK::Keyword(Kw::Let | Kw::Const) => Ok(Stmt::Var(self.parse_var_stmt(true)?)),
            TK::Keyword(Kw::If) => self.parse_if(),
            TK::Keyword(Kw::While) => {
                self.advance();
                self.expect_punct(P::LParen)?;
                let cond = self.parse_expr()?;
                self.expect_punct(P::RParen)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::While { cond, body })
            }
            TK::Keyword(Kw::Do) => {
                self.advance();
                let body = Box::new(self.parse_stmt()?);
                self.expect_kw(Kw::While)?;
                self.expect_punct(P::LParen)?;
                let cond = self.parse_expr()?;
                self.expect_punct(P::RParen)?;
                self.expect_punct(P::Semi)?;
                Ok(Stmt::DoWhile { body, cond })
            }
            TK::Keyword(Kw::For) => self.parse_for(),
            TK::Keyword(Kw::Switch) => self.parse_switch(),
            TK::Keyword(Kw::Break) => {
                let pos = self.pos();
                self.advance();
                let label = self.opt_label()?;
                self.expect_punct(P::Semi)?;
                Ok(Stmt::Break { label, pos })
            }
            TK::Keyword(Kw::Continue) => {
                let pos = self.pos();
                self.advance();
                let label = self.opt_label()?;
                self.expect_punct(P::Semi)?;
                Ok(Stmt::Continue { label, pos })
            }
            TK::Keyword(Kw::Return) => {
                let pos = self.pos();
                self.advance();
                let value = if self.at_punct(P::Semi) { None } else { Some(self.parse_expr()?) };
                self.expect_punct(P::Semi)?;
                Ok(Stmt::Return { value, pos })
            }
            TK::Keyword(Kw::Throw) => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect_punct(P::Semi)?;
                Ok(Stmt::Throw(e))
            }
            TK::Keyword(Kw::Try) => self.parse_try(),
            _ if self.at_decl_start() => {
                self.report(
                    Code::MisplacedDeclaration,
                    "declarations are only allowed at module level (§6.7); \
                     for a local function use `const f = (…) => …;`",
                );
                let _ = self.parse_decl()?; // parse & discard to recover cleanly
                Ok(Stmt::Empty)
            }
            TK::Ident if self.kind_at(1) == TK::Punct(P::Colon) => {
                let label = Name { text: self.text_of(0).to_string(), pos: self.pos() };
                self.advance();
                self.advance();
                let body = self.parse_stmt()?;
                if !matches!(
                    body,
                    Stmt::While { .. } | Stmt::DoWhile { .. } | Stmt::For { .. } | Stmt::ForOf { .. }
                ) {
                    self.report(Code::InvalidLabel, "a label must be attached to a loop (§6.5)");
                }
                Ok(Stmt::Labeled { label, body: Box::new(body) })
            }
            _ => {
                let e = self.parse_expr()?;
                self.expect_punct(P::Semi)?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn opt_label(&mut self) -> PResult<Option<Name>> {
        if self.kind() == TK::Ident {
            let name = Name { text: self.text_of(0).to_string(), pos: self.pos() };
            self.advance();
            Ok(Some(name))
        } else {
            Ok(None)
        }
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        self.expect_kw(Kw::If)?;
        self.expect_punct(P::LParen)?;
        let cond = self.parse_expr()?;
        self.expect_punct(P::RParen)?;
        let then = Box::new(self.parse_stmt()?);
        let els = if self.eat_kw(Kw::Else) { Some(Box::new(self.parse_stmt()?)) } else { None };
        Ok(Stmt::If { cond, then, els })
    }

    fn parse_var_stmt(&mut self, want_semi: bool) -> PResult<VarStmt> {
        let kind = if self.eat_kw(Kw::Let) {
            VarKind::Let
        } else {
            self.expect_kw(Kw::Const)?;
            VarKind::Const
        };
        let mut bindings = Vec::new();
        loop {
            bindings.push(self.parse_binding()?);
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        if want_semi {
            self.expect_punct(P::Semi)?;
        }
        Ok(VarStmt { kind, bindings })
    }

    fn parse_binding(&mut self) -> PResult<Binding> {
        let target = self.parse_pattern()?;
        let ty = if self.eat_punct(P::Colon) { Some(self.parse_type()?) } else { None };
        let init = if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
        Ok(Binding { target, ty, init })
    }

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        match self.kind() {
            TK::Punct(P::LBracket) => {
                self.advance();
                let mut elems = Vec::new();
                let mut rest = None;
                while !self.at_punct(P::RBracket) {
                    if self.eat_punct(P::DotDotDot) {
                        rest = Some(Box::new(self.parse_pattern()?));
                        break;
                    }
                    let target = self.parse_pattern()?;
                    let default =
                        if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
                    elems.push(PatternElem { target, default });
                    if !self.eat_punct(P::Comma) {
                        break;
                    }
                }
                self.expect_punct(P::RBracket)?;
                Ok(Pattern::Array { elems, rest })
            }
            TK::Punct(P::LBrace) => {
                self.advance();
                let mut fields = Vec::new();
                while !self.at_punct(P::RBrace) {
                    let pos = self.pos();
                    let was_kw = matches!(self.kind(), TK::Keyword(_));
                    let name = Name { text: self.expect_member_name()?, pos };
                    let target = if self.eat_punct(P::Colon) {
                        Some(self.parse_pattern()?)
                    } else {
                        if was_kw {
                            let n = &name.text;
                            self.report(
                                Code::ReservedBinding,
                                format!(
                                    "shorthand `{{{n}}}` would bind the reserved word \
                                     `{n}`; write `{n}: otherName`"
                                ),
                            );
                        }
                        None
                    };
                    let default =
                        if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
                    fields.push(PatternField { name, target, default });
                    if !self.eat_punct(P::Comma) {
                        break;
                    }
                }
                self.expect_punct(P::RBrace)?;
                Ok(Pattern::Record(fields))
            }
            _ => Ok(Pattern::Name(self.expect_ident("a binding name")?)),
        }
    }

    fn parse_for(&mut self) -> PResult<Stmt> {
        self.expect_kw(Kw::For)?;
        let is_await = self.eat_kw(Kw::Await);
        self.expect_punct(P::LParen)?;

        // for-of requires let/const (§6.5).
        if matches!(self.kind(), TK::Keyword(Kw::Let | Kw::Const)) {
            let kind = if self.eat_kw(Kw::Let) {
                VarKind::Let
            } else {
                self.expect_kw(Kw::Const)?;
                VarKind::Const
            };
            let target = self.parse_pattern()?;
            let ty = if self.eat_punct(P::Colon) { Some(self.parse_type()?) } else { None };
            if self.eat_kw(Kw::Of) {
                let iter = self.parse_assignment()?;
                self.expect_punct(P::RParen)?;
                let body = Box::new(self.parse_stmt()?);
                return Ok(Stmt::ForOf { is_await, kind, target, ty, iter, body });
            }
            if is_await {
                return self.err(Code::UnexpectedToken, "`for await` requires `of` (§6.5)");
            }
            // Classic for with declaration init: finish this binding, then
            // any further ones.
            let init0 = if self.eat_punct(P::Eq) { Some(self.parse_assignment()?) } else { None };
            let mut bindings = vec![Binding { target, ty, init: init0 }];
            while self.eat_punct(P::Comma) {
                bindings.push(self.parse_binding()?);
            }
            let var = VarStmt { kind, bindings };
            return self.parse_for_tail(Some(ForInit::Var(var)));
        }
        if is_await {
            return self.err(Code::UnexpectedToken, "`for await` requires `of` (§6.5)");
        }
        if self.at_punct(P::Semi) {
            return self.parse_for_tail(None);
        }
        let exprs = self.parse_expr_list()?;
        self.parse_for_tail(Some(ForInit::Exprs(exprs)))
    }

    fn parse_for_tail(&mut self, init: Option<ForInit>) -> PResult<Stmt> {
        self.expect_punct(P::Semi)?;
        let cond = if self.at_punct(P::Semi) { None } else { Some(self.parse_expr()?) };
        self.expect_punct(P::Semi)?;
        let step = if self.at_punct(P::RParen) { Vec::new() } else { self.parse_expr_list()? };
        self.expect_punct(P::RParen)?;
        let body = Box::new(self.parse_stmt()?);
        Ok(Stmt::For { init, cond, step, body })
    }

    fn parse_expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut out = vec![self.parse_assignment()?];
        while self.eat_punct(P::Comma) {
            out.push(self.parse_assignment()?);
        }
        Ok(out)
    }

    fn parse_switch(&mut self) -> PResult<Stmt> {
        self.expect_kw(Kw::Switch)?;
        self.expect_punct(P::LParen)?;
        let scrutinee = self.parse_expr()?;
        self.expect_punct(P::RParen)?;
        self.expect_punct(P::LBrace)?;
        let mut clauses = Vec::new();
        let mut seen_default = false;
        while !self.at_punct(P::RBrace) && self.kind() != TK::Eof {
            let test = if self.eat_kw(Kw::Case) {
                let e = self.parse_assignment()?;
                Some(e)
            } else if self.at_kw(Kw::Default) {
                self.advance();
                if seen_default {
                    self.report(Code::UnexpectedToken, "duplicate `default` clause");
                }
                seen_default = true;
                None
            } else {
                return self.expected("`case`, `default`, or `}`");
            };
            self.expect_punct(P::Colon)?;
            let mut body = Vec::new();
            while !matches!(
                self.kind(),
                TK::Keyword(Kw::Case | Kw::Default) | TK::Punct(P::RBrace) | TK::Eof
            ) {
                let before = self.idx;
                match self.parse_stmt() {
                    Ok(s) => body.push(s),
                    Err(()) => self.sync_stmt(),
                }
                if self.idx == before {
                    break;
                }
            }
            clauses.push(SwitchClause { test, body });
        }
        self.expect_punct(P::RBrace)?;
        Ok(Stmt::Switch { scrutinee, clauses })
    }

    fn parse_try(&mut self) -> PResult<Stmt> {
        self.expect_kw(Kw::Try)?;
        let block = self.parse_block()?;
        let mut catches = Vec::new();
        while self.at_kw(Kw::Catch) {
            self.advance();
            self.expect_punct(P::LParen)?;
            let name = self.expect_ident("a catch binding")?;
            self.expect_punct(P::Colon)?;
            let ty = self.parse_type()?;
            self.expect_punct(P::RParen)?;
            let cblock = self.parse_block()?;
            catches.push(Catch { name, ty, block: cblock });
        }
        let finally = if self.eat_kw(Kw::Finally) { Some(self.parse_block()?) } else { None };
        if catches.is_empty() && finally.is_none() {
            return self.expected("`catch` or `finally` after `try` block");
        }
        Ok(Stmt::Try { block, catches, finally })
    }

    // ---- expressions -----------------------------------------------------------

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> PResult<Expr> {
        // Arrow forms (§6.4): bare-ident, async bare-ident, (params) via
        // speculation, async (params) directly (async is reserved, so
        // `async (` can only start an arrow).
        if self.kind() == TK::Ident && self.kind_at(1) == TK::Punct(P::Arrow) {
            return self.parse_bare_arrow(false);
        }
        if self.at_kw(Kw::Async) {
            if self.kind_at(1) == TK::Ident && self.kind_at(2) == TK::Punct(P::Arrow) {
                self.advance();
                return self.parse_bare_arrow(true);
            }
            if self.kind_at(1) == TK::Punct(P::LParen) {
                self.advance();
                let (params, ret) = self.parse_arrow_header()?;
                return self.parse_arrow_body(true, params, ret);
            }
        }
        if self.at_punct(P::LParen) {
            if let Some((params, ret)) = self.try_parse(|p| p.parse_arrow_header()) {
                return self.parse_arrow_body(false, params, ret);
            }
        }

        let e = self.parse_conditional()?;
        if let Some(op) = self.assign_op() {
            if !matches!(
                e,
                Expr::Ident(_) | Expr::Member { .. } | Expr::Index { .. } | Expr::SuperMember { .. }
            ) {
                self.report(
                    Code::InvalidAssignmentTarget,
                    "invalid assignment target; expected a variable, member, or index",
                );
            }
            self.advance();
            let value = self.parse_assignment()?;
            return Ok(Expr::Assign { op, target: Box::new(e), value: Box::new(value) });
        }
        Ok(e)
    }

    fn assign_op(&self) -> Option<&'static str> {
        Some(match self.kind() {
            TK::Punct(P::Eq) => "=",
            TK::Punct(P::PlusEq) => "+=",
            TK::Punct(P::MinusEq) => "-=",
            TK::Punct(P::StarEq) => "*=",
            TK::Punct(P::SlashEq) => "/=",
            TK::Punct(P::PercentEq) => "%=",
            TK::Punct(P::StarStarEq) => "**=",
            TK::Punct(P::ShlEq) => "<<=",
            TK::Punct(P::ShrEq) => ">>=",
            TK::Punct(P::AmpEq) => "&=",
            TK::Punct(P::PipeEq) => "|=",
            TK::Punct(P::CaretEq) => "^=",
            TK::Punct(P::AmpAmpEq) => "&&=",
            TK::Punct(P::PipePipeEq) => "||=",
            TK::Punct(P::QuestionQuestionEq) => "??=",
            _ => return None,
        })
    }

    fn parse_bare_arrow(&mut self, is_async: bool) -> PResult<Expr> {
        let name = self.expect_ident("a parameter name")?;
        self.expect_punct(P::Arrow)?;
        let params = vec![Param {
            rest: false,
            target: Pattern::Name(name),
            optional: false,
            ty: None,
            default: None,
        }];
        self.parse_arrow_body(is_async, params, None)
    }

    /// Parameter clause + optional return annotation + `=>`. Used
    /// speculatively for plain `(`, directly after `async`.
    fn parse_arrow_header(&mut self) -> PResult<(Vec<Param>, Option<Type>)> {
        let params = self.parse_param_clause()?;
        let ret = if self.eat_punct(P::Colon) { Some(self.parse_type()?) } else { None };
        self.expect_punct(P::Arrow)?;
        Ok((params, ret))
    }

    fn parse_arrow_body(
        &mut self,
        is_async: bool,
        params: Vec<Param>,
        ret: Option<Type>,
    ) -> PResult<Expr> {
        let body = if self.at_punct(P::LBrace) {
            ArrowBody::Block(self.parse_block()?)
        } else {
            ArrowBody::Expr(Box::new(self.parse_assignment()?))
        };
        Ok(Expr::Arrow { is_async, params, ret, body })
    }

    fn parse_conditional(&mut self) -> PResult<Expr> {
        let cond = self.parse_coalesce()?;
        if self.eat_punct(P::Question) {
            let then = self.parse_assignment()?;
            self.expect_punct(P::Colon)?;
            let els = self.parse_assignment()?;
            return Ok(Expr::Cond {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        Ok(cond)
    }

    fn parse_coalesce(&mut self) -> PResult<Expr> {
        let mut e = self.parse_logical_or()?;
        while self.at_punct(P::QuestionQuestion) {
            let bad = |x: &Expr| matches!(x, Expr::Binary { op: BinOp::Or | BinOp::And, .. });
            if bad(&e) {
                self.report(
                    Code::MixedCoalesce,
                    "`??` cannot be mixed with `&&`/`||` without parentheses (§6.4)",
                );
            }
            self.advance();
            let r = self.parse_logical_or()?;
            if bad(&r) {
                self.report(
                    Code::MixedCoalesce,
                    "`??` cannot be mixed with `&&`/`||` without parentheses (§6.4)",
                );
            }
            e = Expr::Binary { op: BinOp::Coalesce, l: Box::new(e), r: Box::new(r) };
        }
        Ok(e)
    }

    left_chain!(parse_logical_or, parse_logical_and, PipePipe => Or);
    left_chain!(parse_logical_and, parse_bitor, AmpAmp => And);
    left_chain!(parse_bitor, parse_bitxor, Pipe => BitOr);
    left_chain!(parse_bitxor, parse_bitand, Caret => BitXor);
    left_chain!(parse_bitand, parse_equality, Amp => BitAnd);
    left_chain!(parse_equality, parse_relational, EqEq => Eq, NotEq => Ne);
    left_chain!(parse_shift, parse_additive, Shl => Shl, Shr => Shr);
    left_chain!(parse_additive, parse_multiplicative, Plus => Add, Minus => Sub);
    left_chain!(parse_multiplicative, parse_exponent, Star => Mul, Slash => Div, Percent => Rem);

    fn parse_relational(&mut self) -> PResult<Expr> {
        let mut e = self.parse_shift()?;
        loop {
            let op = match self.kind() {
                TK::Punct(P::Lt) => BinOp::Lt,
                TK::Punct(P::Gt) => BinOp::Gt,
                TK::Punct(P::LtEq) => BinOp::Le,
                TK::Punct(P::GtEq) => BinOp::Ge,
                TK::Keyword(Kw::Instanceof) => BinOp::Instanceof,
                _ => break,
            };
            self.advance();
            let r = self.parse_shift()?;
            e = Expr::Binary { op, l: Box::new(e), r: Box::new(r) };
        }
        Ok(e)
    }

    fn parse_exponent(&mut self) -> PResult<Expr> {
        let base = self.parse_cast()?;
        if self.at_punct(P::StarStar) {
            if matches!(&base, Expr::Unary { .. } | Expr::Update { prefix: true, .. }) {
                self.report(
                    Code::AmbiguousExponent,
                    "unary operand of `**` must be parenthesized (§6.4)",
                );
            }
            self.advance();
            let r = self.parse_exponent()?; // right-associative
            return Ok(Expr::Binary { op: BinOp::Pow, l: Box::new(base), r: Box::new(r) });
        }
        Ok(base)
    }

    fn parse_cast(&mut self) -> PResult<Expr> {
        let mut e = self.parse_unary()?;
        while self.eat_kw(Kw::As) {
            let wrapping = self.eat_kw(Kw::Wrapping);
            let ty = self.parse_type()?;
            e = Expr::Cast { expr: Box::new(e), wrapping, ty };
        }
        Ok(e)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let op = match self.kind() {
            TK::Punct(P::Plus) => Some(UnaryOp::Plus),
            TK::Punct(P::Minus) => Some(UnaryOp::Neg),
            TK::Punct(P::Tilde) => Some(UnaryOp::BitNot),
            TK::Punct(P::Bang) => Some(UnaryOp::Not),
            TK::Keyword(Kw::Await) => Some(UnaryOp::Await),
            _ => None,
        };
        if let Some(op) = op {
            let pos = self.pos();
            self.advance();
            let e = self.parse_unary()?;
            return Ok(Expr::Unary { op, pos, expr: Box::new(e) });
        }
        if matches!(self.kind(), TK::Punct(P::PlusPlus | P::MinusMinus)) {
            let inc = self.at_punct(P::PlusPlus);
            self.advance();
            let e = self.parse_unary()?;
            return Ok(Expr::Update { prefix: true, inc, expr: Box::new(e) });
        }
        let e = self.parse_lhs()?;
        if matches!(self.kind(), TK::Punct(P::PlusPlus | P::MinusMinus)) {
            let inc = self.at_punct(P::PlusPlus);
            self.advance();
            return Ok(Expr::Update { prefix: false, inc, expr: Box::new(e) });
        }
        Ok(e)
    }

    fn parse_lhs(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.kind() {
                TK::Punct(P::Dot) => {
                    self.advance();
                    let name = self.expect_member_name()?;
                    e = Expr::Member { obj: Box::new(e), name, optional: false };
                }
                TK::Punct(P::QuestionDot) => {
                    self.advance();
                    match self.kind() {
                        TK::Punct(P::LBracket) => {
                            self.advance();
                            let index = self.parse_expr()?;
                            self.expect_punct(P::RBracket)?;
                            e = Expr::Index {
                                obj: Box::new(e),
                                index: Box::new(index),
                                optional: true,
                            };
                        }
                        TK::Punct(P::LParen) => {
                            let args = self.parse_args()?;
                            e = Expr::Call {
                                callee: Box::new(e),
                                type_args: Vec::new(),
                                args,
                                optional: true,
                            };
                        }
                        _ => {
                            let name = self.expect_member_name()?;
                            e = Expr::Member { obj: Box::new(e), name, optional: true };
                        }
                    }
                }
                TK::Punct(P::LBracket) => {
                    self.advance();
                    let index = self.parse_expr()?;
                    self.expect_punct(P::RBracket)?;
                    e = Expr::Index { obj: Box::new(e), index: Box::new(index), optional: false };
                }
                TK::Punct(P::LParen) => {
                    let args = self.parse_args()?;
                    e = Expr::Call {
                        callee: Box::new(e),
                        type_args: Vec::new(),
                        args,
                        optional: false,
                    };
                }
                TK::Punct(P::Lt) if self.looks_like_type_args() => {
                    self.advance();
                    let mut type_args = vec![self.parse_type()?];
                    while self.eat_punct(P::Comma) {
                        type_args.push(self.parse_type()?);
                    }
                    self.expect_gt()?;
                    let args = self.parse_args()?;
                    e = Expr::Call { callee: Box::new(e), type_args, args, optional: false };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// §6.9 note 3: `<` after an expression starts a type-argument list iff
    /// a balanced `>` is directly followed by `(`. Token-level scan, bounded.
    fn looks_like_type_args(&self) -> bool {
        let mut depth = 0i32;
        for i in 0..256 {
            match self.kind_at(i) {
                TK::Punct(P::Lt) => depth += 1,
                TK::Punct(P::Gt) => depth -= 1,
                TK::Punct(P::GtEq) => depth -= 1,
                TK::Punct(P::Shr) => depth -= 2,
                TK::Punct(P::ShrEq) => depth -= 2,
                TK::Ident
                | TK::Keyword(
                    Kw::Bool
                    | Kw::CharTy
                    | Kw::StringTy
                    | Kw::BigIntTy
                    | Kw::BigDecTy
                    | Kw::Int
                    | Kw::Int8
                    | Kw::Int16
                    | Kw::Int32
                    | Kw::Int64
                    | Kw::Uint
                    | Kw::Uint8
                    | Kw::Uint16
                    | Kw::Uint32
                    | Kw::Uint64
                    | Kw::Float
                    | Kw::Float32
                    | Kw::Float64
                    | Kw::Void
                    | Kw::Readonly,
                )
                | TK::Punct(
                    P::Comma
                    | P::Dot
                    | P::Question
                    | P::LBracket
                    | P::RBracket
                    | P::LParen
                    | P::RParen
                    | P::LBrace
                    | P::RBrace
                    | P::Arrow
                    | P::Pipe
                    | P::Colon
                    | P::DotDotDot,
                ) => {}
                _ => return false,
            }
            if depth <= 0 {
                return self.kind_at(i + 1) == TK::Punct(P::LParen);
            }
        }
        false
    }

    fn parse_args(&mut self) -> PResult<Vec<ArrayElem>> {
        self.expect_punct(P::LParen)?;
        let mut args = Vec::new();
        while !self.at_punct(P::RParen) {
            let spread = self.eat_punct(P::DotDotDot);
            let expr = self.parse_assignment()?;
            args.push(ArrayElem { spread, expr });
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        self.expect_punct(P::RParen)?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let lit = |k: LitKind, text: &str| Expr::Lit { kind: k, text: text.to_string() };
        match self.kind() {
            TK::Int { .. } => {
                let e = lit(LitKind::Int, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::Float { .. } => {
                let e = lit(LitKind::Float, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::BigInt => {
                let e = lit(LitKind::BigInt, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::BigDec => {
                let e = lit(LitKind::BigDec, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::Str => {
                let e = lit(LitKind::Str, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::Char => {
                let e = lit(LitKind::Char, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::Keyword(Kw::True | Kw::False) => {
                let e = lit(LitKind::Bool, self.text_of(0));
                self.advance();
                Ok(e)
            }
            TK::Keyword(Kw::Null) => {
                self.advance();
                Ok(Expr::Lit { kind: LitKind::Null, text: "null".to_string() })
            }
            TK::Keyword(Kw::This) => {
                let pos = self.pos();
                self.advance();
                Ok(Expr::This(pos))
            }
            TK::Keyword(Kw::Super) => {
                let pos = self.pos();
                self.advance();
                if self.eat_punct(P::Dot) {
                    let name = self.expect_member_name()?;
                    Ok(Expr::SuperMember { name, pos })
                } else if self.at_punct(P::LParen) {
                    let args = self.parse_args()?;
                    Ok(Expr::SuperCall { args, pos })
                } else {
                    self.expected("`.` or `(` after `super`")
                }
            }
            TK::Keyword(Kw::New) => {
                self.advance();
                let ty = self.parse_type_reference()?;
                let args = self.parse_args()?;
                Ok(Expr::New { ty, args })
            }
            TK::Keyword(Kw::Import) => {
                self.advance();
                self.expect_punct(P::LParen)?;
                let e = self.parse_assignment()?;
                self.expect_punct(P::RParen)?;
                Ok(Expr::ImportCall(Box::new(e)))
            }
            TK::Ident => {
                let e = Expr::Ident(Name { text: self.text_of(0).to_string(), pos: self.pos() });
                self.advance();
                Ok(e)
            }
            TK::TemplateNoSub | TK::TemplateHead => self.parse_template(),
            TK::Punct(P::LParen) => {
                self.advance();
                let e = self.parse_assignment()?;
                self.expect_punct(P::RParen)?;
                Ok(Expr::Paren(Box::new(e)))
            }
            TK::Punct(P::LBracket) => {
                self.advance();
                let mut elems = Vec::new();
                while !self.at_punct(P::RBracket) {
                    let spread = self.eat_punct(P::DotDotDot);
                    let expr = self.parse_assignment()?;
                    elems.push(ArrayElem { spread, expr });
                    if !self.eat_punct(P::Comma) {
                        break;
                    }
                }
                self.expect_punct(P::RBracket)?;
                Ok(Expr::Array(elems))
            }
            TK::Punct(P::LBrace) => {
                self.advance();
                let mut fields = Vec::new();
                while !self.at_punct(P::RBrace) {
                    if self.eat_punct(P::DotDotDot) {
                        fields.push(RecordField::Spread(self.parse_assignment()?));
                    } else {
                        let pos = self.pos();
                        let was_kw = matches!(self.kind(), TK::Keyword(_));
                        let name = Name { text: self.expect_member_name()?, pos };
                        let value = if self.eat_punct(P::Colon) {
                            Some(self.parse_assignment()?)
                        } else {
                            if was_kw {
                                let n = &name.text;
                                self.report(
                                    Code::ReservedBinding,
                                    format!(
                                        "shorthand `{{{n}}}` would reference the reserved \
                                         word `{n}`; write `{n}: value`"
                                    ),
                                );
                            }
                            None
                        };
                        fields.push(RecordField::Named { name, value });
                    }
                    if !self.eat_punct(P::Comma) {
                        break;
                    }
                }
                self.expect_punct(P::RBrace)?;
                Ok(Expr::Record(fields))
            }
            _ => self.expected("an expression"),
        }
    }

    fn strip_template(&self, kind: TK) -> String {
        let raw = self.text_of(0);
        let (a, b) = match kind {
            TK::TemplateNoSub | TK::TemplateTail => (1, 1), // `…`  or  }…`
            _ => (1, 2),                                    // `…${ or }…${
        };
        raw[a..raw.len() - b].to_string()
    }

    fn parse_template(&mut self) -> PResult<Expr> {
        let mut parts = Vec::new();
        if self.kind() == TK::TemplateNoSub {
            parts.push(TplPart::Text(self.strip_template(TK::TemplateNoSub)));
            self.advance();
            return Ok(Expr::Template(parts));
        }
        parts.push(TplPart::Text(self.strip_template(TK::TemplateHead)));
        self.advance();
        loop {
            parts.push(TplPart::Expr(self.parse_assignment()?));
            match self.kind() {
                TK::TemplateMiddle => {
                    parts.push(TplPart::Text(self.strip_template(TK::TemplateMiddle)));
                    self.advance();
                }
                TK::TemplateTail => {
                    parts.push(TplPart::Text(self.strip_template(TK::TemplateTail)));
                    self.advance();
                    return Ok(Expr::Template(parts));
                }
                _ => return self.expected("`}` continuing the template literal"),
            }
        }
    }

    // ---- types -------------------------------------------------------------

    fn parse_type(&mut self) -> PResult<Type> {
        let first = self.parse_postfix_type()?;
        if !self.at_punct(P::Pipe) {
            return Ok(first);
        }
        let mut arms = vec![first];
        while self.eat_punct(P::Pipe) {
            arms.push(self.parse_postfix_type()?);
        }
        Ok(Type::Union(arms))
    }

    fn parse_postfix_type(&mut self) -> PResult<Type> {
        let mut t = self.parse_primary_type()?;
        loop {
            if self.eat_punct(P::Question) {
                t = Type::Nullable(Box::new(t));
            } else if self.at_punct(P::LBracket) && self.kind_at(1) == TK::Punct(P::RBracket) {
                self.advance();
                self.advance();
                t = Type::ArrayOf(Box::new(t));
            } else {
                break;
            }
        }
        Ok(t)
    }

    fn is_predefined_type_kw(k: Kw) -> bool {
        matches!(
            k,
            Kw::Bool
                | Kw::CharTy
                | Kw::StringTy
                | Kw::BigIntTy
                | Kw::BigDecTy
                | Kw::Int
                | Kw::Int8
                | Kw::Int16
                | Kw::Int32
                | Kw::Int64
                | Kw::Uint
                | Kw::Uint8
                | Kw::Uint16
                | Kw::Uint32
                | Kw::Uint64
                | Kw::Float
                | Kw::Float32
                | Kw::Float64
                | Kw::Void
        )
    }

    fn parse_primary_type(&mut self) -> PResult<Type> {
        match self.kind() {
            TK::Keyword(k) if Self::is_predefined_type_kw(k) => {
                let pos = self.pos();
                self.advance();
                Ok(Type::Named { name: k.as_str().to_string(), pos, args: Vec::new() })
            }
            TK::Ident => self.parse_type_reference(),
            TK::Punct(P::LBracket) => {
                self.advance();
                let mut types = vec![self.parse_type()?];
                while self.eat_punct(P::Comma) {
                    types.push(self.parse_type()?);
                }
                self.expect_punct(P::RBracket)?;
                if types.len() < 2 {
                    self.report(Code::UnexpectedToken, "1-tuples don't exist (§6.3)");
                }
                Ok(Type::Tuple(types))
            }
            TK::Punct(P::LBrace) => {
                self.advance();
                let mut members = Vec::new();
                while !self.at_punct(P::RBrace) && self.kind() != TK::Eof {
                    let readonly = self.at_kw(Kw::Readonly)
                        && !matches!(self.kind_at(1), TK::Punct(P::Colon | P::Question));
                    if readonly {
                        self.advance();
                    }
                    let name = self.expect_member_name()?;
                    let optional = self.eat_punct(P::Question);
                    self.expect_punct(P::Colon)?;
                    let ty = self.parse_type()?;
                    members.push(RecordTypeMember { readonly, name, optional, ty });
                    if !self.eat_punct(P::Comma)
                        && !self.eat_punct(P::Semi)
                        && !self.at_punct(P::RBrace)
                    {
                        return self.expected("`,` or `;` after a record type member");
                    }
                }
                self.expect_punct(P::RBrace)?;
                Ok(Type::Record(members))
            }
            TK::Punct(P::Lt) => self.parse_fn_type(),
            TK::Punct(P::LParen) => {
                if let Some(t) = self.try_parse(|p| p.parse_fn_type()) {
                    return Ok(t);
                }
                self.advance();
                let t = self.parse_type()?;
                self.expect_punct(P::RParen)?;
                Ok(t)
            }
            _ => self.expected("a type"),
        }
    }

    fn parse_fn_type(&mut self) -> PResult<Type> {
        let type_params = self.parse_type_params_opt()?;
        self.expect_punct(P::LParen)?;
        let mut params = Vec::new();
        while !self.at_punct(P::RParen) {
            let rest = self.eat_punct(P::DotDotDot);
            let name = self.expect_ident("a parameter name")?;
            let optional = self.eat_punct(P::Question);
            self.expect_punct(P::Colon)?;
            let ty = self.parse_type()?;
            params.push(FnTypeParam { rest, name: name.text, optional, ty });
            if !self.eat_punct(P::Comma) {
                break;
            }
        }
        self.expect_punct(P::RParen)?;
        self.expect_punct(P::Arrow)?;
        let ret = self.parse_type()?;
        Ok(Type::Function { type_params, params, ret: Box::new(ret) })
    }

    /// TypeReference ::= QualifiedName TypeArguments? (§6.3). Also the type
    /// position of `new` and `extends`/`implements`.
    fn parse_type_reference(&mut self) -> PResult<Type> {
        let first = self.expect_ident("a type name")?;
        let pos = first.pos;
        let mut name = first.text;
        while self.at_punct(P::Dot) && self.kind_at(1) == TK::Ident {
            self.advance();
            name.push('.');
            name.push_str(self.text_of(0));
            self.advance();
        }
        let mut args = Vec::new();
        if self.eat_punct(P::Lt) {
            loop {
                args.push(self.parse_type()?);
                if !self.eat_punct(P::Comma) {
                    break;
                }
            }
            self.expect_gt()?;
        }
        Ok(Type::Named { name, pos, args })
    }
}
