#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Install with: cargo install diesel_cli --version 2.4.0 --no-default-features --features sqlite-bundled --locked
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
diesel migration run --database-url "$scratch/schema.db" --migration-dir crates/quartz/migrations --config-file /dev/null
diesel print-schema --database-url "$scratch/schema.db" > "$scratch/schema.rs"
diff -u crates/quartz/src/store/schema.rs "$scratch/schema.rs"
