#!/usr/bin/env bash
# The gate to run before every commit: format, lints (warnings are errors), all tests.
# Takes seconds on a warm build. Deep fuzzing is opt-in: LAIN_FUZZ_CASES=20000.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy -q --workspace --all-targets -- -D warnings
cargo test -q --workspace
if [[ -n "${LAIN_FUZZ_CASES:-}" ]]; then
    cargo test -q --release -p lain-vt --test fuzz
fi
echo "ok"
