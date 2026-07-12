//! Lexer, grammar §6.2. Hand-written scanner over the decoded code-point
//! stream. Error recovery: every diagnostic is recorded and scanning
//! continues, so one bad token doesn't hide the rest of the file.
//!
//! Template literals are lexed with a brace-depth stack rather than parser
//! feedback: `TemplateHead` pushes a depth, `{`/`}` inside a substitution
//! adjust it, and a `}` at depth zero resumes template scanning
//! (`TemplateMiddle`/`TemplateTail`).

use crate::diag::{Code, Diagnostic, Pos};
use crate::source::SourceFile;
use crate::token::{IntSuffix, Keyword, Punct, Span, Token, TokenKind};

pub struct LexOutput {
    pub tokens: Vec<Token>,
    /// Comment spans in source order (the formatter re-emits these).
    pub comments: Vec<Span>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(source: &SourceFile) -> LexOutput {
    Lexer::new(&source.text).run()
}

struct Lexer<'s> {
    text: &'s str,
    /// Byte offset of the next unconsumed character.
    idx: usize,
    line: u32,
    col: u32,
    /// Start of the token currently being scanned.
    start: usize,
    start_pos: Pos,
    /// Brace depth per open template substitution, innermost last.
    template_stack: Vec<u32>,
    tokens: Vec<Token>,
    comments: Vec<Span>,
    diagnostics: Vec<Diagnostic>,
}

