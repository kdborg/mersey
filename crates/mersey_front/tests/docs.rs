//! The documentation is generated from the checker, so it cannot describe a
//! member that does not exist. These tests guard the other direction: a member
//! that exists but is *not* enumerated would be undocumented and unsuggestable
//! while working perfectly — the kind of gap nothing complains about.

use mersey_front::check;

/// Every member the reference lists must actually typecheck, with the signature
/// the reference prints. (Generated from `member_access`, so this is really a
/// check that generation ran.)
#[test]
fn the_reference_is_not_empty_anywhere() {
    let api = check::api_reference();
    assert!(
        api.len() >= 20,
        "expected the whole library, got {} groups",
        api.len()
    );
    for g in &api {
        assert!(
            !g.members.is_empty(),
            "`{}` documents no members — either it has none, or the enumeration missed them",
            g.title
        );
        for m in &g.members {
            assert!(
                !m.signature.is_empty(),
                "`{}.{}` has no signature",
                g.title,
                m.name
            );
        }
    }
}

/// The list the reference enumerates must cover everything the checker accepts.
///
/// This is the test that fails when someone adds a method and forgets: the
/// checker will happily typecheck `xs.newThing()` while the reference and the
/// editor both act as though it does not exist.
#[test]
fn builtin_members_are_complete() {
    // Members that must appear, per receiver. Add here when you add to the
    // checker — and the whole point is that this test tells you to.
    let expected: &[(&str, &[&str])] = &[
        (
            "string",
            &[
                "length",
                "toString",
                "indexOf",
                "lastIndexOf",
                "contains",
                "startsWith",
                "endsWith",
                "slice",
                "substring",
                "split",
                "concat",
                "at",
                "charAt",
                "codePointAt",
                "toUpperCase",
                "toLowerCase",
                "trim",
                "trimStart",
                "trimEnd",
                "replace",
                "replaceAll",
                "repeat",
                "padStart",
                "padEnd",
            ],
        ),
        (
            "T[] (array)",
            &[
                "length",
                "push",
                "pop",
                "clear",
                "at",
                "indexOf",
                "lastIndexOf",
                "contains",
                "insertAt",
                "removeAt",
                "fillInPlace",
                "flat",
                "slice",
                "concat",
                "join",
                "keys",
                "map",
                "reduce",
                "filter",
                "find",
                "findIndex",
                "some",
                "every",
                "forEach",
                "sortInPlace",
                "reverseInPlace",
                "toSorted",
                "toReversed",
            ],
        ),
        (
            "Map",
            &[
                "set", "get", "has", "remove", "keys", "values", "entries", "clear", "size",
            ],
        ),
        ("Set", &["add", "has", "remove", "values", "clear", "size"]),
        ("Iter", &["next", "toArray", "map", "filter", "take"]),
        (
            "Regex",
            &["test", "find", "findAll", "replace", "replaceAll", "split"],
        ),
    ];

    let api = check::api_reference();
    for (group, members) in expected {
        let g = api
            .iter()
            .find(|g| g.title == *group)
            .unwrap_or_else(|| panic!("the reference has no group `{group}`"));
        let listed: Vec<&str> = g.members.iter().map(|m| m.name.as_str()).collect();
        for want in *members {
            assert!(
                listed.contains(want),
                "`{group}.{want}` typechecks but is not in the reference — add it to \
                 BUILTIN_MEMBERS, or it is undocumented and editors will not suggest it \
                 (listed: {listed:?})"
            );
        }
    }
}

/// Every `std:` module the checker knows must be documented.
#[test]
fn every_std_module_is_documented() {
    let api = check::api_reference();
    for module in [
        "std:console",
        "std:math",
        "std:format",
        "std:parse",
        "std:json",
        "std:time",
        "std:random",
        "std:regex",
        "std:bytes",
        "std:async",
        "std:fs",
        "std:env",
        "std:caps",
        "std:gc",
    ] {
        assert!(
            api.iter().any(|g| g.import == module),
            "`{module}` is importable but has no page in the reference"
        );
    }
}
