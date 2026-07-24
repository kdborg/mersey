//! The Mersey engine, linked into libxul through gkrust.
//!
//! There is nothing to translate here. `mersey_capi` already exposes the C ABI
//! (`msy_context_new`, `msy_context_run`, `msy_context_invoke`, …) that
//! `dom/mersey/MerseyScriptRunner.cpp` calls, and that the Chromium fork's
//! `//components/mersey` calls, and that `native/host_demo.c` calls. One
//! boundary, three hosts.
//!
//! This crate exists only to pull that ABI into gkrust's link, so the symbols
//! are in libxul. Re-exporting is what makes the linker keep them: a Cargo
//! dependency whose symbols nothing references would be dropped.

pub use mersey_capi::{
    msy_abi_version, msy_context_free, msy_context_invoke, msy_context_invoke_args,
    msy_context_new, msy_context_new_ex, msy_context_release_callback, msy_context_run,
    msy_context_run_graph, msy_context_scan_imports, MsyContext, MsyHostTable,
    MSY_ABI_VERSION, MSY_FLAG_NO_JIT,
};