impl<'s> Lexer<'s> {
    fn new(text: &'s str) -> Self {
        Lexer {
            text,
            idx: 0,
            line: 1,
            col: 1,
            start: 0,
            start_pos: Pos { line: 1, col: 1 },
            template_stack: Vec::new(),
            tokens: Vec::new(),
            comments: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> LexOutput {
        loop {
            self.skip_trivia();
            self.start = self.idx;
            self.start_pos = self.pos();
            let Some(c) = self.peek() else {
                self.emit(TokenKind::Eof);
                break;
            };
            self.scan_token(c);
        }
        LexOutput {
            tokens: self.tokens,
            comments: self.comments,
            diagnostics: self.diagnostics,
        }
    }

    // ---- cursor ----------------------------------------------------------

    fn rest(&self) -> &'s str {
        &self.text[self.idx..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut it = self.rest().chars();
        it.next();
        it.next()
    }

    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            col: self.col,
        }
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.idx += c.len_utf8();
        match c {
            '\r' => {
                // CRLF counts as one terminator (spec §2.2).
                if self.peek() == Some('\n') {
                    self.idx += 1;
                }
                self.line += 1;
                self.col = 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => {
                self.line += 1;
                self.col = 1;
            }
            _ => self.col += 1,
        }
        Some(c)
    }

    fn eat(&mut self, want: char) -> bool {
        if self.peek() == Some(want) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, want: &str) -> bool {
        if self.rest().starts_with(want) {
            for _ in want.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    // ---- output ----------------------------------------------------------

    fn emit(&mut self, kind: TokenKind) {
        self.tokens.push(Token {
            kind,
            span: Span {
                start: self.start,
                end: self.idx,
                pos: self.start_pos,
            },
        });
    }

    fn error_at(&mut self, code: Code, message: impl Into<String>, pos: Pos) {
        self.diagnostics.push(Diagnostic::error(code, message, pos));
    }

    fn error(&mut self, code: Code, message: impl Into<String>) {
        let pos = self.start_pos;
        self.error_at(code, message, pos);
    }

    // ---- trivia ----------------------------------------------------------

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if is_whitespace(c) => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    let start = self.idx;
                    let pos = self.pos();
                    while let Some(c) = self.peek() {
                        if is_line_terminator(c) {
                            break;
                        }
                        self.bump();
                    }
                    self.comments.push(Span {
                        start,
                        end: self.idx,
                        pos,
                    });
                }
                Some('/') if self.peek2() == Some('*') => {
                    let start = self.idx;
                    let open = self.pos();
                    self.bump();
                    self.bump();
                    loop {
                        if self.rest().starts_with("*/") {
                            self.bump();
                            self.bump();
                            break;
                        }
                        if self.bump().is_none() {
                            self.error_at(
                                Code::UnterminatedBlockComment,
                                "unterminated block comment",
                                open,
                            );
                            break;
                        }
                    }
                    self.comments.push(Span {
                        start,
                        end: self.idx,
                        pos: open,
                    });
                }
                _ => break,
            }
        }
    }

    // ---- dispatch --------------------------------------------------------

    fn scan_token(&mut self, c: char) {
        match c {
            // Char literal `c'…'` — must be checked before identifiers.
            'c' if self.peek2() == Some('\'') => self.scan_char_literal(),
            _ if is_id_start(c) => self.scan_ident(),
            '0'..='9' => self.scan_number(),
            '"' | '\'' => self.scan_string(c),
            '`' => self.scan_template_part(TemplateOpen::Backtick),
            '}' if self.template_stack.last() == Some(&0) => {
                self.template_stack.pop();
                self.scan_template_part(TemplateOpen::Brace);
            }
            _ => self.scan_punct(c),
        }
    }

    // ---- identifiers -----------------------------------------------------

    fn scan_ident(&mut self) {
        self.bump();
        while let Some(c) = self.peek() {
            if is_id_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let text = &self.text[self.start..self.idx];
        match Keyword::from_str(text) {
            Some(kw) => self.emit(TokenKind::Keyword(kw)),
            None => self.emit(TokenKind::Ident),
        }
    }

    // ---- numbers ---------------------------------------------------------

    fn scan_number(&mut self) {
        let radix = if self.eat_str("0x") {
            16
        } else if self.eat_str("0o") {
            8
        } else if self.eat_str("0b") {
            2
        } else {
            10
        };

        if radix != 10 {
            self.scan_digits(radix);
            self.finish_number(NumForm::Int { radix });
            return;
        }

        self.scan_digits(10);
        let int_part = &self.text[self.start..self.idx];
        if int_part.len() > 1 && int_part.starts_with('0') {
            self.error(
                Code::BadDigitSeparator,
                "leading zeros are not allowed; write octal as `0o…` (§2.6)",
            );
        }
        let mut form = NumForm::Int { radix: 10 };

        // Fractional part: `.` must be followed by a digit — `1.foo()` and
        // `1.` are errors (grammar §6.2 note).
        if self.peek() == Some('.') {
            match self.peek2() {
                Some('0'..='9') => {
                    self.bump();
                    self.scan_digits(10);
                    form = NumForm::Float;
                }
                _ => {
                    self.bump();
                    self.error(
                        Code::ExpectedDigitAfterDot,
                        "expected a digit after the decimal point; write `1.0`, not `1.`",
                    );
                    self.emit(TokenKind::Float { is_f32: false });
                    return;
                }
            }
        }

        if matches!(self.peek(), Some('e' | 'E'))
            && matches!(
                (self.peek2(), self.char_at(2)),
                (Some('0'..='9'), _) | (Some('+' | '-'), Some('0'..='9'))
            )
        {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            self.scan_digits(10);
            // An exponent alone makes it a float unless the `m` suffix
            // turns it into a bigdec (`1e3m`).
            if form == (NumForm::Int { radix: 10 }) {
                form = NumForm::FloatExpOnly;
            }
        }

        self.finish_number(form);
    }

    /// Consume digits and `_` separators; a separator must sit between two
    /// digits of the same literal part.
    fn scan_digits(&mut self, radix: u32) {
        let mut last_sep_pos: Option<Pos> = None;
        let mut saw_digit = false;
        loop {
            match self.peek() {
                Some('_') => {
                    let pos = self.pos();
                    if !saw_digit || last_sep_pos.is_some() {
                        self.error_at(Code::BadDigitSeparator, "`_` must separate two digits", pos);
                    }
                    last_sep_pos = Some(pos);
                    self.bump();
                }
                Some(c) if c.is_digit(radix) => {
                    saw_digit = true;
                    last_sep_pos = None;
                    self.bump();
                }
                _ => break,
            }
        }
        if let Some(pos) = last_sep_pos {
            self.error_at(Code::BadDigitSeparator, "`_` must separate two digits", pos);
        }
        if !saw_digit {
            self.error(Code::UnexpectedChar, "expected at least one digit");
        }
    }

    fn char_at(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    /// Scan the trailing suffix (lexed as part of the token, §6.9 note 6)
    /// and emit the numeric token.
    fn finish_number(&mut self, form: NumForm) {
        let suffix_start = self.idx;
        while let Some(c) = self.peek() {
            if is_id_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        let suffix = &self.text[suffix_start..self.idx];

        let kind = match (form, suffix) {
            (NumForm::Int { .. }, "") => TokenKind::Int { suffix: None },
            (NumForm::Int { .. }, "n") => TokenKind::BigInt,
            (NumForm::Int { radix: 10 }, "m") => TokenKind::BigDec,
            (NumForm::Int { radix: 10 }, "f") => TokenKind::Float { is_f32: true },
            (NumForm::Int { .. }, s) if int_suffix(s).is_some() => TokenKind::Int {
                suffix: int_suffix(s),
            },
            (NumForm::Float | NumForm::FloatExpOnly, "") => TokenKind::Float { is_f32: false },
            (NumForm::Float | NumForm::FloatExpOnly, "f") => TokenKind::Float { is_f32: true },
            (NumForm::Float | NumForm::FloatExpOnly, "m") => TokenKind::BigDec,
            (_, s) => {
                let what = if s.is_empty() {
                    "here".to_string()
                } else {
                    format!("`{s}`")
                };
                self.error(
                    Code::InvalidNumericSuffix,
                    format!("invalid suffix {what} on this numeric literal"),
                );
                TokenKind::Int { suffix: None }
            }
        };
        self.emit(kind);
    }

    // ---- strings, chars, templates ----------------------------------------

    fn scan_string(&mut self, quote: char) {
        self.bump();
        loop {
            match self.peek() {
                None => {
                    self.error(Code::UnterminatedString, "unterminated string literal");
                    break;
                }
                Some(c) if is_line_terminator(c) => {
                    self.error(
                        Code::UnterminatedString,
                        "string literal must close before the end of the line",
                    );
                    break;
                }
                Some(c) if c == quote => {
                    self.bump();
                    break;
                }
                Some('\\') => self.scan_escape(),
                Some(_) => {
                    self.bump();
                }
            }
        }
        self.emit(TokenKind::Str);
    }

    fn scan_char_literal(&mut self) {
        self.bump(); // c
        self.bump(); // '
        match self.peek() {
            None | Some('\'') => {
                self.error(Code::InvalidCharLiteral, "empty character literal");
                self.eat('\'');
                self.emit(TokenKind::Char);
                return;
            }
            Some(c) if is_line_terminator(c) => {
                self.error(Code::InvalidCharLiteral, "unterminated character literal");
                self.emit(TokenKind::Char);
                return;
            }
            Some('\\') => self.scan_escape(),
            Some(_) => {
                self.bump();
            }
        }
        if !self.eat('\'') {
            self.error(
                Code::InvalidCharLiteral,
                "character literal must contain exactly one code point",
            );
            // Recover: skip to closing quote on this line if there is one.
            while let Some(c) = self.peek() {
                if is_line_terminator(c) {
                    break;
                }
                self.bump();
                if c == '\'' {
                    break;
                }
            }
        }
        self.emit(TokenKind::Char);
    }

    fn scan_template_part(&mut self, open: TemplateOpen) {
        self.bump(); // ` or }
        loop {
            match self.peek() {
                None => {
                    self.error(Code::UnterminatedTemplate, "unterminated template literal");
                    self.emit(match open {
                        TemplateOpen::Backtick => TokenKind::TemplateNoSub,
                        TemplateOpen::Brace => TokenKind::TemplateTail,
                    });
                    return;
                }
                Some('`') => {
                    self.bump();
                    self.emit(match open {
                        TemplateOpen::Backtick => TokenKind::TemplateNoSub,
                        TemplateOpen::Brace => TokenKind::TemplateTail,
                    });
                    return;
                }
                Some('$') if self.peek2() == Some('{') => {
                    self.bump();
                    self.bump();
                    self.template_stack.push(0);
                    self.emit(match open {
                        TemplateOpen::Backtick => TokenKind::TemplateHead,
                        TemplateOpen::Brace => TokenKind::TemplateMiddle,
                    });
                    return;
                }
                Some('\\') => self.scan_escape(),
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn scan_escape(&mut self) {
        let pos = self.pos();
        self.bump(); // backslash
        match self.peek() {
            Some('n' | 'r' | 't' | '0' | '\\' | '\'' | '"' | '`') => {
                self.bump();
            }
            Some('u') if self.peek2() == Some('{') => {
                self.bump();
                self.bump();
                let mut value: u32 = 0;
                let mut digits = 0;
                let mut overflow = false;
                while let Some(c) = self.peek() {
                    if let Some(d) = c.to_digit(16) {
                        value = match value.checked_mul(16).and_then(|v| v.checked_add(d)) {
                            Some(v) => v,
                            None => {
                                overflow = true;
                                u32::MAX
                            }
                        };
                        digits += 1;
                        self.bump();
                    } else {
                        break;
                    }
                }
                if !self.eat('}') {
                    self.error_at(Code::InvalidEscape, "unterminated `\\u{…}` escape", pos);
                    return;
                }
                if digits == 0 {
                    self.error_at(
                        Code::InvalidEscape,
                        "`\\u{}` needs at least one hex digit",
                        pos,
                    );
                } else if overflow || char::from_u32(value).is_none() {
                    // Rejects > U+10FFFF and surrogates (spec §2.1 validity).
                    self.error_at(
                        Code::InvalidEscape,
                        "escape does not name a Unicode scalar value",
                        pos,
                    );
                }
            }
            Some('u') => {
                self.bump();
                self.error_at(
                    Code::InvalidEscape,
                    "write code points as `\\u{…}`; there are no UTF-16 `\\uXXXX` escapes (§2.6)",
                    pos,
                );
            }
            Some(c) => {
                self.bump();
                self.error_at(Code::InvalidEscape, format!("unknown escape `\\{c}`"), pos);
            }
            None => self.error_at(Code::InvalidEscape, "trailing backslash", pos),
        }
    }

    // ---- punctuators -------------------------------------------------------

    fn scan_punct(&mut self, c: char) {
        use Punct::*;
        // JS-migration: `===`/`!==` don't exist; `==` is already strict.
        if self.rest().starts_with("===") {
            self.error(
                Code::UnexpectedChar,
                "there is no `===`; `==` is already strict (§3.5)",
            );
            self.eat_str("===");
            self.emit(TokenKind::Punct(EqEq));
            return;
        }
        if self.rest().starts_with("!==") {
            self.error(
                Code::UnexpectedChar,
                "there is no `!==`; `!=` is already strict (§3.5)",
            );
            self.eat_str("!==");
            self.emit(TokenKind::Punct(NotEq));
            return;
        }
        // Longest match first within each leading character.
        let table: &[(&str, Punct)] = &[
            ("...", DotDotDot),
            ("**=", StarStarEq),
            ("<<=", ShlEq),
            (">>=", ShrEq),
            ("&&=", AmpAmpEq),
            ("||=", PipePipeEq),
            ("??=", QuestionQuestionEq),
            ("=>", Arrow),
            ("==", EqEq),
            ("!=", NotEq),
            ("<=", LtEq),
            (">=", GtEq),
            ("<<", Shl),
            (">>", Shr),
            ("&&", AmpAmp),
            ("||", PipePipe),
            ("??", QuestionQuestion),
            ("?.", QuestionDot),
            ("++", PlusPlus),
            ("--", MinusMinus),
            ("**", StarStar),
            ("+=", PlusEq),
            ("-=", MinusEq),
            ("*=", StarEq),
            ("/=", SlashEq),
            ("%=", PercentEq),
            ("&=", AmpEq),
            ("|=", PipeEq),
            ("^=", CaretEq),
            ("(", LParen),
            (")", RParen),
            ("[", LBracket),
            ("]", RBracket),
            ("{", LBrace),
            ("}", RBrace),
            (";", Semi),
            (",", Comma),
            (".", Dot),
            (":", Colon),
            ("?", Question),
            ("+", Plus),
            ("-", Minus),
            ("*", Star),
            ("/", Slash),
            ("%", Percent),
            ("=", Eq),
            ("<", Lt),
            (">", Gt),
            ("&", Amp),
            ("|", Pipe),
            ("^", Caret),
            ("~", Tilde),
            ("!", Bang),
        ];
        for &(text, punct) in table {
            if self.eat_str(text) {
                match punct {
                    LBrace => {
                        if let Some(depth) = self.template_stack.last_mut() {
                            *depth += 1;
                        }
                    }
                    RBrace => {
                        // A `}` at template depth 0 never reaches here
                        // (handled in scan_token as a template continuation).
                        if let Some(depth) = self.template_stack.last_mut() {
                            *depth -= 1;
                        }
                    }
                    _ => {}
                }
                self.emit(TokenKind::Punct(punct));
                return;
            }
        }
        self.bump();
        self.error(Code::UnexpectedChar, format!("unexpected character `{c}`"));
    }
}

#[derive(PartialEq, Clone, Copy)]
enum NumForm {
    Int {
        radix: u32,
    },
    Float,
    /// `1e3` — float form, but `m` suffix may still make it a bigdec.
    FloatExpOnly,
}

#[derive(Clone, Copy)]
enum TemplateOpen {
    Backtick,
    Brace,
}

fn int_suffix(s: &str) -> Option<IntSuffix> {
    Some(match s {
        "u" => IntSuffix::U,
        "l" => IntSuffix::L,
        "ul" => IntSuffix::Ul,
        "i8" => IntSuffix::I8,
        "i16" => IntSuffix::I16,
        "i32" => IntSuffix::I32,
        "i64" => IntSuffix::I64,
        "u8" => IntSuffix::U8,
        "u16" => IntSuffix::U16,
        "u32" => IntSuffix::U32,
        "u64" => IntSuffix::U64,
        _ => return None,
    })
}

fn is_id_start(c: char) -> bool {
    c == '_' || c == '$' || unicode_ident::is_xid_start(c)
}

fn is_id_continue(c: char) -> bool {
    c == '$' || unicode_ident::is_xid_continue(c)
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{000B}' | '\u{000C}' | '\u{00A0}' | '\u{FEFF}'
    ) || is_line_terminator(c)
        || matches!(
            c,
            '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
        )
}

/// Render tokens in the conformance-dump format: one `line:col kind "text"`
/// per line, then diagnostics. Golden `.expect` files contain exactly this.
pub fn dump(source: &SourceFile) -> String {
    use std::fmt::Write;
    let out = lex(source);
    let mut s = String::new();
    for t in &out.tokens {
        let text = &source.text[t.span.start..t.span.end];
        let _ = write!(s, "{}:{} ", t.span.pos.line, t.span.pos.col);
        match &t.kind {
            TokenKind::Ident => {
                let _ = writeln!(s, "ident {text}");
            }
            TokenKind::Keyword(kw) => {
                let _ = writeln!(s, "keyword {}", kw.as_str());
            }
            TokenKind::Int { suffix: None } => {
                let _ = writeln!(s, "int {text}");
            }
            TokenKind::Int { suffix: Some(sfx) } => {
                let _ = writeln!(s, "int {text} ({sfx:?})");
            }
            TokenKind::Float { is_f32 } => {
                let ty = if *is_f32 { "float32" } else { "float64" };
                let _ = writeln!(s, "float {text} ({ty})");
            }
            TokenKind::BigInt => {
                let _ = writeln!(s, "bigint {text}");
            }
            TokenKind::BigDec => {
                let _ = writeln!(s, "bigdec {text}");
            }
            TokenKind::Str => {
                let _ = writeln!(s, "string {text}");
            }
            TokenKind::Char => {
                let _ = writeln!(s, "char {text}");
            }
            TokenKind::TemplateNoSub
            | TokenKind::TemplateHead
            | TokenKind::TemplateMiddle
            | TokenKind::TemplateTail => {
                let kind = match t.kind {
                    TokenKind::TemplateNoSub => "template",
                    TokenKind::TemplateHead => "template-head",
                    TokenKind::TemplateMiddle => "template-middle",
                    _ => "template-tail",
                };
                let _ = writeln!(s, "{kind} {}", text.replace('\n', "\\n"));
            }
            TokenKind::Punct(p) => {
                let _ = writeln!(s, "punct {}", p.as_str());
            }
            TokenKind::Eof => {
                let _ = writeln!(s, "eof");
            }
        }
    }
    for d in &out.diagnostics {
        let _ = writeln!(s, "{d}");
    }
    s
}
