//! `mersey fmt`: token-stream reprinter.
//!
//! Canonicalizes indentation (brace depth), token spacing, blank runs
//! (at most one blank line), line endings (LF), and identifier NFC (§2.4),
//! while preserving the author's line-break decisions and comments.
//!
//! Safety invariant: the formatted text must lex to the exact same token
//! sequence as the input (kinds and lexemes, identifiers compared after
//! NFC). If it doesn't, formatting fails loudly rather than emit output
//! that could change meaning. Genuinely ambiguous spacing at the token
//! level (`<` comparison vs. generics, ternary `?`/`:` vs. type syntax,
//! unary vs. binary `+`/`-` after `++`) falls back to preserving the
//! source's whitespace.

use unicode_normalization::{is_nfc, UnicodeNormalization};

use crate::diag::Diagnostic;
use crate::lexer;
use crate::source::SourceFile;
use crate::token::{Keyword as Kw, Punct as P, Span, TokenKind as TK};

pub fn format(source: &SourceFile) -> Result<String, Vec<Diagnostic>> {
    let out = lexer::lex(source);
    if !out.diagnostics.is_empty() {
        return Err(out.diagnostics);
    }

    #[derive(Clone, Copy)]
    enum Kind {
        Tok(TK),
        Comment,
    }
    let mut pieces: Vec<(Span, Kind)> = out
        .tokens
        .iter()
        .filter(|t| t.kind != TK::Eof)
        .map(|t| (t.span, Kind::Tok(t.kind)))
        .chain(out.comments.iter().map(|s| (*s, Kind::Comment)))
        .collect();
    pieces.sort_by_key(|(s, _)| s.start);

    let text = &source.text;
    let piece_text = |span: &Span, kind: &Kind| -> String {
        let raw = &text[span.start..span.end];
        match kind {
            Kind::Tok(TK::Ident) if !is_nfc(raw) => raw.nfc().collect(),
            Kind::Comment => raw.trim_end().to_string(),
            _ => raw.to_string(),
        }
    };

    let mut buf = String::new();
    let mut depth: i32 = 0;
    let mut prev: Option<(TK, bool)> = None; // (kind, this +/-/~/! was unary)
    let mut prev_end_line: u32 = 1;
    let mut prev_end: usize = 0;

    for (span, kind) in &pieces {
        let txt = piece_text(span, kind);
        let gap = span.pos.line.saturating_sub(prev_end_line);
        let had_ws = span.start > prev_end;

        let tok = match kind {
            Kind::Tok(t) => Some(*t),
            Kind::Comment => None,
        };

        if tok == Some(TK::Punct(P::RBrace)) {
            depth -= 1;
        }

        if prev.is_none() {
            // first piece: no leading whitespace
        } else if gap >= 2 {
            buf.push_str("\n\n");
            push_indent(&mut buf, depth);
        } else if gap == 1 {
            buf.push('\n');
            push_indent(&mut buf, depth);
        } else if let Some((pk, p_unary)) = prev {
            let space = match kind {
                Kind::Comment => true,
                Kind::Tok(t) => need_space(pk, p_unary, *t, had_ws),
            };
            if space {
                buf.push(' ');
            }
        }

        buf.push_str(&txt);

        if tok == Some(TK::Punct(P::LBrace)) {
            depth += 1;
        }
        let is_unary = match tok {
            Some(TK::Punct(P::Plus | P::Minus)) => {
                !prev.map(|(k, _)| ends_expr(k)).unwrap_or(false)
            }
            Some(TK::Punct(P::Tilde | P::Bang)) => true,
            _ => false,
        };
        prev = Some((tok.unwrap_or(TK::Eof), is_unary)); // comments act like Eof: word-ish
        prev_end_line = span.pos.line + txt.matches('\n').count() as u32;
        prev_end = span.end;
    }
    buf.push('\n');

    // Safety invariant: identical token stream after formatting.
    let reformatted = SourceFile {
        name: source.name.clone(),
        text: buf.clone(),
    };
    let relex = lexer::lex(&reformatted);
    let orig_kinds: Vec<TK> = out.tokens.iter().map(|t| t.kind).collect();
    let new_kinds: Vec<TK> = relex.tokens.iter().map(|t| t.kind).collect();
    if !relex.diagnostics.is_empty() || orig_kinds != new_kinds {
        return Err(vec![Diagnostic::error(
            crate::diag::Code::UnexpectedChar,
            "internal: formatting would change the token stream; file left untouched \
             (please report this)",
            crate::diag::Pos { line: 1, col: 1 },
        )]);
    }
    Ok(buf)
}

fn push_indent(buf: &mut String, depth: i32) {
    for _ in 0..depth.max(0) {
        buf.push_str("    ");
    }
}

