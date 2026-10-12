#!/bin/zsh
set -eu
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
trap 'rm -f /tmp/girt-timing.lock' EXIT
uptime | tee /tmp/23-combined-bench/workloads-uptime-start.txt
python3 /tmp/23-combined-bench/workloads.py
python3 /tmp/23-combined-bench/verify.py
uptime | tee /tmp/23-combined-bench/workloads-uptime-end.txt
