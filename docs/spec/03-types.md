# Mersey Language Specification — 3. Types and Conversions

## 3.1 Primitive types

| Type | Size | Range / notes |
|---|---|---|
| `bool` | 1 byte | `true` / `false` |
| `int8` `int16` `int32` `int64` | 1/2/4/8 | two's complement signed |
| `uint8` `uint16` `uint32` `uint64` | 1/2/4/8 | unsigned |
| `float32` `float64` | 4/8 | IEEE 754 binary32/binary64 |
| `char` | 4 | one Unicode scalar value |
| `bigint` | heap | arbitrary-precision integer |
| `bigdec` | heap | arbitrary-precision decimal (value + scale) |
| `string` | heap | immutable sequence of UTF-16 code units (WTF-16), §3.4 |
| `void` | — | function-return only |

`int` and `uint` are aliases for `int32`/`uint32`; `float` for `float64`.
There is no `number` type and no `undefined`.

## 3.2 Declarations and inference

Only `let` (mutable) and `const` (immutable binding) exist; both are block
scoped with temporal dead zone semantics as in JS strict mode.

```mersey
let a: int64 = 5;      // explicit
let b = 5;             // inferred int32 (literal default)
const c = 2.5f;        // inferred float32
```

Every expression has a static type known at compile time. `any` does not
exist. Where a type genuinely varies, use generics, interfaces, or tagged
unions (`A | B`, discriminated and narrowed by `switch`/`instanceof`).

Nullability is explicit: `T?` admits `null`; `T` does not. Accessing a member
on `T?` requires narrowing (`if (x != null)`) or the `?.` operator.

### Zero-defaults

A declaration without an initializer starts at its type's **zero** — never at
`null`:

| type | starts as |
|---|---|
| every numeric type (`int8`…`uint64`, `float32`, `float64`) | `0` |
| `bigint`, `bigdec` | `0` |
| `string` | `""` |
| `char` | `'\0'` |
| `bool` | `false` |
| `T[]`, `Map<K, V>`, `Set<T>`, `bytes` | empty (a fresh one per binding — two instances of a class never share a default container) |
| `T?` | `null` — the honest zero of a nullable type |

```mersey
let count: int32;          // 0
let name: string;          // ""
let seen: Set<int32>;      // an empty set

class Point {
    public x: float64;     // every Point is born with x == 0
    public tags: string[]; // …and its own empty array
}
```

This applies to locals, fields (instance and static), and resolves through
type aliases — `type Meters = float64` defaults the way `float64` does.

The rule exists because the alternative was the type system lying. A `float64`
field holding `null` is a value its own declared type says cannot exist, and
it was not hypothetical: the engine's compiled tier believes declared types,
and a null in a number-typed field produced a real divergence between tiers.
Now a number-typed cell always holds a number.

The one type with no constructible zero is a non-nullable class or interface:
`public owner: Node;` with no initializer still starts as `null`, and is the
remaining place a declared type can disagree with the value. Prefer `Node?`,
which says what is true. (A definite-assignment rule for reference fields is
tracked in the ROADMAP.)

## 3.3 Numeric conversions (the C/C++ rules, made safe)

Implicit conversion exists **only among numeric types**, following C's model:

1. **Integer promotion.** In arithmetic, `int8/int16/uint8/uint16` promote to
   `int32` first.
2. **Usual arithmetic conversions.** For a binary operator on mixed numeric
   operands, both convert to the *common type*: the wider of the two ranks;
   if one is floating point, the common type is floating point; if signed and
   unsigned of the same rank meet, the unsigned type wins (as in C).
3. **Widening is implicit** in assignment/argument position
   (`int32 → int64`, `int32 → float64`, `float32 → float64`).
4. **Narrowing is explicit.** `int64 → int32`, `float64 → int32`,
   signed ↔ unsigned reinterpretation all require a cast.

Casts use TS `as` syntax with checked semantics, plus explicit unchecked forms:

