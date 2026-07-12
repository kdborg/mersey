# Mersey Language Specification — 4. Classes and Modules

## 4.1 Classes, not prototypes

Classes are the only object mechanism. A class declaration fixes its instance
layout permanently:

- Instances cannot gain, lose, or re-type properties at runtime.
- There is no `prototype` object, no `__proto__`, no runtime patching of
  methods. Method dispatch is static where the receiver type is exact, and a
  vtable call otherwise — exactly the C++ model.
- Consequence for the engine: every property access compiles to a fixed
  offset load; no hidden classes, shape transitions, or inline-cache misses.

```mersey
class Account {
    private balance: bigdec = 0m;
    protected readonly owner: string;
    public readonly id: uint64;

    public constructor(id: uint64, owner: string) {
        this.id = id;
        this.owner = owner;
    }

    public deposit(amount: bigdec): void {
        if (amount <= 0m) { throw new RangeError("amount must be positive"); }
        this.balance = this.balance + amount;
    }

    public getBalance(): bigdec { return this.balance; }
}
```

## 4.2 Access control

Three levels, enforced at compile time by the type checker and at runtime by
the engine (reflection and the embedding API cannot bypass them):

- `private` — the declaring class only.
- `protected` — the declaring class and its subclasses.
- `public` — everyone.

The default, when no modifier is written, is `private` — encapsulation is
opt-out, not opt-in, in keeping with the security-first design. `mersey fmt`
inserts explicit modifiers so real code always reads unambiguously.

Additional modifiers: `static`, `readonly` (assignable only in the
constructor), `abstract`, `final` (non-overridable method / non-extendable
class), `override` (required when overriding — silent shadowing is an error),
`get`/`set` accessors.

## 4.3 Inheritance and interfaces

Single inheritance (`extends`), multiple interface implementation
(`implements`). Interfaces are structural in what they require but classes
satisfy them by declaration only (nominal conformance) — this keeps dispatch
compilable and prevents accidental conformance. `super` calls as in TS.
Abstract classes may declare abstract members.

Generics use TS syntax (`class Box<T> { … }`) with constraints
(`<T extends Comparable<T>>`). Generics are reified enough for runtime checks
(`x instanceof Box<int32>` is answerable) but compiled via a hybrid of
specialization for primitive type arguments (so `Array<int32>` stores raw
4-byte ints, not boxes) and shared code for reference types — the CLR model,
chosen for performance.

## 4.4 Functions

Function declarations, arrow functions, and methods all require fully typed
signatures (parameter and return types; return type may be inferred for
non-exported functions). Optional parameters `p?: T` (implicitly `T?` with
`null` default) and default values are allowed; there is no `arguments`
object; rest parameters are `...rest: Array<T>`.

## 4.5 Modules

ES-module syntax, fully static:

```mersey
import { Account } from "./account.mersey";
import * as fmt from "std:format";
export class SavingsAccount extends Account { … }
```

- Import specifiers resolve at compile/load time; the module graph is closed
  before execution starts. `import()` (dynamic) exists but loads only modules
  whose types were declared at compile time via an import-map manifest — no
  string-built code loading.
- No `eval`, no `new Function(string)`, no runtime module synthesis. This is
  load-bearing for both performance (whole-program knowledge) and security
  (§5).
- `std:` namespace for the standard library; `browser:` for host bindings in
  the browser profile (see architecture docs).
- Module-level *declarations* (functions, classes, interfaces, enums, type
  aliases, imports) are order-independent: a function may call one declared
  below it. Module-level `let`/`const` follow textual order with a temporal
  dead zone, exactly like locals — this is a compiled language, not a
  top-to-bottom script.

## 4.6 Exceptions

`try` / `catch` / `finally` with **typed catches**:

```mersey
try {
    risky();
} catch (e: RangeError) {
    …
} catch (e: Error) {
    …
}
```

Only `Error` subclasses may be thrown. Catch clauses are matched in order by
runtime type.
