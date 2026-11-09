#!/usr/bin/env bash
# Line/region coverage for the core crates (lain-vt, lain-pty), measured across all
# their tests plus the end-to-end suite. GPU code is excluded: it's covered by the
# pure `frame`/`boxdraw`/`images` unit tests and by manual runs.
#
#   scripts/coverage.sh            # summary
#   scripts/coverage.sh --html     # also write target/coverage/html/index.html
#
# Needs: rustup component add llvm-tools
set -euo pipefail
cd "$(dirname "$0")/.."

bin="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin"
[[ -x "$bin/llvm-profdata" ]] || { echo "run: rustup component add llvm-tools" >&2; exit 1; }

out=target/coverage
rm -rf "$out/prof" && mkdir -p "$out/prof"
export CARGO_TARGET_DIR="$out/build"
export RUSTFLAGS="-C instrument-coverage"
export LLVM_PROFILE_FILE="$PWD/$out/prof/%p-%m.profraw"

cargo test -q -p lain-vt -p lain-pty -p lain-e2e >/dev/null

"$bin/llvm-profdata" merge -sparse "$out"/prof/*.profraw -o "$out/merged.profdata"

objects=()
for f in $(cargo test -q -p lain-vt -p lain-pty -p lain-e2e --no-run --message-format=json 2>/dev/null |
    sed -n 's/.*"executable":"\([^"]*\)".*/\1/p'); do
    objects+=(-object "$f")
done

filter=(--ignore-filename-regex='(\.cargo/registry|/rustc/|/tests/|lain-e2e/src|/examples/)')
"$bin/llvm-cov" report "${objects[@]}" -instr-profile="$out/merged.profdata" "${filter[@]}" |
    sed "s|$PWD/||"
if [[ "${1:-}" == "--html" ]]; then
    "$bin/llvm-cov" show "${objects[@]}" -instr-profile="$out/merged.profdata" "${filter[@]}" \
        -format=html -output-dir="$out/html" >/dev/null
    echo "html: $out/html/index.html"
fi
