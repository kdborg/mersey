// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Whole programs, run by the real binary, with Tier 1 on and with it off.
//!
//! These two cases need things the in-process JIT tests cannot easily reach: a
//! module graph (for `import(…)`), and the engine's real tier thresholds. And both
//! of them are regressions — each one printed a *different answer* on the two
//! tiers before it was fixed, which is the only kind of bug that matters here,
//! because a wrong answer arrives quietly.
//!
//! The golden is beside each program. Re-bless with `MERSEY_BLESS=1`.

use std::path::Path;
use std::process::Command;

fn run(name: &str, jit: bool) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/jit")
        .join(name);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mersey"));
    cmd.arg("run").arg(&path);
    if !jit {
        cmd.env("MERSEY_JIT", "0");
    }
    let out = cmd.output().expect("run mersey");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

fn check(name: &str) {
    let jit = run(name, true);
    let interp = run(name, false);
    assert_eq!(jit, interp, "Tier 1 and the interpreter disagree on {name}");

    let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/jit")
        .join(name)
        .with_extension("expect");
    if std::env::var_os("MERSEY_BLESS").is_some() {
        std::fs::write(&golden, &jit).expect("bless");
        return;
    }
    let want =
        std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e}", golden.display()));
    assert_eq!(jit, want, "{name} against its golden");
}

/// A subclass that overrides a method, arriving *after* the code that calls it was
/// compiled — the one thing that can make class hierarchy analysis false. Compiled
/// code must be discarded, and dispatch must be right.
#[test]
fn a_subclass_arriving_late_invalidates_a_direct_call() {
    check("cha-late-subclass.mersey");
    // 4 + 4, then 4 + 4 + 999. If the compiled direct call had survived the
    // import, the second number would be 12.
    let out = run("cha-late-subclass.mersey", true);
    assert!(out.contains("before import: 8"), "{out}");
    assert!(out.contains("after import: 1007"), "{out}");
}

/// Allocation in compiled code: `new` + compiled constructors, owned locals
/// released on overwrite, returned objects (owned and borrowed), and a trap
/// after allocations — all agreeing with the interpreter, including the
/// deterministic replay of a real use-after-free (see the .mersey file).
#[test]
fn compiled_allocation_agrees_with_the_interpreter() {
    check("alloc.mersey");
}

/// A declaration without an initializer starts at its type's zero — 0, "",
/// '\0', false, an empty container — on every tier, including compiled code
/// reading a default it never saw written. (This file used to pin the *old*
/// behaviour, a `float64` field holding `null`, which produced a real tier
/// divergence; the zero-default rule removed that shape from the language.)
#[test]
fn an_unset_field_starts_at_its_types_zero_on_both_tiers() {
    check("unset-field.mersey");
    let out = run("unset-field.mersey", true);
    assert!(
        out.contains("zeros: 0 0 0 0 false 0 0"),
        "a default was not its type's zero:\n{out}"
    );
    assert!(
        out.contains("not shared: 1 0"),
        "two instances share one default container:\n{out}"
    );
}

/// Strings and opaques crossing the tier boundary. A `string` parameter had no
/// register shape at all, so any function taking one was refused outright; a
/// string *return* had none either, so `Sig` called it `void` and refused the
/// function and every caller of it; and an opaque (a native's `Bytes`) could not
/// survive an OSR entry, be compared against null, or be passed to a native. Each
/// of those is an ownership question as much as a representation one — a handle
/// lives in exactly one place — and getting one wrong loses a value rather than
/// crashing, which is why the two tiers are compared rather than the timings.
#[test]
fn strings_and_opaques_agree_across_the_tier_boundary() {
    check("string-params-and-opaques.mersey");
    let out = run("string-params-and-opaques.mersey", true);
    // "payload" is 7 units, and `i % 7` over 200000 iterations sums to 599994.
    assert!(out.contains("score     1999994"), "{out}");
    // 4 units when present, 1 when null — so the null branch must be taken.
    assert!(out.contains("widthOf   800000 200000"), "{out}");
    // Three string-returning calls per iteration, one of them nullable, one
    // returning a *built* string that has to hand over the handle it owns.
    assert!(out.contains("tally     2795557"), "{out}");
}