```mersey
let x: int64 = 5_000_000_000l;
let y = x as int32;          // traps at runtime if out of range
let z = x as wrapping int32; // truncates like C (defined, two's complement)
let f = 0.5 + 1;             // float64: int promotes to float, as in C
```

Two ergonomic rules complete the model:

5. **Literals fit their context.** An unsuffixed integer literal (possibly
   negated) takes the integer type expected by its context when the value
   fits: `let small: int16 = 1000;` is `int16`. A literal that does not fit
   is a compile error (E0110), never a silent wrap.
6. **Compound assignment converts back.** `a op= b` computes in the common
   type and converts the result back to `a`'s type with wrapping, as in C:
   `int16 a; a += 1;` stays `int16`. (Plain `a = a + 1` still requires the
   result type to be assignable — promotion makes it `int32`, so write `+=`
   or cast.)

These conversions are **carried in the bytecode**, not left to the engine to
infer from the values it happens to find. The checker is the only thing that
knows a `7` is being stored into a `float64`, so it records the conversion at the
node where it decided, and the compiler emits it — a literal is *built* at its
declared type, everything else is converted at the point it crosses. Without
that, the engine erased the declared type and dispatched on the value: `let x:
float64 = 7; x / 2` stored an `int32` and did an integer divide, and the answer
was 3. See `docs/architecture/engine.md`.

`bigint` and `bigdec` never convert implicitly to or from fixed-size types
(too easy to lose precision or allocate accidentally); `BigInt.from(x)`,
`big.toInt64()` etc. are explicit.

**No other coercion exists.** `1 + "1"` is a compile error. Conditions
(`if`, `while`, `? :`, `&&`, `||`) accept `bool` or any numeric type, where a
numeric tests `!= 0` — the C convention. Strings, objects, and `null` are not
valid conditions; write the comparison.

## 3.4 Strings and characters

`string` is an immutable sequence of **UTF-16 code units**; `char` is one
Unicode scalar value. A string may contain unpaired surrogates — the encoding
is WTF-16, not strict UTF-16 — because the hosts Mersey shares strings with
produce them, and a string type that cannot hold what the host handed it has
to fail somewhere worse.

This is JS's string model, chosen deliberately: engine strings cross to a JS
or DOM host constantly (§5 of `architecture/web-platform.md`), and any other
representation puts a transcode on the hottest boundary the engine has.

- `s.length` is the **code-unit** count, and every position — `s[i]`, `slice`,
  `indexOf`, `split` — is a code-unit index. For `"a\u{1F600}b"`, `length` is
  4 and `indexOf("b")` is 3.
- `s[i]` is a `char` in O(1): the whole scalar value *beginning* at unit `i`.
  At the lead unit of a surrogate pair that is the character the pair encodes;
  at its trailing unit it is U+FFFD, because a lone surrogate is not a scalar
  value and `char` holds nothing else. This is the one place Mersey does not
  follow JS, which yields the unpaired half instead.
- Iteration (`for (const c of s)`) is by **scalar value**, so a surrogate pair
  is one step: `"a\u{1F600}b"` iterates three times, not four. JS's `for…of`
  agrees.
- `slice` and the other unit-indexed methods may cut a surrogate pair in half,
  and the result is a string holding the unpaired half. JS does this too.
- Comparison (`<`, `>`, and sort order) is by code unit. That is JS's order and
  it is **not** code-point order: the two disagree whenever a non-BMP character
  is compared against a BMP character at U+E000 or above, since the pair's lead
  unit (U+D800–U+DBFF) sorts below it.
- `localeCompare` and normalization live in the standard library, never
  implicitly.
- `trim`, `trimStart` and `trimEnd` remove the whitespace of §2.2 — the same
  set the lexer skips between tokens — and nothing else. Notably U+FEFF (a
  BOM leading a field) *is* removed and U+0085 (NEL) is *not*. Every member of
  that set is in the BMP, so removing units and removing characters are the
  same operation here.
