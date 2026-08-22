#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
cargo build --release
mkdir -p build
cp -f target/release/libreference.so build/reference.so
cp -f target/release/librandom_engine.so build/random.so