/// `math.max`/`math.min` with a NaN argument. The interpreter's fold compared with
/// `<`, which is false whichever side the NaN is on, so it returned the other
/// operand — `math.max(NaN, 5)` was 5 while `math.max(5, NaN)` was NaN, and Tier 1
/// disagreed with both by propagating. Argument order must not change the answer,
/// and the two tiers must not print different ones.
#[test]
fn min_and_max_propagate_nan_on_both_tiers() {
    check("math-minmax-nan.mersey");
    let out = run("math-minmax-nan.mersey", true);
    assert!(out.contains("max nan  NaN NaN"), "{out}");
    assert!(out.contains("min nan  NaN NaN"), "{out}");
    // Symmetric on ordinary arguments too, which is the property NaN broke.
    assert!(out.contains("max      7 7"), "{out}");
    assert!(out.contains("min      3 3"), "{out}");
}

/// Every string method Tier 1 emits, and `==` on strings. One of these anywhere
/// in a function used to cost that function its compilation; they reach the
/// interpreter's one implementation now, by arena handle, with the receiver and
/// arguments parked on the way in — a mistake there hands the method a *different*
/// string rather than failing, so the tiers are compared answer for answer.
/// `==` needs no arena, and its trap is that a null string and an empty one are
/// both "no characters" and must not compare equal.
#[test]
fn string_methods_and_equality_agree_across_the_tier_boundary() {
    check("string-methods.mersey");
    let out = run("string-methods.mersey", true);
    // `slice(1, 4)` and `substring(4, 1)` differ only in that one swaps bounds
    // the wrong way round — both must give "/b/".
    assert!(out.contains(",/b/,/b/,"), "{out}");
    // null == null, and null != "" — 16 (is null) + 2 (not equal) for the mixed
    // pair, against 17 (equal + is null) when both are null.
    assert!(out.contains("eq null  17"), "{out}");
    assert!(out.contains("eq mixed 18"), "{out}");
}

/// A compiled function reading a global of the module it was *written* in. The
/// shims that read a global asked `Interp::globals`, which names the module being
/// *run* — so for a function belonging to any other module the binding was simply
/// absent, the shim answered so, and the compiled body bailed to the interpreter
/// on every iteration. The answers stayed right and the speed stayed interpreted,
/// which is why this asserts what *compiled*, not only what it printed.
#[test]
fn a_compiled_function_reads_its_own_modules_globals() {
    check("module-globals.mersey");
    let out = run("module-globals.mersey", true);
    assert!(out.contains("bufSize   160"), "{out}");
    // 16 units + indexOf("a") == 10 + 1 for the equality, ten times over.
    assert!(out.contains("tableSpan 270"), "{out}");
}

/// Indexing a `Bytes`, and giving one back. `b[i]` is the only indexing the
/// language allows that is neither an array nor a string, and every encoder in
/// the standard library is built on it; a `Bytes?` return is what every `decode`
/// gives. Both reach the interpreter's own implementation, so the bounds check
/// and the message it raises are the same — which a compiled check that
/// disagreed would have left to a program to discover.
#[test]
fn indexing_and_returning_a_bytes_agree_across_the_tier_boundary() {
    check("bytes-index.mersey");
    let out = run("bytes-index.mersey", true);
    assert!(out.contains("total   8480"), "{out}");
    // The null return is handle 0, and must not read as a value.
    assert!(
        out.contains("some    true") && out.contains("none    true"),
        "{out}"
    );
    // Word for word what the interpreter says, length included.
    assert!(
        out.contains("range   index 99 out of bounds (length 16)"),
        "{out}"
    );
}

/// Building an array in compiled code. Reading one always worked — `Ty::Arr`
/// carries the element buffer's address and the length — but that shape is wrong
/// for an array that *grows*, since a `push` moves both. One built here is carried
/// as an opaque instead, by arena handle, which makes it the compiled code's to
/// release when the slot holding it is overwritten. `churn` is the case that
/// matters: a fresh array every iteration, which would grow without bound if the
/// old one were never let go, and would print a wrong number if it were let go too
/// early.
#[test]
fn building_an_array_agrees_across_the_tier_boundary() {
    check("array-build.mersey");
    let out = run("array-build.mersey", true);
    assert!(out.contains("build   124506"), "{out}");
    assert!(out.contains("churn   1500500"), "{out}");
    // An array literal is the same ops in a row: make, then push each element.
    assert!(out.contains("literal 9000"), "{out}");
}

