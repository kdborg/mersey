//! Diagnostics. Positions are 1-based; columns count code points, not bytes
//! (spec §2.1), so they match what an editor shows.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
}

/// Stable diagnostic codes. The conformance suite matches on these, so a
/// code is never reused or renumbered once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// E0001: file is not UTF-8 (wrong encoding detected via BOM)
    WrongEncoding,
    /// E0002: invalid UTF-8 byte sequence
    InvalidUtf8,
    /// E0101: unexpected character
    UnexpectedChar,
    /// E0102: unterminated string literal
    UnterminatedString,
    /// E0103: invalid escape sequence
    InvalidEscape,
    /// E0104: invalid numeric literal suffix
    InvalidNumericSuffix,
    /// E0105: expected digit after decimal point
    ExpectedDigitAfterDot,
    /// E0106: unterminated template literal
    UnterminatedTemplate,
    /// E0107: invalid character literal
    InvalidCharLiteral,
    /// E0108: unterminated block comment
    UnterminatedBlockComment,
    /// E0109: digit separator misplaced
    BadDigitSeparator,
    /// E0110: integer literal out of range for its type (spec §2.6)
    IntOutOfRange,
    /// E0201: unexpected token (parser)
    UnexpectedToken,
    /// E0202: declaration in statement position (declarations are module-level, §6.7)
    MisplacedDeclaration,
    /// E0203: label not attached to a loop (§6.5)
    InvalidLabel,
    /// E0204: invalid assignment target
    InvalidAssignmentTarget,
    /// E0205: `??` mixed with `&&`/`||` without parentheses (§6.4)
    MixedCoalesce,
    /// E0206: unary operand of `**` must be parenthesized (§6.4)
    AmbiguousExponent,
    /// E0207: reserved word used as a binding name (§2.5)
    ReservedBinding,
    /// E0301: reference to an undefined name
    UndefinedName,
    /// E0302: duplicate declaration in the same scope
    DuplicateDeclaration,
    /// E0303: let/const used before its declaration (TDZ, §3.2)
    UseBeforeDeclaration,
    /// E0304: assignment to a `const` binding
    AssignToConst,
    /// E0305: `break`/`continue` names an unknown label
    UndefinedLabel,
    /// E0306: `await` outside an async function
    AwaitOutsideAsync,
    /// E0307: `this`/`super` used where it has no meaning
    InvalidThisSuper,
    /// E0308: reference to an undefined type name
    UnknownTypeName,
    /// E0309: `return` outside a function
    ReturnOutsideFunction,
    /// E0310: `break`/`continue` outside a loop (or `switch` for break)
    BreakOutsideLoop,
    /// E0401: type mismatch (assignment, argument, return, …)
    TypeMismatch,
    /// E0402: bad call (not callable, arity, type arguments)
    BadCall,
    /// E0403: unknown member
    UnknownMember,
    /// E0404: access control violation (§4.2)
    AccessViolation,
    /// E0405: operator applied to invalid operand types (§3.3)
    BadOperand,
    /// E0406: condition is not bool or numeric (§3.3)
    BadCondition,
    /// E0407: nullable used without narrowing (§3.2)
    NullableMisuse,
    /// E0408: assignment to `readonly` outside the constructor
    ReadonlyViolation,
    /// E0409: override / implements / abstract violations (§4.2–4.3)
    BadOverride,
    /// E0410: invalid cast (§3.3)
    BadCast,
    /// E0411: return type errors / missing annotation
    BadReturn,
    /// E0412: only `Error` subclasses may be thrown or caught (§4.6)
    BadThrow,
    /// E0413: `yield` outside a function body
    YieldOutsideFunction,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::WrongEncoding => "E0001",
            Code::InvalidUtf8 => "E0002",
            Code::UnexpectedChar => "E0101",
            Code::UnterminatedString => "E0102",
            Code::InvalidEscape => "E0103",
            Code::InvalidNumericSuffix => "E0104",
            Code::ExpectedDigitAfterDot => "E0105",
            Code::UnterminatedTemplate => "E0106",
            Code::InvalidCharLiteral => "E0107",
            Code::UnterminatedBlockComment => "E0108",
            Code::BadDigitSeparator => "E0109",
            Code::IntOutOfRange => "E0110",
            Code::UnexpectedToken => "E0201",
            Code::MisplacedDeclaration => "E0202",
            Code::InvalidLabel => "E0203",
            Code::InvalidAssignmentTarget => "E0204",
            Code::MixedCoalesce => "E0205",
            Code::AmbiguousExponent => "E0206",
            Code::ReservedBinding => "E0207",
            Code::UndefinedName => "E0301",
            Code::DuplicateDeclaration => "E0302",
            Code::UseBeforeDeclaration => "E0303",
            Code::AssignToConst => "E0304",
            Code::UndefinedLabel => "E0305",
            Code::AwaitOutsideAsync => "E0306",
            Code::InvalidThisSuper => "E0307",
            Code::UnknownTypeName => "E0308",
            Code::ReturnOutsideFunction => "E0309",
            Code::BreakOutsideLoop => "E0310",
            Code::TypeMismatch => "E0401",
            Code::BadCall => "E0402",
            Code::UnknownMember => "E0403",
            Code::AccessViolation => "E0404",
            Code::BadOperand => "E0405",
            Code::BadCondition => "E0406",
            Code::NullableMisuse => "E0407",
            Code::ReadonlyViolation => "E0408",
            Code::BadOverride => "E0409",
            Code::BadCast => "E0410",
            Code::BadReturn => "E0411",
            Code::BadThrow => "E0412",
            Code::YieldOutsideFunction => "E0413",
        }
    }
}

/// A source position: 1-based line, 1-based code-point column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    pub message: String,
    pub pos: Pos,
}

impl Diagnostic {
    pub fn error(code: Code, message: impl Into<String>, pos: Pos) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            pos,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "error[{}] @ {}:{}: {}",
            self.code.as_str(),
            self.pos.line,
            self.pos.col,
            self.message
        )
    }
}
