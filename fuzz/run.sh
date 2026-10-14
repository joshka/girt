#!/usr/bin/env bash
# Seeds corpora if absent, then fuzzes each target for a bounded time.
#
# Usage: fuzz/run.sh [seconds-per-target] [target...]
# Defaults to 60 seconds and every target. Needs a nightly toolchain, cargo-fuzz and Git.
# libFuzzer's timeout and RSS limit back up the explicit budgets in `girt::fuzzing`.
set -euo pipefail
cd "$(dirname "$0")"
seconds=${1:-60}
shift || true
targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
    mapfile -t targets < <(cargo +nightly fuzz list)
fi
[ -d corpus ] || ./seed.py corpus
cargo +nightly fuzz build
failed=()
for target in "${targets[@]}"; do
    mkdir -p "corpus/$target"
    echo "fuzzing $target for ${seconds}s"
    cargo +nightly fuzz run "$target" "corpus/$target" -- \
        -max_total_time="$seconds" -timeout=10 -rss_limit_mb=2048 -max_len=1048576 \
        -print_final_stats=1 || failed+=("$target")
done
if [ ${#failed[@]} -ne 0 ]; then
    echo "failing targets: ${failed[*]}; inputs are under fuzz/artifacts" >&2
    exit 1
fi
