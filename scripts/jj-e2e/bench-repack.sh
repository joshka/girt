#!/usr/bin/env bash
# Compare girt and Git repacking byte-identical disposable benchmark clones.
# Build examples/repack_repository in release mode first. The caller must hold
# /tmp/girt-timing.lock, as for bench-large.sh. Git is only an executable oracle.
# Usage: bench-repack.sh <repack_repository binary> <work directory> [repository URL]
set -euo pipefail
REPACK=$1
WORK=$2
URL=${3:-https://github.com/jj-vcs/jj}
mkdir -p "$WORK"
git -c commit.gpgsign=false clone --mirror "$URL" "$WORK/source" >"$WORK/clone.out" 2>"$WORK/clone.err"
cp -R "$WORK/source" "$WORK/girt"
cp -R "$WORK/source" "$WORK/git"
pack_bytes() {
    find "$1/objects/pack" -name '*.pack' -exec stat -f '%z' {} + | awk '{sum += $1} END {print sum+0}'
}
printf 'original pack_bytes=%s\n' "$(pack_bytes "$WORK/source")"
/usr/bin/time -l "$REPACK" "$WORK/girt" >"$WORK/girt.out" 2>"$WORK/girt.err"
/usr/bin/time -l git -C "$WORK/git" repack -ad >"$WORK/git.out" 2>"$WORK/git.err"
for label in girt git; do
    real=$(awk '/ real /{print $1}' "$WORK/$label.err")
    rss=$(awk '/maximum resident set size/{printf "%.2f", $1/1048576}' "$WORK/$label.err")
    printf '%s real=%ss rss=%sMiB pack_bytes=%s\n' "$label" "$real" "$rss" "$(pack_bytes "$WORK/$label")"
    git -C "$WORK/$label" fsck --full >"$WORK/$label.fsck" 2>&1
    for index in "$WORK/$label/objects/pack/"*.idx; do
        git -C "$WORK/$label" verify-pack "$index"
    done
    echo "$label fsck and verify-pack ok"
done
cat "$WORK/girt.out"
