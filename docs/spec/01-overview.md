# Mersey Language Specification — 1. Overview

Status: draft 0.1

Mersey is derived from JavaScript/TypeScript syntax but is a different language:
statically and strictly typed, class-based (no prototypes), compiled by its own
engine. Familiarity with TS is a design goal; compatibility with JS is not.

## 1.1 Relationship to JavaScript/TypeScript

Kept from JS/TS:

- Expression and statement syntax (operators, `if`/`for`/`while`/`switch`,
  arrow functions, template literals, destructuring).
- `let` / `const` block-scoped declarations.
- TypeScript-style type annotation syntax (`x: int32`, generics `Array<T>`,
  interfaces, union of nullable via `T?` — see §3).
- ES-module `import` / `export` syntax with static resolution.
- `class` syntax, extended with real access control.

Removed:

- **Sloppy mode.** There is only strict semantics. Directives like
  `"use strict"` are meaningless and rejected.
- **Prototypes.** No `prototype` property, no `__proto__`, no
  `Object.create`, no monkey-patching. Class shapes are sealed: instances
  cannot gain, lose, or re-type properties at runtime.
- **`var`**, `with`, `eval`, `Function(string)`, `arguments`.
- **Implicit non-numeric coercion.** No truthy strings, no `==` vs `===`
  distinction (only `==`, which is strict), no `NaN`-producing conversions
  from strings.
- **`undefined`.** There is `null` only, and only on nullable types (`T?`).
- **Automatic semicolon insertion.** Semicolons are required.

## 1.2 One mode of operation

Every conforming implementation executes every program with identical, fully
specified semantics. There are no dialect switches, no per-file pragmas, and no
implementation-defined or undefined behavior. Where C/C++ leave behavior
undefined (signed overflow, shift overflow), Mersey defines it (§3.6).

## 1.3 Consistent API signatures

All standard-library and host APIs follow one convention:

- **Naming:** `lowerCamelCase` methods, `UpperCamelCase` types,
  `SCREAMING_SNAKE` constants.
- **Verb-first methods:** `list.sortInPlace()` vs `list.toSorted()` — mutation
  is always explicit in the name; mutating methods return `void`, never `this`
  or a copy.
- **Options objects:** any function with more than two optional knobs takes a
  trailing typed options record, never positional boolean flags.
- **No overloading on behavior.** A given name does one thing. Overloads may
  vary parameter types, never semantics.
- **Errors:** recoverable failures return `Result<T, E>` or throw typed
  exceptions per a single documented rule per module; never both, never
  sentinel values (`-1`, `null`) for errors.

## 1.4 Compilation model

A Mersey program is a set of UTF-8 source files (§2) forming a static module
graph. Compilation proceeds: decode → lex → parse → bind → type-check →
bytecode. Execution is tiered: interpreter first, JIT for hot code
(see `docs/architecture/engine.md`). Because the type system is sound and
shapes are sealed, the JIT does not speculate on types; it compiles them.

## 1.5 Specification structure

- §2 Lexical structure (`02-lexical.md`)
- §3 Types and conversions (`03-types.md`)
- §4 Classes and modules (`04-classes-and-modules.md`)
- §5 Security model (`05-security.md`)
- §6 Grammar (`06-grammar.md`) — normative EBNF for the full syntax
