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
| `string` | heap | immutable sequence of `char` (UTF-32), §3.4 |
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

`bigint` and `bigdec` never convert implicitly to or from fixed-size types
(too easy to lose precision or allocate accidentally); `BigInt.from(x)`,
`big.toInt64()` etc. are explicit.

**No other coercion exists.** `1 + "1"` is a compile error. Conditions
(`if`, `while`, `? :`, `&&`, `||`) accept `bool` or any numeric type, where a
numeric tests `!= 0` — the C convention. Strings, objects, and `null` are not
valid conditions; write the comparison.

## 3.4 Strings and characters

`string` is an immutable sequence of 4-byte code points.

- `s.length` is the code-point count; `s[i]` is a `char` in O(1). There are
  no surrogate pairs to mis-handle and no “length in UTF-16 units” trap.
- Comparison is by code points; `localeCompare` and normalization live in the
  standard library, never implicitly.
- The engine may transparently use a compact internal representation for
  ASCII-only strings, but the semantics are always UTF-32.
- Host boundaries (DOM, JS interop, files, network) transcode at the edge;
  inside Mersey a string is always whole code points.

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
