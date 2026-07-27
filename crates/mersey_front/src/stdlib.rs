// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Standard-library modules written **in Mersey**.
//!
//! Phase 3 shipped `std:` as native kernels only. These modules are ordinary
//! Mersey source, loaded through the module graph like any other import — so
//! the standard library is partly self-hosted, and anything it can express, a
//! user's own library can too.

/// The Mersey source of a `std:` module, if it has one.
pub fn source(spec: &str) -> Option<&'static str> {
    Some(match spec {
        "std:result" => include_str!("../../../std/result.mersey"),
        "std:test" => include_str!("../../../std/test.mersey"),
        "std:url" => include_str!("../../../std/url.mersey"),
        "std:date" => include_str!("../../../std/date.mersey"),
        "std:abort" => include_str!("../../../std/abort.mersey"),
        "std:http" => include_str!("../../../std/http.mersey"),
        "std:semver" => include_str!("../../../std/semver.mersey"),
        "std:path" => include_str!("../../../std/path.mersey"),
        "std:base64" => include_str!("../../../std/base64.mersey"),
        "std:hex" => include_str!("../../../std/hex.mersey"),
        "std:uuid" => include_str!("../../../std/uuid.mersey"),
        "std:mime" => include_str!("../../../std/mime.mersey"),
        "std:csv" => include_str!("../../../std/csv.mersey"),
        "std:cookie" => include_str!("../../../std/cookie.mersey"),
        "std:jwt" => include_str!("../../../std/jwt.mersey"),
        _ => return None,
    })
}

/// Is this a `std:` module implemented in Mersey (rather than natively)?
pub fn is_source_module(spec: &str) -> bool {
    source(spec).is_some()
}

/// Every `std:` module written in Mersey. The documentation enumerates these the
/// same way it enumerates the native ones: by checking them and listing what they
/// export, so the reference cannot describe an export that does not exist.
pub fn source_modules() -> &'static [&'static str] {
    &[
        "std:result",
        "std:url",
        "std:date",
        "std:abort",
        "std:test",
        "std:http",
        "std:semver",
        "std:path",
        "std:base64",
        "std:hex",
        "std:uuid",
        "std:mime",
        "std:csv",
        "std:cookie",
        "std:jwt",
    ]
}