/// Token kinds that can end an expression (used to classify `+`/`-`/`++`).
fn ends_expr(k: TK) -> bool {
    matches!(
        k,
        TK::Ident
            | TK::Int { .. }
            | TK::Float { .. }
            | TK::BigInt
            | TK::BigDec
            | TK::Str
            | TK::Char
            | TK::TemplateNoSub
            | TK::TemplateTail
            | TK::Keyword(Kw::This | Kw::Super | Kw::True | Kw::False | Kw::Null)
            | TK::Punct(P::RParen | P::RBracket | P::RBrace | P::PlusPlus | P::MinusMinus)
    )
}

fn is_wordlike(k: TK) -> bool {
    matches!(
        k,
        TK::Ident
            | TK::Keyword(_)
            | TK::Int { .. }
            | TK::Float { .. }
            | TK::BigInt
            | TK::BigDec
            | TK::Str
            | TK::Char
            | TK::TemplateNoSub
            | TK::TemplateHead
            | TK::Eof // comments are treated as words
    )
}

fn is_binary_op(p: P) -> bool {
    use P::*;
    matches!(
        p,
        Star | Slash
            | Percent
            | StarStar
            | Eq
            | PlusEq
            | MinusEq
            | StarEq
            | SlashEq
            | PercentEq
            | StarStarEq
            | ShlEq
            | ShrEq
            | AmpEq
            | PipeEq
            | CaretEq
            | AmpAmpEq
            | PipePipeEq
            | QuestionQuestionEq
            | EqEq
            | NotEq
            | LtEq
            | GtEq
            | Shl
            | Amp
            | Pipe
            | Caret
            | AmpAmp
            | PipePipe
            | QuestionQuestion
            | Arrow
    )
}

fn need_space(prev: TK, prev_unary: bool, cur: TK, had_ws: bool) -> bool {
    use P::*;

    // -- template parts glue directly to their substitution expressions
    if matches!(prev, TK::TemplateHead | TK::TemplateMiddle)
        || matches!(cur, TK::TemplateMiddle | TK::TemplateTail)
    {
        return false;
    }

    // -- attach-left punctuation never takes a space before it
    if let TK::Punct(p) = cur {
        match p {
            Semi | Comma | RParen | RBracket | Dot | QuestionDot => return false,
            PlusPlus | MinusMinus if ends_expr(prev) => return false, // postfix
            Question => return had_ws,                                // `T?` vs `a ? b`
            Colon => return had_ws,                                   // `x: T` vs `a ? b : c`
            Lt | Gt | Shr => return had_ws,                           // generics vs comparison
            _ => {}
        }
    }

    // -- attach-right punctuation never takes a space after it
    if let TK::Punct(p) = prev {
        match p {
            LParen | LBracket | Dot | QuestionDot | Tilde | Bang | DotDotDot => return false,
            Plus | Minus if prev_unary => return false,
            PlusPlus | MinusMinus if is_wordlike(cur) => return false, // prefix
            Semi | Comma | Colon => return true,
            Question => return had_ws,
            Lt | Shr => return had_ws,
            Gt => return had_ws,            // generics vs comparison: preserve
            LBrace | RBrace => return true, // same-line `{ x }`, `} else`
            _ => {}
        }
    }

    // -- binary operators get spaces on both sides
    if let TK::Punct(p) = cur {
        if is_binary_op(p) {
            return true;
        }
        if matches!(p, Plus | Minus) {
            return true; // space before; unary-ness controls the other side
        }
        if p == LBrace {
            return true;
        }
        if p == DotDotDot {
            return true; // after `(`/`,` the prev rules already said no
        }
        if p == LParen || p == LBracket {
            // Call/index attach to the expression; control keywords and
            // operators keep a space.
            return match prev {
                TK::Keyword(
                    Kw::If
                    | Kw::While
                    | Kw::For
                    | Kw::Switch
                    | Kw::Catch
                    | Kw::Return
                    | Kw::Throw
                    | Kw::Case
                    | Kw::Do
                    | Kw::Else
                    | Kw::In
                    | Kw::Of
                    | Kw::Extends
                    | Kw::Implements
                    | Kw::As
                    | Kw::Instanceof
                    | Kw::New
                    | Kw::Await
                    | Kw::Typeof
                    | Kw::Let
                    | Kw::Const
                    | Kw::Export
                    | Kw::Import
                    | Kw::From,
                ) => true,
                k if ends_expr(k) => false,
                TK::Keyword(_) => false, // predefined type names, get/set…
                _ => true,
            };
        }
    }
    if let TK::Punct(p) = prev {
        if is_binary_op(p) || matches!(p, Plus | Minus) {
            return true;
        }
    }

    // -- words next to words
    if is_wordlike(prev) && is_wordlike(cur) {
        return true;
    }
    if is_wordlike(cur) {
        return true;
    }
    true
}
