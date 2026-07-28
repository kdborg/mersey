// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Mersey language frontend.
//!
//! Pipeline (spec §1.4): decode → lex → parse → bind → typecheck.
//! This crate currently implements decode (spec §2.1) and lex (spec §6.2).

/// The frontend's maps, hashed with FxHash rather than SipHash.
///
/// Binder scopes, checker environments, the module graph and the WebIDL surface
/// are all keyed by *program text* — identifiers, type names, module paths —
/// which arrives from a source file the toolchain was pointed at, not from a
/// running program. There is no table here an attacker chooses keys for, so
/// SipHash's collision resistance defends nothing while costing every lookup;
/// FxHash is what rustc hashes its own symbols with. Sharing one hasher with
/// `mersey_interp` also keeps a single `hashbrown` instantiation in the wasm
/// build instead of two.
pub type HashMap<K, V> = std::collections::HashMap<K, V, rustc_hash::FxBuildHasher>;
pub type HashSet<K> = std::collections::HashSet<K, rustc_hash::FxBuildHasher>;

pub mod ast;
pub mod astdump;
pub mod bind;
pub mod check;
pub mod diag;
pub mod fmt;
pub mod graph;
pub mod lexer;
pub mod parser;
pub mod source;
pub mod sourcemap;
pub mod stdlib;
pub mod token;
pub mod webapi;
