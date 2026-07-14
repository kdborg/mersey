# Mersey Language Specification — 4. Classes and Modules

Every example on this page is a program that runs, not a sketch. The working
code is executed by `tests/conformance/runtime/class-features.mersey` and the
output shown beside it is the output it printed; the mistakes are compiled by
`tests/conformance/checker/class-errors.mersey` and the codes shown are the codes
it reported. A feature that stops working — or that never worked the way this
page describes — fails one of those two tests.

## 4.1 Classes, not prototypes

Classes are the only object mechanism. A class declaration fixes its instance
layout permanently:

- Instances cannot gain, lose, or re-type properties at runtime.
- There is no `prototype` object, no `__proto__`, no runtime patching of
  methods. Method dispatch is static where the receiver type is exact, and a
  vtable call otherwise — exactly the C++ model.
- Consequence for the engine: every property access compiles to a fixed
  offset load; no hidden classes, shape transitions, or inline-cache misses.

This is not an aspiration about the engine; it is what the engine does. Tier 1
compiles `p.x` to a load at a constant offset, with **no class check**, because a
subclass's layout begins with its base's — so an offset computed for `Shape` is
still the right offset on a `Circle`, and a `Shape[]` full of both runs the same
compiled code. And because the class set is *closed* (no `eval`, no prototype
patching, a static module graph — §4.5), the engine can ask whether any subclass
overrides a method: when none does, `s.area()` is a direct jump with no vtable and
no inline cache. A language with prototypes cannot ask that question. This is what
deleting them was *for*. `tests/conformance/runtime/jit-heap.mersey` is where all
three tiers are made to agree about it.

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

const acct = new Account(1, "ada");
acct.deposit(150m);
console.log("balance:", acct.getBalance());   // balance: 150
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

```mersey
class Vault {
    rate: float64 = 0.05;                     // no modifier: private
    private pin: int32 = 1234;
    protected key: string = "shared-with-subclasses";
    public label: string = "public";

    public check(guess: int32): bool {
        return guess == this.pin;             // the declaring class sees `private`
    }
}

new Vault().check(1234);   // true
new Vault().pin;           // error[E0404]: `pin` is private
```

### `static`

Static members belong to the class, not to an instance.

```mersey
class Counter {
    private static made: int32 = 0;

    public static create(): Counter {
        Counter.made += 1;
        return new Counter();
    }

    public static total(): int32 { return Counter.made; }
}

Counter.create();
Counter.create();
console.log("static:", Counter.total());   // static: 2
```

### `readonly`

A `readonly` field is assignable in the constructor and nowhere else.

```mersey
acct.id;        // 1
acct.id = 2;    // error[E0408]: `id` is readonly
```

### `get` / `set` accessors

An accessor is called like a field and typed like a method.

```mersey
class Temperature {
    private celsius: float64 = 0.0;

    public get fahrenheit(): float64 {
        return this.celsius * 9.0 / 5.0 + 32.0;
    }

    public set fahrenheit(f: float64) {
        this.celsius = (f - 32.0) * 5.0 / 9.0;
    }
}

const temp = new Temperature();
temp.fahrenheit = 212.0;              // calls the setter
console.log(temp.fahrenheit);         // 212 — calls the getter
```

## 4.3 Inheritance and interfaces

Single inheritance (`extends`), multiple interface implementation
(`implements`). `super` calls as in TS. Abstract classes may declare abstract
members.

### `abstract`, `extends`, `super`, `override`, `final`

```mersey
abstract class Shape {
    public readonly name: string;

    public constructor(name: string) { this.name = name; }

    public abstract area(): float64;          // no body: a subclass must supply one

    public final describe(): string {         // `final`: no subclass may replace it
        return `${this.name} of area ${this.area()}`;
    }
}

class Circle extends Shape {
    private r: float64;

    public constructor(r: float64) {
        super("circle");                      // the base constructor runs first
        this.r = r;
    }

    public override area(): float64 {         // `override` is required, not optional
        return 3.14159 * this.r * this.r;
    }
}

class Square extends Shape {
    private side: float64;

    public constructor(side: float64) {
        super("square");
        this.side = side;
    }

    public override area(): float64 { return this.side * this.side; }
}

const shapes: Shape[] = [new Circle(1.0), new Square(3.0)];
for (const s of shapes) {
    console.log("shape:", s.describe());
}
// shape: circle of area 3.14159
// shape: square of area 9

new Shape("x");   // error[E0402]: cannot instantiate abstract class `Shape`
```

Omitting `override` on a method that redeclares a base method is an error
(`E0409: method `m` shadows a base method; add `override``): silent shadowing is
the bug the keyword exists to prevent.

### Interfaces

Interfaces are structural in what they *require*, but a class satisfies one by
declaration only (nominal conformance) — this keeps dispatch compilable and
prevents accidental conformance. The checker verifies the signatures, not just
the names.

```mersey
interface Drawable { draw(): string; }
interface Sized { size(): int32; }

class Icon implements Drawable, Sized {
    public draw(): string { return "[icon]"; }
    public size(): int32 { return 16; }
}

function render(d: Drawable): string { return d.draw(); }

render(new Icon());   // "[icon]"
```

A class that declares `implements Drawable` but whose `draw` returns the wrong
type does not compile — matching the shape by accident is not conformance, and
declaring conformance you do not have is an error.

