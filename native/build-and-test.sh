#!/usr/bin/env bash
# Build the embedding library and run the native C host demo (Stage B proof).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p mersey_capi -q
gcc -O2 -Wall -o native/host_demo native/host_demo.c \
    target/release/libmersey_capi.a -lpthread -ldl -lm
./native/host_demo
