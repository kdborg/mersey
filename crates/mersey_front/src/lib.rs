// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Mersey language frontend.
//!
//! Pipeline (spec §1.4): decode → lex → parse → bind → typecheck.
//! This crate currently implements decode (spec §2.1) and lex (spec §6.2).

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