/// A nullable number. `int32?` is what a scan over code points is written in, and
/// every decoder in the standard library starts with one. It rides a single
/// register with `i64::MIN` for null — 0 being an ordinary value, the null test
/// cannot be the "is it zero" one every other nullable here uses — and is unboxed
/// where a number is required, which the checker has already narrowed. The guard
/// on that unbox is not for well-typed code; it is so a stray sentinel bails
/// rather than being read as a number.
#[test]
fn a_nullable_number_agrees_across_the_tier_boundary() {
    check("nullable-number.mersey");
    let out = run("nullable-number.mersey", true);
    // A nullable *parameter*, present and absent, and one that misses its range.
    assert!(out.contains("nib   0 -1 -2"), "{out}");
    // Compared against numbers, added, and handed on to a nullable parameter.
    assert!(out.contains("scan  1408"), "{out}");
    // `codePointAt` past the end is where null actually comes from.
    assert!(out.contains("past  40"), "{out}");
}

/// Top-level bindings of every shape a compiled function can read, a nullable
/// number compared against a plain one, and `split`. The numeric global is read
/// live rather than lifted to the top of the call, because nothing in the engine
/// can tell a `const` from a `let`; `bump` is the case that would break a lifted
/// read, and is refused today only because writing a global has no lowering.
#[test]
fn globals_and_split_agree_across_the_tier_boundary() {
    check("globals-and-split.mersey");
    let out = run("globals-and-split.mersey", true);
    assert!(out.contains("shapes  370"), "{out}");
    assert!(out.contains("bump    55"), "{out}");
    // `cp == QUOTE` needs no unboxing, and `split(",").length` reads through the
    // opaque an array is carried as.
    assert!(out.contains("quoted  442"), "{out}");
}

/// A `string` field and a `static` method. A string field had no register shape,
/// so any method touching one cost its class every compilation. Reading one is a
/// borrow, exactly as an object field's is; *writing* one is the opposite of an
/// object write — the field takes its own copy of the units rather than sharing a
/// reference, which is the only reason a string may be stored into a field when an
/// object may not. A static is a call with no receiver at all.
#[test]
fn string_fields_and_statics_agree_across_the_tier_boundary() {
    check("class-strings-and-statics.mersey");
    let out = run("class-strings-and-statics.mersey", true);
    assert!(out.contains("run 437"), "{out}");
}

/// Arrays of strings — read, written and iterated. An array's elements are
/// `Value`s in a buffer exactly as an object's fields are, so once a string cell
/// had a shape a string element came almost free. The limit is stated in the
/// program: this holds where the element type is *known* (a parameter, a field);
/// an array built in compiled code is an opaque, whose element has no shape at
/// compile time, so an index read off one assumes a number and bails when it is
/// not — slow, and refused further along, never silently wrong.
#[test]
fn string_arrays_agree_across_the_tier_boundary() {
    check("string-arrays.mersey");
    let out = run("string-arrays.mersey", true);
    assert!(out.contains("widths  18"), "{out}");
    assert!(out.contains("joined  18"), "{out}");
    // The elements were rewritten in place, so the lengths are the new ones.
    assert!(out.contains("relabel 4"), "{out}");
    // The array `split` hands back: a *known* opaque, so its elements have a
    // shape. 3 fields + 6 units, twice over, four times.
    assert!(out.contains("fields  60"), "{out}");
    // An array of strings *built here* — comparing an element against a string,
    // slicing it, joining it. Needs the declared element type, which only the
    // checker knows.
    assert!(out.contains("trim    56"), "{out}");
}

