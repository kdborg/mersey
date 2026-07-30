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
/// register shape at all, so any function taking one was refused outright; an
/// opaque (a native's `Bytes`) could not survive an OSR entry, be compared
/// against null, or be passed to a native. Each of those is an ownership
/// question as much as a representation one — a handle lives in exactly one
/// place — and getting one wrong loses a value rather than crashing, which is
/// why the two tiers are compared rather than the timings.
#[test]
fn strings_and_opaques_agree_across_the_tier_boundary() {
    check("string-params-and-opaques.mersey");
    let out = run("string-params-and-opaques.mersey", true);
    // "payload" is 7 units, and `i % 7` over 200000 iterations sums to 599994.
    assert!(out.contains("score     1999994"), "{out}");
    // 4 units when present, 1 when null — so the null branch must be taken.
    assert!(out.contains("widthOf   800000 200000"), "{out}");
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
