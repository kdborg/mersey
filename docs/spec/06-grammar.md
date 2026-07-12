# Mersey Language Specification — 6. Grammar

Status: draft 0.1. This is the normative syntax; earlier sections govern
semantics. The parser this describes needs no backtracking beyond the
bounded lookaheads listed in §6.9.

## 6.1 Notation

```
Rule ::= …;         definition (order of alternatives is not significant)
'x'                 terminal (literal token text)
A | B               alternatives
A?  A*  A+          optional, zero-or-more, one-or-more
( … )               grouping
/…/                 character set, regex-style, lexical rules only
```

Whitespace and comments (spec §2.2–2.3) may appear between any two tokens
and are not shown. There is no automatic semicolon insertion; every `;`
below is a real token.

## 6.2 Lexical grammar

```
Identifier    ::= IdStart IdContinue* ;         /* not a ReservedWord; NFC-compared, §2.4 */
IdStart       ::= /\p{ID_Start}/ | '_' | '$' ;
IdContinue    ::= /\p{ID_Continue}/ | '$' ;

ReservedWord  ::=
    'abstract' | 'as' | 'async' | 'await' | 'break' | 'case' | 'catch'
  | 'class' | 'const' | 'continue' | 'default' | 'do' | 'else' | 'enum'
  | 'export' | 'extends' | 'extern' | 'false' | 'final' | 'finally' | 'for'
  | 'from' | 'function' | 'get' | 'if' | 'implements' | 'import' | 'in'
  | 'instanceof' | 'interface' | 'let' | 'new' | 'null' | 'of' | 'override'
  | 'private' | 'protected' | 'public' | 'return' | 'set' | 'static'
  | 'super' | 'switch' | 'this' | 'throw' | 'true' | 'try' | 'type'
  | 'typeof' | 'void' | 'while' | 'wrapping' | 'yield'
  | PredefinedTypeName ;

PredefinedTypeName ::=
    'bool' | 'char' | 'string' | 'bigint' | 'bigdec'
  | 'int' | 'int8' | 'int16' | 'int32' | 'int64'
  | 'uint' | 'uint8' | 'uint16' | 'uint32' | 'uint64'
  | 'float' | 'float32' | 'float64' ;

/* Reserved for future use, illegal in 0.1 programs: 'in', 'typeof',
   'yield'. All other reserved words are used by this grammar. */

IdentifierName ::= Identifier | ReservedWord ;   /* member names only, §6.9 */
```

### Numeric literals (spec §2.6, §3.1)

```
IntLiteral    ::= (DecDigits | HexLiteral | OctLiteral | BinLiteral) IntSuffix? ;
DecDigits     ::= /[0-9]/ ( /[0-9_]/* /[0-9]/ )? ;
HexLiteral    ::= '0x' /[0-9a-fA-F]/ ( /[0-9a-fA-F_]/* /[0-9a-fA-F]/ )? ;
OctLiteral    ::= '0o' /[0-7]/ ( /[0-7_]/* /[0-7]/ )? ;
BinLiteral    ::= '0b' /[01]/ ( /[01_]/* /[01]/ )? ;
IntSuffix     ::= 'u' | 'l' | 'ul'
                | 'i8' | 'i16' | 'i32' | 'i64'
                | 'u8' | 'u16' | 'u32' | 'u64' ;

FloatLiteral  ::= DecDigits '.' DecDigits Exponent? FloatSuffix?
                | DecDigits Exponent FloatSuffix?
                | DecDigits FloatSuffix ;
Exponent      ::= ('e' | 'E') ('+' | '-')? DecDigits ;
FloatSuffix   ::= 'f' ;

BigIntLiteral ::= (DecDigits | HexLiteral | OctLiteral | BinLiteral) 'n' ;
BigDecLiteral ::= DecDigits ('.' DecDigits)? Exponent? 'm' ;
```

No suffix: integer literals are `int32`, float literals `float64`. A suffix
is part of the token (no whitespace). `1.foo()` is a parse error; write
`(1).foo()` or `1.0.foo()`.

