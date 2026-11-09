#!/usr/bin/env bash
# Main-thread wakeups per second of an idle lain window (should be 0 without a
# blinking cursor; see third_party/gpui/LAIN_PATCH.md).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -q --release -p lain
target/release/lain -e sh -c 'printf "\033[2 q"; exec sleep 600' >/dev/null 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null' EXIT
sleep 3
ctx() { awk '/^voluntary_ctxt/{print $2}' "/proc/$pid/task/$pid/status"; }
a=$(ctx); sleep 10; b=$(ctx)
echo "idle main-thread wakeups: $(( (b - a) / 10 ))/s"
