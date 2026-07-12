//! Token definitions, spec §6.2.

use crate::diag::Pos;

/// Byte range into the source text plus its resolved position, so later
/// stages never recompute lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Integer literal suffixes, spec §2.6. `None` in the token means the
/// default type `int32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSuffix {
    U,  // uint32
    L,  // int64
    Ul, // uint64
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Keyword(Keyword),

    Int {
        suffix: Option<IntSuffix>,
    },
    Float {
        is_f32: bool,
    },
    BigInt,
    BigDec,
    Str,
    Char,

    /// `` `abc` `` — template with no substitutions
    TemplateNoSub,
    /// `` `abc${ ``
    TemplateHead,
    /// `}abc${`
    TemplateMiddle,
    /// `` }abc` ``
    TemplateTail,

    Punct(Punct),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Punct {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Semi,
    Comma,
    Dot,
    DotDotDot,
    QuestionDot,
    Colon,
    Question,
    Arrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    ShlEq,
    ShrEq,
    AmpEq,
    PipeEq,
    CaretEq,
    AmpAmpEq,
    PipePipeEq,
    QuestionQuestionEq,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Shl,
    Shr,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    AmpAmp,
    PipePipe,
    QuestionQuestion,
    PlusPlus,
    MinusMinus,
}

impl Punct {
    pub fn as_str(self) -> &'static str {
        use Punct::*;
        match self {
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            LBrace => "{",
            RBrace => "}",
            Semi => ";",
            Comma => ",",
            Dot => ".",
            DotDotDot => "...",
            QuestionDot => "?.",
            Colon => ":",
            Question => "?",
            Arrow => "=>",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            StarStar => "**",
            Eq => "=",
            PlusEq => "+=",
            MinusEq => "-=",
            StarEq => "*=",
            SlashEq => "/=",
            PercentEq => "%=",
            StarStarEq => "**=",
            ShlEq => "<<=",
            ShrEq => ">>=",
            AmpEq => "&=",
            PipeEq => "|=",
            CaretEq => "^=",
            AmpAmpEq => "&&=",
            PipePipeEq => "||=",
            QuestionQuestionEq => "??=",
            EqEq => "==",
            NotEq => "!=",
            Lt => "<",
            Gt => ">",
            LtEq => "<=",
            GtEq => ">=",
            Shl => "<<",
            Shr => ">>",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Tilde => "~",
            Bang => "!",
            AmpAmp => "&&",
            PipePipe => "||",
            QuestionQuestion => "??",
            PlusPlus => "++",
            MinusMinus => "--",
        }
    }
}

macro_rules! keywords {
    ( $( $variant:ident => $text:literal ),+ $(,)? ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Keyword {
            $( $variant, )+
        }

        impl Keyword {
            pub fn from_str(s: &str) -> Option<Keyword> {
                match s {
                    $( $text => Some(Keyword::$variant), )+
                    _ => None,
                }
            }

            pub fn as_str(self) -> &'static str {
                match self {
                    $( Keyword::$variant => $text, )+
                }
            }
        }
    };
}

// Spec §2.5 / grammar §6.2: the reserved words, including predefined type
// names. `in`, `typeof`, `yield` are reserved but unused in 0.1.
keywords! {
    Abstract => "abstract", As => "as", Async => "async", Await => "await",
    Break => "break", Case => "case", Catch => "catch", Class => "class",
    Const => "const", Continue => "continue", Default => "default",
    Do => "do", Else => "else", Enum => "enum", Export => "export",
    Extends => "extends", Extern => "extern", False => "false",
    Final => "final", Finally => "finally", For => "for", From => "from",
    Function => "function", Get => "get", If => "if",
    Implements => "implements", Import => "import", In => "in",
    Instanceof => "instanceof", Interface => "interface", Let => "let",
    New => "new", Null => "null", Of => "of", Override => "override",
    Private => "private", Protected => "protected", Public => "public",
    Readonly => "readonly",
    Return => "return", Set => "set", Static => "static", Super => "super",
    Switch => "switch", This => "this", Throw => "throw", True => "true",
    Try => "try", TypeExpr => "type", Typeof => "typeof", Void => "void",
    While => "while", Wrapping => "wrapping", Yield => "yield",
    // Predefined type names (spec §3.1)
    Bool => "bool", CharTy => "char", StringTy => "string",
    BigIntTy => "bigint", BigDecTy => "bigdec",
    Int => "int", Int8 => "int8", Int16 => "int16", Int32 => "int32",
    Int64 => "int64",
    Uint => "uint", Uint8 => "uint8", Uint16 => "uint16",
    Uint32 => "uint32", Uint64 => "uint64",
    Float => "float", Float32 => "float32", Float64 => "float64",
}
