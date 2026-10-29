#!/bin/bash
# Reference solution: installs the complete jqlite implementation into /app.
set -euo pipefail
SOLUTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

rm -rf /app/src
cp -r "$SOLUTION_DIR/jqlite/src" /app/src
cp "$SOLUTION_DIR/jqlite/Cargo.toml" /app/Cargo.toml

cd /app
cargo build --release --offline
python3 run_examples.py