An interface may require a **getter**. From the caller's side that is all a
getter is — `s.size` either works or it does not — so it is required as a
readonly property, and a class satisfies it with an accessor or with a plain
readonly field, whichever it actually has:

```mersey
interface Sized {
    get size(): int32;
}

class Box implements Sized {
    private items: int32[] = [1, 2, 3];
    public get size(): int32 { return this.items.length; }   // computed
}

class Fixed implements Sized {
    public readonly size: int32 = 9;                          // stored
}

const s: Sized = new Box();
s.size;       // 3
s.size = 5;   // error[E0408]: `size` is readonly on interface `Sized`
```

Adding `set size(v: int32)` to the interface makes it writable. (`get(` with no
name is still a *method* called `get`, as in a class body — §6.9.)

### Generics, and bounded type parameters

Generics use TS syntax with constraints (`<T extends Numeric>`). They are
reified enough for runtime checks (`x is Box<int32>` is answerable) but compiled
via a hybrid of specialization for primitive type arguments (so `Array<int32>`
stores raw 4-byte ints, not boxes) and shared code for reference types — the CLR
model, chosen for performance.

```mersey
class Box<T> {
    private value: T;

    public constructor(value: T) { this.value = value; }

    public get(): T { return this.value; }
}

new Box<string>("held").get();   // "held"
```

A bound is what makes a type parameter *usable*. `T extends Numeric` says every
`T` that can be substituted is a number, so arithmetic on two `T`s is a `T` —
and the width survives the call, which is the whole point of having widths:

```mersey
function sum<T extends Numeric>(xs: T[], zero: T): T {
    let total = zero;
    for (const x of xs) { total = total + x; }
    return total;
}

sum<int32>([1, 2, 3], 0);        // 6      — an int32, not a float
sum<float64>([0.5, 0.25], 0.0);  // 0.75

sum<string>(["a"], "");          // error[E0401]: `string` does not satisfy the bound `Numeric`
```

`%` and `**` are *not* offered on a `Numeric` `T`: `Numeric` admits `bigdec`,
which has neither, and an operator that only works for some substitutions is a
promise the language cannot keep. An unbounded `T` supports no arithmetic at
all — with no bound, nothing is known.

### `is`

`is` is a checked runtime test that narrows the value in the branch it guards —
no cast, and nothing to get wrong:

```mersey
function area(s: Shape): float64 {
    if (s is Circle) {
        return s.area();   // `s` is a Circle here
    }
    return 0.0;
}
```

## 4.4 Functions

Function declarations, arrow functions, and methods all require fully typed
signatures (parameter and return types; return type may be inferred for
non-exported functions). There is no `arguments` object.

Optional parameters are `p?: T` (implicitly `T?`, defaulting to `null`),
defaults are `p: T = value`, and rest parameters are `...rest: T[]`:

```mersey
function greet(name: string, greeting: string = "hello", loud?: bool): string {
    const text = `${greeting}, ${name}`;
    return loud == true ? text.toUpperCase() : text;
}

greet("ada");                 // "hello, ada"
greet("ada", "hi", true);     // "HI, ADA"

function joinAll(sep: string, ...parts: string[]): string {
    return parts.join(sep);
}

joinAll("-", "a", "b", "c");  // "a-b-c"

const double = (n: int32): int32 => n * 2;   // an arrow function is a value
double(21);                                  // 42
```

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
  below it, and a type alias may name one declared below it. Module-level
  `let`/`const` follow textual order with a temporal dead zone, exactly like
  locals — this is a compiled language, not a top-to-bottom script.
- **A declaration is not a variable, and cannot be assigned to** (`E0304`).
  `f = g` on a function, class, enum or import does not compile. A `let` is the
  only binding that may be reassigned:

  ```mersey
  function f(): int32 { return 1; }
  f = g;          // error[E0304]: cannot assign to `f`: it is a function declaration
  console = x;    // error[E0304]: cannot assign to `console`: it is an import
  ```

  This is what the rest of the design rests on. Classes are sealed, dispatch is
  static, the module graph is closed, and the JIT compiles a call into a direct
  jump to the function the name refers to — every one of those assumes the name
  still means what it meant. A binding that can be repointed at run time takes
  all of it back, and a declaration was never a thing anyone wanted to reassign:
  it says what something *is*, not which value a variable happens to hold.

## 4.6 Exceptions

`try` / `catch` / `finally` with **typed catches**. Only `Error` subclasses may
be thrown, and catch clauses are matched in order by runtime type — so the
specific one goes first:

```mersey
class WithdrawalError extends Error {
    public readonly attempted: bigdec;

    public constructor(message: string, attempted: bigdec) {
        super(message);
        this.attempted = attempted;
    }
}

function withdraw(amount: bigdec): void {
    if (amount > 100m) { throw new WithdrawalError("over limit", amount); }
    if (amount <= 0m) { throw new RangeError("amount must be positive"); }
}

for (const amount of [500m, -1m, 50m]) {
    try {
        withdraw(amount);
        console.log("withdrew:", amount);
    } catch (e: WithdrawalError) {
        console.log("caught WithdrawalError:", e.message, e.attempted);
    } catch (e: Error) {
        console.log("caught Error:", e.message);
    } finally {
        // runs on every path, including the ones that threw
    }
}
// caught WithdrawalError: over limit 500
// caught Error: amount must be positive
// withdrew: 50

throw 42;   // error[E0412]: only `Error` subclasses may be thrown
```