### String, character, template literals

```
StringLiteral ::= '"' DQChar* '"' | "'" SQChar* "'" ;
DQChar        ::= Escape | /[^"\\\r\n  ]/ ;
SQChar        ::= Escape | /[^'\\\r\n  ]/ ;
Escape        ::= '\\' ( 'n' | 'r' | 't' | '0' | '\\' | "'" | '"' | '`'
                        | 'u{' /[0-9a-fA-F]/+ '}' ) ;

CharLiteral   ::= "c'" (Escape | /[^'\\\r\n]/) "'" ;   /* exactly one code point */

/* Template literals are lexed as a head/middle/tail token sequence with
   the parser recursing for each substitution: */
TemplateHead   ::= '`' TChar* '${' ;
TemplateMiddle ::= '}' TChar* '${' ;
TemplateTail   ::= '}' TChar* '`' ;
NoSubTemplate  ::= '`' TChar* '`' ;
TChar          ::= Escape | /[^`\\$]/ | '$' /[^{]/ ;
```

## 6.3 Types

```
Type            ::= UnionType ;
UnionType       ::= PostfixType ('|' PostfixType)* ;
PostfixType     ::= PrimaryType PostfixTypeOp* ;
PostfixTypeOp   ::= '?'                         /* nullable, §3.2      */
                  | '[' ']' ;                   /* sugar for Array<T>  */
PrimaryType     ::= PredefinedTypeName
                  | 'void'
                  | TypeReference
                  | TupleType
                  | RecordType
                  | FunctionType
                  | '(' Type ')' ;

TypeReference   ::= QualifiedName TypeArguments? ;
QualifiedName   ::= Identifier ('.' Identifier)* ;
TypeArguments   ::= '<' Type (',' Type)* '>' ;
TupleType       ::= '[' Type (',' Type)+ ']' ;  /* 1-tuples don't exist */
RecordType      ::= '{' (RecordTypeMember (',' | ';'))* '}' ;
RecordTypeMember::= 'readonly'? IdentifierName '?'? ':' Type ;
FunctionType    ::= TypeParameters? '(' ParameterTypeList? ')' '=>' Type ;
ParameterTypeList ::= ParameterType (',' ParameterType)* ;
ParameterType   ::= '...'? Identifier '?'? ':' Type ;

TypeParameters  ::= '<' TypeParameter (',' TypeParameter)* '>' ;
TypeParameter   ::= Identifier ('extends' Type)? ;
TypeAnnotation  ::= ':' Type ;
```

`T?` binds tighter than `|`; `A | B?` is `A | (B?)`. `void` is legal only
as a function return type (semantic restriction).

## 6.4 Expressions

Presented as a precedence ladder, loosest first.

```
Expression      ::= AssignmentExpr (',' AssignmentExpr)* ;   /* comma only in 'for' heads */

AssignmentExpr  ::= ConditionalExpr
                  | ArrowFunction
                  | LeftHandSideExpr AssignmentOp AssignmentExpr ;
AssignmentOp    ::= '=' | '+=' | '-=' | '*=' | '/=' | '%=' | '**='
                  | '<<=' | '>>=' | '&=' | '|=' | '^=' | '&&=' | '||=' | '??=' ;

ConditionalExpr ::= CoalesceExpr ('?' AssignmentExpr ':' AssignmentExpr)? ;

/* '??' may not mix with '&&'/'||' without parentheses (as in JS): */
CoalesceExpr    ::= LogicalOrExpr ('??' LogicalOrExpr)* ;
LogicalOrExpr   ::= LogicalAndExpr ('||' LogicalAndExpr)* ;
LogicalAndExpr  ::= BitOrExpr ('&&' BitOrExpr)* ;
BitOrExpr       ::= BitXorExpr ('|' BitXorExpr)* ;
BitXorExpr      ::= BitAndExpr ('^' BitAndExpr)* ;
BitAndExpr      ::= EqualityExpr ('&' EqualityExpr)* ;
EqualityExpr    ::= RelationalExpr (('==' | '!=') RelationalExpr)* ;
RelationalExpr  ::= ShiftExpr (('<' | '>' | '<=' | '>=' | 'instanceof') ShiftExpr)* ;
ShiftExpr       ::= AdditiveExpr (('<<' | '>>') AdditiveExpr)* ;
AdditiveExpr    ::= MultiplicativeExpr (('+' | '-') MultiplicativeExpr)* ;
MultiplicativeExpr ::= ExponentExpr (('*' | '/' | '%') ExponentExpr)* ;
ExponentExpr    ::= CastExpr ('**' ExponentExpr)? ;         /* right-assoc */

CastExpr        ::= UnaryExpr ('as' 'wrapping'? Type)* ;    /* §3.3 */

UnaryExpr       ::= ('+' | '-' | '~' | '!' | 'await') UnaryExpr
                  | UpdateExpr ;
UpdateExpr      ::= ('++' | '--') UnaryExpr
                  | LeftHandSideExpr ('++' | '--')? ;

LeftHandSideExpr::= PrimaryExpr Suffix* ;
Suffix          ::= '.' IdentifierName
                  | '?.' IdentifierName
                  | '[' Expression ']'
                  | '?.' '[' Expression ']'
                  | TypeArguments? Arguments
                  | '?.' Arguments ;
Arguments       ::= '(' (Argument (',' Argument)*)? ')' ;
Argument        ::= '...'? AssignmentExpr ;

PrimaryExpr     ::= 'this'
                  | 'super' ('.' IdentifierName | Arguments)
                  | Identifier
                  | Literal
                  | ArrayLiteral
                  | RecordLiteral
                  | TemplateLiteral
                  | NewExpr
                  | ImportCall
                  | '(' AssignmentExpr ')' ;

Literal         ::= IntLiteral | FloatLiteral | BigIntLiteral | BigDecLiteral
                  | StringLiteral | CharLiteral | 'true' | 'false' | 'null' ;

NewExpr         ::= 'new' TypeReference Arguments ;
ImportCall      ::= 'import' '(' AssignmentExpr ')' ;       /* §4.5 */

ArrayLiteral    ::= '[' (Element (',' Element)* ','?)? ']' ;
Element         ::= '...'? AssignmentExpr ;                 /* no holes */

RecordLiteral   ::= '{' (RecordField (',' RecordField)* ','?)? '}' ;
RecordField     ::= IdentifierName ':' AssignmentExpr
                  | Identifier                              /* shorthand {x} */
                  | '...' AssignmentExpr ;

TemplateLiteral ::= NoSubTemplate
                  | TemplateHead AssignmentExpr
                    (TemplateMiddle AssignmentExpr)* TemplateTail ;

ArrowFunction   ::= 'async'? (Identifier | ParameterClause) TypeAnnotation?
                    '=>' (AssignmentExpr | Block) ;
```

Restrictions carried from JS, kept deliberately: the operand of `**` may not
be an unparenthesized unary expression (`-2 ** 3` is a parse error); an
arrow body starting `{` is always a Block. `**=`..`??=` follow their
operators' semantics. There is no `>>>` — `>>` is arithmetic on signed,
logical on unsigned operands (§3.6). There is no `,`-expression outside
`for` heads, no `delete`, `void expr`, `typeof expr`, or `in` expression.

## 6.5 Statements

```
Statement       ::= Block | VariableStatement | ExpressionStatement
                  | IfStatement | IterationStatement | SwitchStatement
                  | BreakStatement | ContinueStatement | ReturnStatement
                  | ThrowStatement | TryStatement | LabeledStatement | ';' ;

Block           ::= '{' Statement* '}' ;
ExpressionStatement ::= Expression ';' ;      /* may not start '{', 'function', 'class' */

VariableStatement ::= ('let' | 'const') Binding (',' Binding)* ';' ;
Binding         ::= BindingTarget TypeAnnotation? ('=' AssignmentExpr)? ;
BindingTarget   ::= Identifier | ArrayPattern | RecordPattern ;
ArrayPattern    ::= '[' (PatternElem (',' PatternElem)* (',' '...' BindingTarget)?)? ']' ;
PatternElem     ::= BindingTarget ('=' AssignmentExpr)? ;
RecordPattern   ::= '{' PatternField (',' PatternField)* '}' ;
PatternField    ::= IdentifierName (':' BindingTarget)? ('=' AssignmentExpr)? ;

IfStatement     ::= 'if' '(' Expression ')' Statement ('else' Statement)? ;

IterationStatement ::=
      'while' '(' Expression ')' Statement
    | 'do' Statement 'while' '(' Expression ')' ';'
    | 'for' '(' ForInit? ';' Expression? ';' Expression? ')' Statement
    | 'for' 'await'? '(' ('let' | 'const') BindingTarget TypeAnnotation?
          'of' AssignmentExpr ')' Statement ;
ForInit         ::= ('let' | 'const') Binding (',' Binding)* | Expression ;

SwitchStatement ::= 'switch' '(' Expression ')'
                    '{' CaseClause* DefaultClause? CaseClause* '}' ;
CaseClause      ::= 'case' AssignmentExpr ':' Statement* ;   /* constant expr, checked */
DefaultClause   ::= 'default' ':' Statement* ;

BreakStatement    ::= 'break' Identifier? ';' ;
ContinueStatement ::= 'continue' Identifier? ';' ;
ReturnStatement   ::= 'return' Expression? ';' ;
ThrowStatement    ::= 'throw' Expression ';' ;
LabeledStatement  ::= Identifier ':' IterationStatement ;    /* loops only */

TryStatement    ::= 'try' Block CatchClause+ ('finally' Block)?
                  | 'try' Block 'finally' Block ;
CatchClause     ::= 'catch' '(' Identifier ':' Type ')' Block ;   /* typed, §4.6 */
```

There is no `for`-`in` (no enumerable prototype keys to walk); iterate
`map.keys()` etc. with `for`-`of`. Non-block bodies (`if (x) f();`) are
legal; `mersey fmt` adds braces.

## 6.6 Declarations: functions, classes, interfaces, enums, aliases

```
FunctionDeclaration ::= 'async'? 'function' Identifier TypeParameters?
                        ParameterClause TypeAnnotation? Block ;
ParameterClause ::= '(' (Parameter (',' Parameter)* (',' RestParameter)?
                        | RestParameter)? ')' ;
Parameter       ::= BindingTarget '?'? TypeAnnotation? ('=' AssignmentExpr)? ;
RestParameter   ::= '...' Identifier TypeAnnotation ;

ClassDeclaration ::= 'abstract'? 'final'? 'class' Identifier TypeParameters?
                     ('extends' TypeReference)?
                     ('implements' TypeReference (',' TypeReference)*)?
                     '{' ClassMember* '}' ;

ClassMember     ::= FieldDeclaration | MethodDeclaration
                  | AccessorDeclaration | ConstructorDeclaration ;

/* Modifier order is fixed; fmt enforces, parser requires: */
MemberModifiers ::= AccessModifier? 'static'? ('abstract' | 'final' | 'override')? ;
AccessModifier  ::= 'public' | 'protected' | 'private' ;    /* omitted = private */

FieldDeclaration ::= MemberModifiers 'readonly'? IdentifierName
                     TypeAnnotation ('=' AssignmentExpr)? ';' ;
MethodDeclaration ::= MemberModifiers 'async'? IdentifierName TypeParameters?
                      ParameterClause TypeAnnotation (Block | ';') ;
                      /* ';' body only if abstract */
AccessorDeclaration ::= MemberModifiers 'get' IdentifierName '(' ')' TypeAnnotation Block
                      | MemberModifiers 'set' IdentifierName
                        '(' Parameter ')' Block ;
ConstructorDeclaration ::= AccessModifier? 'constructor' ParameterClause Block ;

InterfaceDeclaration ::= 'interface' Identifier TypeParameters?
                         ('extends' TypeReference (',' TypeReference)*)?
                         '{' (InterfaceMember (';' | ','))* '}' ;
InterfaceMember ::= 'readonly'? IdentifierName '?'? ':' Type
                  | IdentifierName TypeParameters? ParameterClause TypeAnnotation ;

EnumDeclaration ::= 'enum' Identifier (':' PredefinedTypeName)? /* integer types only */
                    '{' EnumMember (',' EnumMember)* ','? '}' ;
EnumMember      ::= Identifier ('=' AssignmentExpr)? ;          /* constant expr */

TypeAliasDeclaration ::= 'type' Identifier TypeParameters? '=' Type ';' ;
```

Exported functions and all class members must write their return type
(`TypeAnnotation` required in `MethodDeclaration`); local function return
types may be inferred (§4.4) — the parser accepts omission, the checker
enforces the export rule.

## 6.7 Modules

```
Module          ::= ModuleItem* ;
ModuleItem      ::= ImportDeclaration | ExportDeclaration | Declaration | Statement ;
Declaration     ::= FunctionDeclaration | ClassDeclaration | InterfaceDeclaration
                  | EnumDeclaration | TypeAliasDeclaration ;

ImportDeclaration ::= 'import' ImportClause 'from' StringLiteral ';'
                    | 'import' StringLiteral ';' ;            /* side-effect only */
ImportClause    ::= '{' ImportSpecifier (',' ImportSpecifier)* ','? '}'
                  | '*' 'as' Identifier ;
ImportSpecifier ::= Identifier ('as' Identifier)? ;

ExportDeclaration ::= 'export' 'extern'? Declaration
                    | 'export' 'extern'? VariableStatement    /* const only */
                    | 'export' '{' ExportSpecifier (',' ExportSpecifier)* ','? '}'
                      ('from' StringLiteral)? ';' ;
ExportSpecifier ::= Identifier ('as' Identifier)? ;
```

There are **no default exports** — every export is named, per the
consistent-API rules (§1.3): one construct, one spelling at both ends.
`export extern` additionally exposes the declaration to the host world
(JS interop in the browser profile — see
`docs/architecture/browser-integration.md`).

## 6.8 Goal symbol

```
SourceFile ::= Module ;    /* every .mersey file is a module; no script goal */
```

## 6.9 Disambiguation notes (normative)

1. **Member names.** After `.`/`?.`, in `RecordLiteral`, `RecordType`,
   class/interface member positions, `IdentifierName` admits reserved words
   (`config.type`, `req.from` are legal). Binding positions never do.
2. **`get`/`set`.** In a class body, `get`/`set` followed by an
   `IdentifierName` then `(` is an accessor; `get`/`set` followed directly
   by `(` or `<` is an ordinary method named `get`/`set` (2-token lookahead).
3. **Generic call vs. comparison.** In `f<T>(x)`, `<` after an expression
   is a type-argument list if a balanced `>` is followed by `(` — the TS
   rule, bounded lookahead, no semantic feedback into the lexer.
4. **`>>` in nested type arguments** (`Array<Array<int32>>`): the parser
   splits the `>>`/`>>=` token when closing type-argument lists.
5. **Arrow vs. parenthesized expression.** `(` starts an arrow parameter
   clause iff the balanced `)` is followed by `=>` or `: Type =>`;
   otherwise it is a parenthesized expression (same strategy as TS).
6. **Suffix adjacency.** Literal suffixes (`u`, `l`, `f`, `n`, `m`, `i8`…)
   are lexed as part of the numeric token; `5 l` is two tokens and a parse
   error, `5l` is one token.
7. **`as wrapping`.** `wrapping` is a reserved word and appears only
   between `as` and a type; `x as wrapping` without a type is a parse error.
8. **Dangling else** binds to the nearest `if`, as everywhere.
