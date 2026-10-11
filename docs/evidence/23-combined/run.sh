#!/bin/zsh
set -eu
export PATH="$HOME/.cargo/bin:$PATH"
unset RUSTUP_TOOLCHAIN
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
trap 'rm -f /tmp/girt-timing.lock' EXIT
uptime | tee /tmp/23-combined-bench/uptime-start.txt
python3 /tmp/23-combined-bench/measure.py
uptime | tee /tmp/23-combined-bench/uptime-end.txt
