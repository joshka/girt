#!/usr/bin/env bash
# Runs jj's Git workflows with every `git` program on PATH replaced by a tripwire that logs and
# fails. Any log entry means jj (or girt) executed Git.
#
# Covers init, commit, local and file:// clone, push, fetch, remotes, workspaces, export, import,
# GC and log, in both object formats. Network transports and credential helpers aren't exercised.
#
# Usage: no-git.sh <path-to-jj>
set -u
JJ=$1
WORK=$(mktemp -d)
TRIP="$WORK/trip-bin"
LOG="$WORK/git-calls.log"
mkdir -p "$TRIP"
: >"$LOG"
# Shadow git and every git-* helper found on the real PATH.
for name in git $(compgen -c git- | sort -u); do
    printf '#!/bin/sh\necho "%s $*" >>"%s"\nexit 97\n' "$name" "$LOG" >"$TRIP/$name"
    chmod +x "$TRIP/$name"
done
# Keep only non-Git tools: system dirs, with the tripwires first. Drop any dir holding real git.
SAFE_PATH="$TRIP"
for dir in /usr/bin /bin /usr/sbin /sbin; do SAFE_PATH="$SAFE_PATH:$dir"; done
export PATH="$SAFE_PATH"
export HOME="$WORK/home" JJ_CONFIG="$WORK/jj.toml"
mkdir -p "$HOME"
printf '[user]\nname = "T"\nemail = "t@example.com"\n[ui]\npaginate = "never"\n' >"$JJ_CONFIG"
fail=0
run() {
    if ! "$@" >>"$WORK/out.log" 2>&1; then
        echo "FAIL: $*"
        fail=1
    fi
}
cd "$WORK"
for format in sha1 sha256; do
    base="$WORK/$format"
    mkdir -p "$base"
    printf '[git]\nobject-hash = "%s"\n' "$format" >"$WORK/fmt.toml"
    export JJ_CONFIG="$WORK/jj.toml:$WORK/fmt.toml"
    run "$JJ" git init --colocate "$base/origin"
    cd "$base/origin"
    echo one >file
    run "$JJ" commit -m one
    run "$JJ" bookmark create main -r @-
    cd "$base"
    run "$JJ" git clone "$base/origin" "$base/clone"
    cd "$base/clone"
    echo two >file
    run "$JJ" commit -m two
    run "$JJ" bookmark move main --to @-
    run "$JJ" git push -b main
    run "$JJ" git fetch
    run "$JJ" git remote add other "file://$base/origin"
    run "$JJ" git fetch --remote other
    run "$JJ" git remote list
    run "$JJ" workspace add "$base/ws"
    run "$JJ" workspace forget ws
    run "$JJ" git export
    run "$JJ" git import
    run "$JJ" util gc --expire=now
    run "$JJ" log -r 'all()'
    cd "$WORK"
done
echo "git invocations: $(wc -l <"$LOG" | tr -d ' ')"
cat "$LOG"
[ "$fail" = 0 ] && echo "all commands succeeded" || tail -20 "$WORK/out.log"
