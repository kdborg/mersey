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
