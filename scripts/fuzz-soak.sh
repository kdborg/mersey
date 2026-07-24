#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kirk D. Brown

# Long-running fuzzing, for the criterion the roadmap states in wall-clock terms.
#
# The harness is deterministic: every run prints the seed it used, so a failure
# reproduces exactly with
#
#     cargo run --release -p mersey_fuzz -- all <iters> <seed>
#
# Usage: scripts/fuzz-soak.sh [minutes]   (default: 60)
set -euo pipefail
cd "$(dirname "$0")/.."

minutes="${1:-60}"
deadline=$(( $(date +%s) + minutes * 60 ))
batch=${FUZZ_BATCH:-50000}
seed=${FUZZ_SEED:-$(date +%s)}
runs=0

cargo build --release -p mersey_fuzz

echo "fuzzing for ${minutes}m from seed ${seed} (batches of ${batch})"
while [ "$(date +%s)" -lt "$deadline" ]; do
    if ! ./target/release/mersey-fuzz all "$batch" "$seed"; then
        echo "FAILURE at seed ${seed} — reproduce with:"
        echo "  cargo run --release -p mersey_fuzz -- all ${batch} ${seed}"
        exit 1
    fi
    runs=$(( runs + 1 ))
    seed=$(( seed + 1 ))
done

echo "clean: ${runs} batches x ${batch} iterations"