- The engine may transparently use a compact internal representation for
  ASCII-only strings, but the semantics are always the ones above.
- File and network boundaries transcode UTF-8 at the edge. DOM and JS interop
  do not transcode at all, which is the point.

> **Superseded design.** Through Phase 1 this section specified UTF-32 — a
> sequence of 4-byte code points, `length` in code points, "no surrogate pairs
> to mis-handle". The engine is UTF-16 and has been since the browser work;
> checksum parity with the JS benchmark twins depends on the code-unit
> reading. Documents written against the older model may still describe it.

## 3.5 Equality

One equality operator: `==` (and `!=`). It is strict — no coercion beyond the
numeric conversions of §3.3 (so `1 == 1l` is true via widening; `1 == "1"` is
a compile error). Reference types compare by identity unless the class opts
into value equality by implementing `Equatable`.

## 3.6 Defined arithmetic (no UB)

Mersey adopts C's conversion *rules* but none of its undefined behavior:

- Signed and unsigned overflow in `+ - *` **wraps** (two's complement),
  deterministically. Checked (`Math.checkedAdd`) and saturating variants are
  in the standard library; the `mersey check`/debug build can be configured
  to trap on wrap for testing.
- Integer division by zero and `INT_MIN / -1` **trap** (throw `RangeError`).
- Shift counts are masked to the width (`x << (n & 31)` for `int32`), as on
  common hardware, and specified as such.
- Float operations follow IEEE 754 exactly; no fast-math.

### Printing a float

`float32`/`float64` `toString()` — and therefore `console.log`, template
literals, string concatenation and `Json.stringify` — produces exactly what
ECMA-262 `Number::toString` (§6.1.6.1.20) produces:

- the **shortest** digit string that reads back as the same value;
- written positionally while the decimal exponent is in `(-6, 21]`, and in
  exponential form outside it, with the exponent always signed — so `1e21`
  prints as `1e+21` and `1e-7` as `1e-7`, while `1e20` and `1e-6` are written
  out in full;
- `Infinity`, `-Infinity`, `NaN` spelled that way;
- negative zero as `0`, the one place the sign is dropped rather than kept.

This is pinned rather than left to the implementation because there is more
than one reasonable answer and the engine has to give the *same* one on every
tier. Rust's own float formatting — which the interpreter used until this was
written down — differs on all four points, while the transpiler backend emits
JS and so always followed the rule above; the same program printed different
text depending on which tier ran it.

Explicit-precision formatting (`format.fixed` and friends) is a standard
library concern and is not this.

## 3.7 Big numbers

- `bigint`: arbitrary-precision integer. All integer operators work; mixing
  with fixed-size ints requires explicit conversion (§3.3).
- `bigdec`: arbitrary-precision decimal — an integer coefficient with a
  decimal scale, suitable for money and exact decimal arithmetic. `+ - *` are
  exact; `/` requires a rounding context (`a.divide(b, { scale: 2, mode:
  RoundingMode.HALF_EVEN })`) unless the division is exact. Semantics follow
  the proven java.math.BigDecimal / IEEE 754-2019 decimal model.

## 3.8 Composite types

`Array<T>` (dense, homogeneous), fixed typed views `Int32Array` etc. as
aliases of `Array<int32>` slices, `Map<K,V>`, `Set<T>`, tuple types
`[int32, string]`, record/interface types, and function types
`(a: int32) => string`. All are statically typed; arrays never hold holes.


## 3.9 `unknown` — the top type

A value that crosses into the program from outside the type system has type
`unknown`: a parsed JSON document, a JavaScript host object, anything the
compiler cannot have seen a declaration for.

- Anything is **assignable to** `unknown`. That is what makes it the top type.
- `unknown` is **assignable from** nothing. You cannot read its members, call
  it, or index it.
- To use one, **narrow it**: `x as T` (checked at run time — a wrong cast throws
  at the cast) or `x instanceof T`.

There is deliberately no `any`. A type that is assignable in *both* directions
and permits *any* member is not a type; it is a hole in the checker, and it
spreads: every value it touches becomes unchecked too. `unknown` draws the same
boundary honestly — it says "nobody knows what this is yet", and makes you say
what you think it is before you use it.

Functions that keep the width of a number they were given (`math.abs`) are
**generic with a bound**, not untyped: `abs<T: Numeric>(x: T): T`.


## 3.10 `is` — testing a value's type

`x is T` asks whether a value holds a `T`. It is a `bool`, and it **narrows**:

    function describe(v: unknown): string {
        if (v is int32) { return `${v + 1}`; }        // v is an int32 here
        if (v is string) { return v.toUpperCase(); }  // v is a string here
        return "something else";
    }

It is the same question the checked cast `x as T` asks — *answered*, rather than
thrown. That is what makes `unknown` usable without turning ordinary branching
into exception handling.

It is **not** `typeof`. §1.2 has no runtime type reflection: nothing hands a type
back to the program as a value to compute with. `is` tests a value.

- The width is the value's own: an `int64` does not hold an `int32`, even when
  the number would fit.
- `xs is int32[]` tests the elements, not just the container — an answer that
  only checked the container would be a lie the first time someone read one.
- The right-hand side must be a type a value can be tested against: a primitive,
  a class, an interface, or an array of those. Record types are structural — two
  declarations with the same fields are the same type — so there is nothing at
  run time to distinguish them, and `is` will not pretend otherwise.

### Guard clauses narrow the rest of the block

A guard whose body always leaves — returns, throws, breaks, continues — makes the
rest of the block its else-branch:

    function label(id: int32 | string): string {
        if (id is int32) { return `#${id}`; }
        return id.toUpperCase();   // narrowed to string: the only arm left
    }

## 3.11 Members of a union

A union has a member when **every** arm has it, at the same type: then reading it
is safe whichever arm the value turns out to be. If one arm lacks it, or the arms
disagree about its type, it is an error — narrow first.


## 3.12 Protocols: `Iterable<T>`, `AsyncIterable<T>`, `Display`

A class opts into a language protocol by **implementing an interface**:

    class Bag implements Iterable<int32>, Display {
        public iter(): Iter<int32> { for (const n of this.items) { yield n; } }
        public toString(): string { return `Bag(${this.items.join(",")})`; }
    }

- `Iterable<T>` — `iter(): Iter<T>`. `for … of` accepts it.

  `for … of` over an **array** iterates *live*, by index, exactly as JS does:
  the length is re-read each pass, so elements pushed during the loop are
  visited and a shrink ends it early. (It used to iterate a snapshot — a full
  copy of the array per loop, which was both a cost and a departure from the
  JS base semantics.) Strings, maps, sets, generators and `Iterable<T>`
  classes are unaffected.
- `AsyncIterable<T>` — `iter(): AsyncIter<T>`. `for await` accepts it. (An
  `async` method whose body yields *is* an async generator, so it returns
  `AsyncIter<T>`, not `Promise<AsyncIter<T>>`.)
- `Display` — `toString(): string`. Honoured by `console.log`, template
  literals, `join`, and inside arrays and records.

JavaScript spells these with well-known symbols (`Symbol.iterator`,
`Symbol.toPrimitive`). **Mersey has no symbols, and does not need them.** A
symbol-keyed method is a runtime convention the type system cannot see: nothing
tells you that you forgot it, nothing checks its signature, and no editor can
suggest it. An interface is the same extension point, declared and checked — a
class that claims `Iterable<int32>` and provides `iter(): Iter<string>` is a
compile error, not strings arriving where numbers were expected.

Opting in is **explicit**. A class with a suitable `iter()` that never declared
`Iterable` is not iterable: a protocol you can join by accident is a protocol
nobody can rely on.
