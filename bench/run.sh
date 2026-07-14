#!/usr/bin/env bash
# Tier 0 vs Tier 1 benchmark (ROADMAP Phase 4). Run from the repo root.
set -euo pipefail
cargo build --release -q
echo "== Tier 0 (MERSEY_JIT=0, bytecode interpreter)"
time MERSEY_JIT=0 ./target/release/mersey run bench/hotloop.mersey
echo "== Tier 1 (Cranelift JIT)"
time ./target/release/mersey run bench/hotloop.mersey

echo
echo "== float64 kernel (Mandelbrot)"
echo "-- Tier 0"
time MERSEY_JIT=0 ./target/release/mersey run bench/float.mersey
echo "-- Tier 1"
time ./target/release/mersey run bench/float.mersey

echo
echo "== calls (fib 32): the subset could not compile a call at all"
echo "-- Tier 0"
time MERSEY_JIT=0 ./target/release/mersey run bench/calls.mersey
echo "-- Tier 1"
time ./target/release/mersey run bench/calls.mersey

echo
echo "== OSR (hot loop, cold function): 200M iterations, called once"
echo "-- Tier 1 (Tier 0 takes minutes: 200M interpreted iterations)"
time ./target/release/mersey run bench/osr.mersey

echo
echo "== objects: fields, array elements and method calls — the heap"
echo "-- Tier 0"
time MERSEY_JIT=0 ./target/release/mersey run bench/objects.mersey
echo "-- Tier 1"
time ./target/release/mersey run bench/objects.mersey

echo
echo "== alloc: new in the hot loop — compiled allocation"
echo "-- Tier 0"
time MERSEY_JIT=0 ./target/release/mersey run bench/alloc.mersey
echo "-- Tier 1"
time ./target/release/mersey run bench/alloc.mersey
