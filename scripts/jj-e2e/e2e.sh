#!/usr/bin/env bash
# End-to-end interoperability check for jj built on girt.
#
# Alternates jj and Git operations on the same repositories for each object format (SHA-1,
# SHA-256) and transport (local path, file://, smart HTTP, SSH), then checks the results with Git.
# Git is used only as the independent oracle and as the remote server.
#
# Usage: e2e.sh <path-to-jj> [work-dir]

set -euo pipefail

JJ=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
WORK=${2:-$(mktemp -d)}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd -P)

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=Git GIT_AUTHOR_EMAIL=git@example.com
export GIT_COMMITTER_NAME=Git GIT_COMMITTER_EMAIL=git@example.com
export JJ_CONFIG="$WORK/jj-config.toml"
cat >"$JJ_CONFIG" <<'EOF'
[user]
name = "JJ User"
email = "jj@example.com"
[ui]
paginate = "never"
[git]
colocate = true
EOF

failures=0
checks=0
check() {
    local description=$1
    shift
    checks=$((checks + 1))
    if "$@" >/dev/null 2>&1; then
        echo "  ok   $description"
    else
        echo "  FAIL $description"
        failures=$((failures + 1))
    fi
}

equal() { [ "$1" = "$2" ]; }

# A stand-in for ssh that runs the remote command locally, like an SSH server would.
FAKE_SSH="$WORK/fake-ssh"
cat >"$FAKE_SSH" <<'EOF'
#!/bin/sh
while [ $# -gt 1 ]; do shift; done
exec sh -c "$1"
EOF
chmod +x "$FAKE_SSH"

HTTP_PID=
AUTH_PID=
cleanup() {
    [ -n "$HTTP_PID" ] && kill "$HTTP_PID" 2>/dev/null || true
    [ -n "$AUTH_PID" ] && kill "$AUTH_PID" 2>/dev/null || true
}
trap cleanup EXIT

start_server() {
    local root=$1 port_file=$2
    shift 2
    rm -f "$port_file"
    python3 "$HERE/http_server.py" "$root" "$port_file" "$@" >"$port_file.log" 2>&1 &
    local pid=$!
    for _ in $(seq 50); do [ -s "$port_file" ] && break; sleep 0.1; done
    echo "$pid"
}

for format in sha1 sha256; do
    echo "== object format: $format"
    export JJ_CONFIG_FORMAT="$WORK/jj-config-$format.toml"
    printf '[git]\nobject-hash = "%s"\n' "$format" >"$JJ_CONFIG_FORMAT"
    export JJ_CONFIG="$WORK/jj-config.toml:$JJ_CONFIG_FORMAT"
    base="$WORK/$format"
    rm -rf "$base"
    mkdir -p "$base"
    git init -q --bare --object-format="$format" "$base/origin.git"
    git -C "$base/origin.git" symbolic-ref HEAD refs/heads/main
    git -C "$base/origin.git" config http.receivepack true
    git init -q --object-format="$format" -b main "$base/seed"
    echo one >"$base/seed/file"
    git -C "$base/seed" add file
    git -C "$base/seed" commit -q -m one
    git -C "$base/seed" tag -a v1 -m "version 1"
    git -C "$base/seed" remote add origin "$base/origin.git"
    git -C "$base/seed" push -q origin main v1

    HTTP_PID=$(start_server "$base" "$base/http-port")
    port=$(cat "$base/http-port")

    for transport in local file http ssh; do
        echo "-- transport: $transport"
        case $transport in
        local) url="$base/origin.git" ;;
        file) url="file://$base/origin.git" ;;
        http) url="http://127.0.0.1:$port/origin.git" ;;
        ssh) url="ssh://example.invalid$base/origin.git" ;;
        esac
        export GIT_SSH_COMMAND="$FAKE_SSH"
        clone="$base/clone-$transport"

        check "clone" "$JJ" git clone "$url" "$clone"
        cd "$clone"
        main=$(git --git-dir="$base/origin.git" rev-parse main)
        check "git HEAD is the remote default branch" equal "$(git rev-parse HEAD)" "$main"
        check "git sees a clean working tree" equal "$(git status --porcelain)" ""
        check "clone has the remote's file" \
            equal "$(cat file)" "$(git --git-dir="$base/origin.git" show main:file)"
        check "tag imported" sh -c "'$JJ' tag list | grep -q v1"

        echo "two-$transport" >file
        "$JJ" commit -m "jj commit via $transport" >/dev/null 2>&1
        "$JJ" bookmark move main --to @- >/dev/null 2>&1
        check "push" "$JJ" git push
        pushed=$("$JJ" log -r main --no-graph -T commit_id)
        check "remote main is the pushed commit" \
            equal "$(git --git-dir="$base/origin.git" rev-parse main)" "$pushed"
        check "remote passes fsck" git --git-dir="$base/origin.git" fsck --strict
        check "git sees remote-tracking ref" equal "$(git rev-parse refs/remotes/origin/main)" "$pushed"

        # Another client pushes with Git; jj fetches.
        git -C "$base/seed" pull -q origin main
        echo "three-$transport" >"$base/seed/file"
        git -C "$base/seed" commit -q -am "git commit via $transport"
        git -C "$base/seed" push -q origin main
        check "fetch" "$JJ" git fetch
        check "fetched Git's commit" \
            equal "$("$JJ" log -r main@origin --no-graph -T commit_id)" \
            "$(git -C "$base/seed" rev-parse HEAD)"

        # Create and delete a remote bookmark.
        "$JJ" new main@origin -m feature >/dev/null 2>&1
        "$JJ" bookmark create "feature-$transport" -r @ >/dev/null 2>&1
        check "push new bookmark" "$JJ" git push -b "feature-$transport"
        check "remote has new bookmark" git --git-dir="$base/origin.git" rev-parse "feature-$transport"
        "$JJ" bookmark delete "feature-$transport" >/dev/null 2>&1
        check "push deletion" "$JJ" git push --deleted
        check "remote bookmark deleted" \
            sh -c "! git --git-dir='$base/origin.git' rev-parse --verify -q 'refs/heads/feature-$transport'"

        # Colocated Git commits are imported by jj.
        git checkout -q -b "git-side-$transport" main@origin 2>/dev/null ||
            git checkout -q -b "git-side-$transport" "$(git rev-parse refs/remotes/origin/main)"
        echo "git-side" >git-file
        git add git-file
        git commit -q -m "colocated git commit"
        git_commit=$(git rev-parse HEAD)
        check "jj imports colocated Git commit" \
            equal "$("$JJ" log -r "git-side-$transport" --no-graph -T commit_id)" "$git_commit"

        # Workspaces are Git worktrees.
        check "workspace add" "$JJ" workspace add "$base/ws-$transport"
        check "git lists the worktree" sh -c "git worktree list | grep -q 'ws-$transport'"
        check "worktree git status works" git -C "$base/ws-$transport" status --porcelain
        check "workspace forget" "$JJ" workspace forget "ws-$transport"
        check "git no longer lists the worktree" \
            sh -c "! git worktree list | grep -q 'ws-$transport'"

        check "gc" "$JJ" util gc --expire=now
        check "clone passes fsck after gc" git fsck --strict
        check "jj still reads the repo after gc" "$JJ" log -r 'all()'
        cd "$WORK"
    done

    # Authenticated HTTP through a credential helper.
    echo "-- transport: http with credentials"
    AUTH_PID=$(start_server "$base" "$base/auth-port" "user:secret")
    auth_port=$(cat "$base/auth-port")
    helper="$WORK/credential-helper"
    cat >"$helper" <<'EOF'
#!/bin/sh
[ "$1" = get ] && printf 'username=user\npassword=%s\n' "${TEST_PASSWORD:-secret}"
exit 0
EOF
    chmod +x "$helper"
    export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=credential.helper GIT_CONFIG_VALUE_0="$helper"
    check "clone with credential helper" \
        "$JJ" git clone "http://127.0.0.1:$auth_port/origin.git" "$base/clone-auth"
    check "wrong password is rejected" \
        sh -c "! TEST_PASSWORD=wrong '$JJ' git clone 'http://127.0.0.1:$auth_port/origin.git' '$base/clone-auth-wrong'"
    unset GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0
    kill "$AUTH_PID" "$HTTP_PID" 2>/dev/null || true
    AUTH_PID=
    HTTP_PID=
done

echo "$checks checks, $failures failures"
[ "$failures" -eq 0 ]
