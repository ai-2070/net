#!/usr/bin/env bash
# Remove every natsim namespace (veths die with their namespaces,
# nft tables die with their netns). Safe to run when nothing is up.
set -uo pipefail
for ns in nsim_a nsim_b nsim_gwa nsim_gwb nsim_wan; do
  ip netns del "$ns" 2>/dev/null || true
done
exit 0
