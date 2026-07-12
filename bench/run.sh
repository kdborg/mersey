#!/usr/bin/env bash
# Tier 0 vs Tier 1 benchmark (ROADMAP Phase 4). Run from the repo root.
set -euo pipefail
cargo build --release -q
echo "== Tier 0 (MERSEY_JIT=0, bytecode interpreter)"
time MERSEY_JIT=0 ./target/release/mersey run bench/hotloop.mersey
echo "== Tier 1 (Cranelift JIT)"
time ./target/release/mersey run bench/hotloop.mersey