/// An engine primitive as a class field, its string parts, and `throw new Error`.
/// Between them these are what `std:url`'s `URL` is made of, and the last of the
/// command-line workloads to be running wholly interpreted. An opaque field is
/// *owned* where a string field is borrowed — there is no representation for one
/// but an arena entry — so reading a part of the same field in a loop would leave
/// an entry behind each time if the reader did not let it go. The throw is lowered
/// as a pair: the interpreter builds the error, the compiled body traps, and what
/// must be checked is that it still throws the same thing with the same message.
#[test]
fn opaque_fields_and_throw_agree_across_the_tier_boundary() {
    check("opaque-fields-and-throw.mersey");
    let out = run("opaque-fields-and-throw.mersey", true);
    assert!(out.contains("span 190"), "{out}");
    assert!(out.contains(r#"bad  not a URL: "nonsense""#), "{out}");
}

/// `Map` and `Set` in compiled code. A keyed reconciler is written out of `has`,
/// `set`, `add` and a `size`, which is the shape browser code leans on hardest.
/// `new Map()` is recognised the way the interpreter recognises it — by the name
/// binding *nothing* — so a program with its own `Map` gets its own; `Own` is here
/// to check `new` of an ordinary class stayed ordinary.
#[test]
fn maps_and_sets_agree_across_the_tier_boundary() {
    check("map-and-set.mersey");
    let out = run("map-and-set.mersey", true);
    // The 1000/4000 terms are the branches that must *not* be taken: a member
    // that was never added, and a second removal of the same key.
    assert!(out.contains("reconcile 114"), "{out}");
    assert!(out.contains("lookup    9"), "{out}");
    assert!(out.contains("own       7"), "{out}");
    // A `Map` carried *through* a signature — as a parameter and as a return.
    assert!(out.contains("carried   85"), "{out}");
}

/// A nullable number returned through a *signature*. The tier has carried one in
/// a register for a while — an `i64` with `i64::MIN` for null — but only as a
/// local, a parameter, or a builtin method's result; a function could not say it
/// returned one. So the signature came out `void`, the body was refused at its
/// first `Return`, and every caller went with it, because a caller that cannot
/// describe the callee cannot call it. That is what kept `parse`-shaped
/// functions interpreted right across the standard library.
///
/// Null here is a *sentinel*, not a zero, and 0 is an ordinary value — so this
/// pins that the two stay distinct on both tiers, on `return null`, on a
/// returned 0, and on falling off the end.
#[test]
fn nullable_returns_agree_across_the_tier_boundary() {
    check("nullable-return.mersey");
    let out = run("nullable-return.mersey", true);
    // Zero comes back as zero, null as null, and neither as the other.
    assert!(out.contains("zeroOrNull 0 null 5"), "{out}");
    // A value found part-way through, and an implicit null off the end.
    assert!(out.contains("firstDigit 7 null"), "{out}");
    assert!(out.contains("digitsOf   5 null null"), "{out}");
}

/// A `bool` crossing a call. A parameter's type says which *register* it uses,
/// and a `bool` uses the `i32` an `int32` does — so the signature said `int32`
/// where the value said `bool`, the call site compared them for equality, and
/// every call to a function taking a `bool` was refused, callers included. The
/// rule is now the one `Return` already used: same machine class, both integral.
/// What this pins is that they stay distinct as *values* despite sharing a
/// register — a bool must not come back as the 0 or 1 it travels in.
#[test]
fn bool_parameters_agree_across_the_tier_boundary() {
    check("bool-params.mersey");
    let out = run("bool-params.mersey", true);
    assert!(out.contains("pick   4 0"), "{out}");
    // Both flags, each alone, and neither — so a swapped or masked argument
    // shows as a different number rather than the same one twice.
    assert!(out.contains("span   15 5 10 0"), "{out}");
    assert!(out.contains("isEven true false"), "{out}");
}

/// Reassigning a variable that holds a built string. This was a use-after-free
/// and it shipped: an assignment is `Dup / StoreSlot / Pop` where a declaration
/// is one store, and `Dup` copied the string's arena handle verbatim — so the
/// slot took one copy and `Pop` released the other, leaving the slot pointing
/// into freed memory. The length register stayed correct, so the string kept its
/// right length and lost its contents, and nothing crashed or bailed. It surfaced
/// as `std:semver` parsing a valid prerelease version into null, several calls
/// downstream of the reassignment that caused it.
///
/// The assertions read *contents* back, not lengths — length was the one thing
/// that stayed right.
#[test]
fn reassigned_strings_agree_across_the_tier_boundary() {
    check("reassigned-strings.mersey");
    let out = run("reassigned-strings.mersey", true);
    // Every one of these printed -1 before the fix: the slot's units were gone.
    assert!(out.contains("fromSelf     1"), "{out}");
    assert!(out.contains("fromOther    1"), "{out}");
    assert!(out.contains("twice        1"), "{out}");
    assert!(out.contains("fromTemplate 1"), "{out}");
    assert!(out.contains("underBranch  1"), "{out}");
    assert!(out.contains("opaque       5"), "{out}");
    // The other route to the same freed buffer, found by audit rather than by a
    // wrong answer: `let a = b; b = …` releases the entry `a` borrows from. The
    // tier guarded this for objects and refused to compile it for arrays and
    // opaques; strings were on neither list, so they took the third option
    // silently.
    assert!(out.contains("alias        1 1"), "{out}");
}

/// The five searching string methods. Compiled code no longer reaches these
/// through the interpreter at all: receiver and needle are already spans in
/// registers and the answer is a number, so the call goes straight to a shim over
/// the two spans — no arena, no `Value`, no argument vector, no dispatch by name.
/// Worth 4.8x on `indexOf` and 7.2x on `startsWith`.
///
/// The search itself is shared (one `find_units`, called from both tiers), so
/// what this pins is everything around it: which argument is the needle, what an
/// empty or over-long one does, and that the index is in code units. Every
/// expected value here was cross-checked against Node.
#[test]
fn string_searches_agree_across_the_tier_boundary() {
    check("string-search.mersey");
    let out = run("string-search.mersey", true);
    // A haystack with a character outside the BMP, so a code-point index and a
    // code-unit index disagree and the test can tell them apart.
    assert!(out.contains("dot       306100"), "{out}");
    assert!(out.contains("emoji     1110"), "{out}");
    assert!(out.contains("whole     1111"), "{out}");
    // An empty needle matches at the start, and at the end for `lastIndexOf`.
    assert!(out.contains("empty     4111"), "{out}");
    // Longer than the haystack, and simply absent: -1 from both indexers.
    assert!(out.contains("toolong   -100000"), "{out}");
    assert!(out.contains("absent    -100000"), "{out}");
    // Present twice, so first and last differ.
    assert!(out.contains("repeated  104100"), "{out}");
}

/// `slice`, `substring`, `charAt` and `codePointAt`. Compiled code reaches these
/// through shims over the receiver's span now: `codePointAt` is a pure function
/// of a span and an index, and the other three allocate their result but need
/// nothing else the general path costs — no `Value` for the receiver, no arena
/// slot per integer argument, no argument vector, no name comparison. Worth 6.2x
/// on `codePointAt` and about 4x on `slice` and `substring`.
///
/// The bounds arithmetic is shared between the tiers, so what this pins is the
/// edges — where two routes to one rule come apart.
#[test]
fn string_subranges_agree_across_the_tier_boundary() {
    check("string-subrange.mersey");
    let out = run("string-subrange.mersey", true);
    // An index inside a surrogate pair gives a lone surrogate, not a character.
    assert!(
        out.contains("mid   [\u{fffd}a|\u{fffd}a.b|\u{fffd}a|\u{fffd}a|\u{fffd}|65533]"),
        "{out}"
    );
    // Negative and past-the-end. Note this is *not* what JS does — a negative
    // index clamps to 0 for `slice` and counts from the end for `charAt` — and
    // it is pinned here so that changing it later is a decision, not a drift.
    assert!(out.contains("neg   [abcd|abcd|abcd|abcd|c|99]"), "{out}");
    // Bounds the wrong way round: `slice` gives nothing, `substring` swaps.
    assert!(out.contains("rev   [|d|bc|bc|d|100]"), "{out}");
    assert!(out.contains("zero  [|abcd|||a|97]"), "{out}");
    // Entirely past the end: empty strings, and null from `codePointAt`.
    assert!(out.contains("past  [|||||null]"), "{out}");
}

/// A nullable number meeting a plain one at a merge. `x == null ? 0 : x` is how
/// parsing code is written: the checker narrows `x` in the else-arm, but a slot's
/// type does not follow that narrowing, so the two arms disagree about what the
/// merged value is and the tier refused the whole function. It converts now —
/// narrowing with a guard in one direction, sign-extending in the other.
///
/// The guard is the part that needs pinning, so every case is also run past the
/// end of the string, where the arm carrying the sentinel is the one taken.
#[test]
fn nullable_merges_agree_across_the_tier_boundary() {
    check("nullable-merge.mersey");
    let out = run("nullable-merge.mersey", true);
    // In range, then out of range — 'a' is 97, and absent falls to the constant.
    assert!(out.contains("digitOr  97 0"), "{out}");
    assert!(out.contains("weigh    195 3"), "{out}");
    // The widening direction, and a null that must survive as null.
    assert!(out.contains("orNull   7 97 null"), "{out}");
}

/// A borrow crossing a block edge whose source is then overwritten. This was
/// refused, and soundly: a borrow rooted in a re-assignable local lives only as
/// long as that local, and a block parameter has no provenance to carry the
/// guard across. Giving the borrow a reference of its own before it crosses
/// removes the question instead of tracking it.
///
/// Each case overwrites the source *after* the merge and then reads what
/// crossed, so a missing promotion is a read of freed memory — a wrong answer
/// rather than a crash, which is the shape of the two use-after-frees this
/// engine has shipped. The assertions read contents, never lengths.
#[test]
fn borrows_across_edges_agree_across_the_tier_boundary() {
    check("borrow-across-edge.mersey");
    let out = run("borrow-across-edge.mersey", true);
    // Both ways through the merge: the borrowed arm and the constant arm.
    assert!(out.contains("viaTernary  1 1"), "{out}");
    assert!(out.contains("viaBranch   1 1"), "{out}");
    // An opaque, whose handle is its identity rather than a pointer.
    assert!(out.contains("viaOpaque   6 2"), "{out}");
}

/// A `std:` native's result returned where a string was promised. The tier does
/// not know what a native returns, so every native's result is an opaque arena
/// handle — fine until the function hands it back, at which point an opaque met a
/// promised string and the whole function was refused.
///
/// The value *is* a string; only its label was missing, so the return re-labels
/// it: same arena entry, same single owner. What makes that safe is that the
/// shim checks, and bails to the interpreter if the handle names anything else —
/// a slow answer rather than a wrong one. `null` is not such a case: it is a
/// string-shaped nothing and must come back as null, which the invalid-UTF-8
/// case exercises.
#[test]
fn native_string_returns_agree_across_the_tier_boundary() {
    check("native-string-return.mersey");
    let out = run("native-string-return.mersey", true);
    assert!(out.contains("decoded   héllo 5"), "{out}");
    assert!(out.contains("empty     [] 0"), "{out}");
    // A decode that fails must be null, not a bail and not an empty string.
    assert!(out.contains("invalid   null"), "{out}");
}

/// `super.method()`. A super call is statically bound — one body, always that
/// body — so it takes the direct-call path rather than virtual dispatch, and the
/// `overridden_below` test that keeps an ordinary method call honest is not
/// merely unnecessary here but wrong.
///
/// The subtlety is which class it resolves *from*: the one that **declares** the
/// running method, not the receiver's. `C` inherits `B.score` without overriding
/// it, so compiling that body with a `C` receiver must still reach `A.score` —
/// resolving from the receiver would find `B.score` again, an infinite regress
/// rather than a wrong number.
#[test]
fn super_methods_agree_across_the_tier_boundary() {
    check("super-method.mersey");
    let out = run("super-method.mersey", true);
    assert!(out.contains("one   7 11"), "{out}");
    assert!(out.contains("viaB  1400000"), "{out}");
    // The inherited case: 200000 * (5*2+1).
    assert!(out.contains("viaC  2200000"), "{out}");
    // `class X extends Error` is constructed by the engine, so the tier refuses
    // that `super(m)` rather than treating it as an ordinary base-constructor
    // call — which would not fail, but would quietly drop the message.
    assert!(out.contains("err   boom-7"), "{out}");
}

/// An array literal whose elements are not numbers. A built array is an opaque
/// and its elements are pushed one at a time, and only a scalar could be pushed —
/// a number has a 64-bit form to hand the shim and a reference does not. Strings
/// were worse than refused: the analysis accepted them and the code generator did
/// not, so the two passes disagreed and the refusal arrived after acceptance.
///
/// A reference goes over by arena handle now — minted at the push, taken by the
/// shim, one owner the whole way. These three shapes were 1 compiled / 3 refused
/// before and are 4 / 0 now.
#[test]
fn array_literals_of_references_agree_across_the_tier_boundary() {
    check("array-literal-refs.mersey");
    let out = run("array-literal-refs.mersey", true);
    assert!(out.contains("objs  600000"), "{out}");
    assert!(out.contains("strs  600000"), "{out}");
    assert!(out.contains("opq   400000"), "{out}");
}

/// A *setter*. Its twin the getter has been a compiled call for a while — `o.p`
/// is a field read's syntax over a method call's body — and the write side was
/// simply never written, so a class with both compiled its reads and dropped the
/// whole enclosing function on its writes. Not visible in any refusal histogram:
/// those count what a function does, not what it was refused for.
///
/// The asymmetry is ownership. `o.p = v` evaluates to `v`, and the call path
/// releases every argument once the callee returns, so the setter is handed a
/// *duplicate* — otherwise the assignment's own value is freed memory, which is a
/// wrong answer and not a crash. Hence `text`, which reads contents back rather
/// than a length.
#[test]
fn setters_agree_across_the_tier_boundary() {
    check("setter.mersey");
    let out = run("setter.mersey", true);
    // The setter doubles, so `c.value = 21` reads back as 42, once.
    assert!(out.contains("one      42 1"), "{out}");
    // The string that crossed the call, and the string the caller kept.
    assert!(
        out.contains("text     <row-3>|row-3 <row-11>|row-11"),
        "{out}"
    );
    assert!(out.contains("strings  1200000"), "{out}");
    // The two accessor bodies differ by 99 per iteration, so a `Base`-typed
    // receiver holding a `Sub` compiled to `Base`'s body would print the first
    // number twice. That is the failure this direct call has to not have.
    assert!(out.contains("dispatch -1474736480 -1454936480"), "{out}");
}

/// What a compiled frame owns when it returns. Nothing swept a callee's frame —
/// `jit_arena.clear()` runs when the outermost compiled call returns — so a
/// `split` result parked in an inner function's local survived until then, one
/// arena entry per call. The CLI `strings` benchmark peaked at 89 MB where the
/// same program interpreted took 6.3, and it grew without bound with the work.
///
/// The dangerous half is the returned value: a local handed back is a borrow, so
/// the return promotes it first and only then may the frame be swept. Get that
/// order wrong and the caller reads the entry that was just freed — a wrong
/// answer, not a crash — so these are contents, read after allocation churn.
#[test]
fn a_returning_frame_releases_what_it_owns() {
    check("frame-sweep.mersey");
    let out = run("frame-sweep.mersey", true);
    assert!(out.contains("viaLocal  7-14 180"), "{out}");
    // The early exit, and the value that crossed it.
    assert!(out.contains("early     20 short"), "{out}");
    assert!(out.contains("opaque    payload-9"), "{out}");
}

/// An object stored into a field — the store `Op::SetMember` refused, whose
/// refusal cascaded: a constructor that keeps a reference could not compile, so
/// no `new` of that class could, so no function building one could. A nine-op
/// `Entry(node, v)` took a whole keyed reconciler with it.
///
/// The ordering is the correctness argument. `cell_set_obj` takes its reference
/// before assigning through the cell, because the assignment drops whatever the
/// field held — and if the field already held this object, dropping first would
/// free the thing being stored. `selfAssign` is that case, and it reads contents
/// back rather than identity, since a freed-then-reused cell can compare equal.
#[test]
fn an_object_stored_into_a_field_agrees_across_the_tier_boundary() {
    check("object-field.mersey");
    let out = run("object-field.mersey", true);
    // Assigned from itself, twice, and still itself.
    assert!(out.contains("selfAssign  41"), "{out}");
    // The old value survives being displaced; the new one is what the field has.
    assert!(out.contains("swapped     9 41"), "{out}");
    // A subclass into a base-typed field.
    assert!(out.contains("viaSub      400000"), "{out}");
    // An object pushed onto an array field and read back through it: the field
    // read becomes an opaque when it feeds a push (an address and a length are
    // the wrong shape for something that grows), and `box_arg` parks the object.
    assert!(out.contains("collect     200010001"), "{out}");
}
