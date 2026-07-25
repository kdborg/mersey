# Conformance suite

The permanent, implementation-independent test suite (ROADMAP Phase 0 item).
Every Mersey implementation — the Rust engine, the Stage A WASM build, and the
Stage B browser forks (Mersey Blink / Gecko / Servo / Ladybird) — must pass it
unmodified.

## Format: golden files

Each case is a UTF-8 `.mersey` file with a sibling `.expect` file holding the
reference tool's exact output for that stage. A case fails if the output
differs byte-for-byte.

```
tests/conformance/
  lexer/    <name>.mersey + <name>.expect   output of `mersey lex`
  parser/   (Phase 1, parsing)              AST dump of `mersey parse`
  checker/  (Phase 1, type checking)        diagnostics of `mersey check`
  runtime/  (Phase 2+)                      stdout of `mersey run`
```

- **Diagnostics are part of the contract.** `.expect` files include
  diagnostic lines (`error[E0104] @ 3:9: …`). Diagnostic *codes* and
  *positions* are stable and normative; message wording may be improved, in
  which case the goldens are re-blessed in the same commit.
- Error-case files are named `err-*.mersey`. Error recovery is tested too:
  the tokens after a bad token are also in the golden file.
- Columns count code points, 1-based (spec §2.1).

## Running

```sh
cargo test -p mersey_front --test conformance          # verify
MERSEY_BLESS=1 cargo test -p mersey_front --test conformance   # regenerate goldens
```

Bless output is reviewed like code: a golden diff in a PR is a semantics
change and gets spec scrutiny.
