#!/bin/sh
set -eu
until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done
trap 'rm -f /tmp/girt-timing.lock' EXIT
uptime > /tmp/astra-perf-evidence/uptime-before.txt
for item in stock52:1 full52:1 full52:2 stock52:2; do
  variant=${item%:*}
  round=${item#*:}
  fixture="/tmp/astra-perf-evidence/$variant-$round.git"
  cp -cR /tmp/opus-med-23/source.git "$fixture"
  /usr/bin/time -l -o "/tmp/astra-perf-evidence/$variant-$round.time" "/tmp/astra-perf-evidence/$variant" repack "$fixture" > "/tmp/astra-perf-evidence/$variant-$round.out" 2>&1
  echo "Completed $variant run $round"
done
uptime > /tmp/astra-perf-evidence/uptime-after.txt
