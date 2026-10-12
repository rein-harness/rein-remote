#!/bin/bash
# Every running `rein-remote serve` (started by Rein): its arguments, and its /stats, read with the
# token Rein gave it (from its environment) on the loopback port it listens on.
for pid in $(pgrep -f '^/root/.rein/bin/rein-remote serve'); do
  args=$(tr '\0' ' ' < /proc/$pid/cmdline)
  token=$(tr '\0' '\n' < /proc/$pid/environ | sed -n 's/^REIN_REMOTE_TOKEN=//p')
  port=$(ss -ltnpH | grep "pid=$pid," | awk '{print $4}' | sed 's/.*://' | head -1)
  stats=$(curl -s -m 2 "http://127.0.0.1:$port/stats?token=$token")
  printf '{"t":%s,"pid":%s,"args":%s,"port":%s,"stats":%s}\n' "$(date +%s.%N)" "$pid" "$(jq -Rn --arg a "$args" '$a')" "${port:-null}" "${stats:-null}"
done
