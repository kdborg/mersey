# Mersey Language Specification — 2. Lexical Structure

## 2.1 Source encoding: UTF-8

A `.mersey` source file is encoded in UTF-8 — the encoding every common
editor, VCS, and web server handles natively. The compiler decodes it once at
the front door, and from the lexer onward *source text* is a sequence of whole
code points: identifiers, columns and escapes are all counted that way.

That is the compiler's business only. The `string` **value** a program builds
is a sequence of UTF-16 code units (§3.4) — a different model, for a different
reason — so a string literal is transcoded from the source's code points to
code units when it is lexed.

- **Accepted encoding:** UTF-8 only. A leading UTF-8 BOM (`EF BB BF`) is
  accepted and ignored; `mersey fmt` removes it. UTF-16/UTF-32 files are
  rejected with a diagnostic naming the detected encoding and suggesting
  `mersey convert`.
- **Validity:** the byte sequence must be well-formed UTF-8 encoding Unicode
  scalar values only (no surrogates, no overlong forms, per RFC 3629 /
  Unicode conformance). Invalid sequences are a compile error, never
  replaced silently with U+FFFD.
- The compiler's internal representation is the decoded code-point sequence;
  column numbers in diagnostics count code points (not bytes), so they match
  what an editor shows.

**Tooling:** `mersey convert <file>` transcodes UTF-16/UTF-32 → UTF-8;
`mersey fmt` always writes UTF-8 without BOM.

## 2.2 Whitespace and line structure

Line terminators: U+000A (LF), U+000D U+000A (CRLF, counted as one
terminator), U+2028, U+2029. `mersey fmt` normalizes to LF.

**Whitespace** is exactly this set, and it may appear between any two tokens
(§6.1):

| | |
|---|---|
| U+0009, U+000B, U+000C | TAB, VT, FF |
| U+000A, U+000D, U+2028, U+2029 | the line terminators above |
| U+FEFF | ZWNBSP (a BOM anywhere but the first byte, §2.1) |
| U+0020, U+00A0, U+1680, U+2000–U+200A, U+202F, U+205F, U+3000 | every `Space_Separator` (Zs) |

This is JS's set — ECMA-262 `WhiteSpace ∪ LineTerminator` — and it is the
**one** set the language means by "space". In particular, `trim`, `trimStart`
and `trimEnd` (§3.4) remove these code points and no others, so a program and
the compiler that reads it agree on the question.

It is deliberately not Unicode's `White_Space` property, which the two differ
from on exactly two code points: `White_Space` excludes U+FEFF and includes
U+0085 (NEL). Neither is a judgement call left to the implementation.

U+200B (ZWSP) is not whitespace. It is `Format`, not `Space_Separator`, and it
is not in JS's set either.

## 2.3 Comments

`// line` and `/* block */` (non-nesting), as in JS. No HTML-style comments.

## 2.4 Identifiers

`ID_Start ID_Continue*` per Unicode UAX #31, plus `_` and `$` as start
characters. Identifiers are compared by code points after NFC normalization;
two identifiers that differ only in normalization form are the same
identifier (and the formatter rewrites to NFC).

## 2.5 Keywords

Reserved: `abstract as async await break case catch class const continue
default do else enum export extends extern false final finally for from
function get if implements import in instanceof interface let new null of
override private protected public readonly return set static super switch
this throw true try type typeof void while wrapping yield` plus the primitive
type names
in §3.1. No contextual keywords: reserved words are reserved everywhere.
(`in`, `typeof`, and `yield` are reserved for future use and appear in no
0.1 production.) Reserved words are still permitted as *member names* —
after `.`/`?.`, in record literals/types, and as class members — see
grammar §6.9.

Notably absent: `var`, `undefined`, `with`, `eval`, `arguments`, `delete`.

## 2.6 Literals

- **Integer:** `123`, `0x7F`, `0o17`, `0b1010`, with `_` digit separators.
  Default type `int32`; suffixes select others: `u` (`uint32`), `l` (`int64`),
  `ul` (`uint64`), and the explicit forms `i8 i16 i32 i64 u8 u16 u32 u64`
  (e.g. `255u8`).
  A literal that does not fit its type is a compile error, not a wrap.
- **Floating point:** `1.5`, `1e10`, default `float64`; suffix `f` → `float32`.
- **Big numbers:** suffix `n` → `bigint` (`123n`); suffix `m` → `bigdec`
  (`1.05m`). `bigdec` literals are exact decimal, not binary-rounded.
- **String:** `"…"` or `'…'`, immutable UTF-16 code units (§3.4). Template literals
  `` `a ${expr} b` `` require `expr` to have an explicit `string`-convertible
  type via its `toString(): string` — there is no implicit any-to-string.
- **Character:** `c'A'`, `c'\u{1F600}'` — type `char`, a single code point
  (4 bytes). Distinct from a 1-length string.
- **Boolean / null:** `true`, `false`, `null`.
- **Escapes:** `\n \r \t \0 \\ \' \" \u{XXXXXX}`. Legacy `\uXXXX` (UTF-16
  style) and octal escapes are not supported; `\u{…}` names a code point
  directly, and one above U+FFFF becomes the two code units that encode it
  (§3.4). There is no escape that writes a lone surrogate into a literal.

## 2.7 Semicolons

Statements are terminated by `;`. There is no automatic semicolon insertion;
a missing semicolon is a parse error with a fix-it hint.
