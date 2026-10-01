#!/usr/bin/env bash
# Measures a jj build on a real-sized repository: clone, no-op fetch and `jj util gc`.
#
# Reports wall time and peak resident memory per step (macOS `/usr/bin/time -l`), then checks the
# result with `git fsck` and reports the pack size. Run it once with a girt-based jj and once with
# stock jj to compare. The default repository is github.com/jj-vcs/jj (about 130k objects and
# 1,500 branches), which is large enough to expose whole-history and whole-pack costs.
#
# Usage: bench-large.sh <label> <path-to-jj> <work-dir> [repository-url]

set -u
LABEL=$1
JJ=$2
WORK=$3/$LABEL
URL=${4:-https://github.com/jj-vcs/jj}
rm -rf "$WORK"
mkdir -p "$WORK"
cd "$WORK"
export JJ_CONFIG=/dev/null JJ_USER=bench JJ_EMAIL=bench@example.com

measure() {
    local step=$1
    shift
    /usr/bin/time -l "$@" >"$WORK/$step.out" 2>"$WORK/$step.err"
    local status=$?
    local real rss
    real=$(awk '/ real /{print $1}' "$WORK/$step.err")
    rss=$(awk '/maximum resident set size/{printf "%.0f", $1/1048576}' "$WORK/$step.err")
    printf '%-8s %-12s status=%s real=%ss rss=%sMiB\n' "$LABEL" "$step" "$status" "$real" "$rss"
    [ "$status" = 0 ] || grep -E '^(Error|Caused|[0-9]+:)' "$WORK/$step.err" | head -5
}

measure clone "$JJ" git clone "$URL" repo
cd repo
measure fetch-noop "$JJ" git fetch
measure gc "$JJ" util gc
git fsck --connectivity-only >/dev/null 2>&1 && echo "$LABEL fsck ok" || echo "$LABEL fsck FAILED"
echo "$LABEL pack: $(git count-objects -vH | awk '/size-pack/{print $2, $3}')"
