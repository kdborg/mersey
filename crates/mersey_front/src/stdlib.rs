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
        _ => return None,
    })
}

/// Is this a `std:` module implemented in Mersey (rather than natively)?
pub fn is_source_module(spec: &str) -> bool {
    source(spec).is_some()
}
