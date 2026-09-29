#!/usr/bin/env bash
# Remove every natsim namespace (veths die with their namespaces,
# nft tables die with their netns). Safe to run when nothing is up.
set -uo pipefail
# The 464XLAT translators (tayga) are daemons running INSIDE the
# namespaces, and a process keeps its namespace alive after `ip netns
# del`: stop them first, or they outlive the row and the next row's
# namespaces collide with them.
for pidfile in /tmp/natsim-tayga-*/tayga.pid; do
  [[ -f "$pidfile" ]] || continue
  kill "$(cat "$pidfile")" 2>/dev/null || true
  rm -f "$pidfile"
done
for ns in nsim_a nsim_b nsim_gwa nsim_gwb nsim_wan; do
  ip netns del "$ns" 2>/dev/null || true
done
exit 0
