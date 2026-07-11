#!/usr/bin/env bash
# natsim network provisioning (NAT_TRAVERSAL_V2_PLAN.md Stage 4).
#
# Builds this topology out of network namespaces + nftables:
#
#            nsim_wan ("the internet", 10.99.0.0/24 on br0)
#      .10 = R (relay/coordinator)   .11 = X (aux classify target)
#      .12 = optional public joiner  (--public-b)
#         |                |
#     [gwa-br]        [gwb-br]              (bridge ports)
#         |                |
#      nsim_gwa .2      nsim_gwb .3         (NAT gateways, masquerade)
#         |                |
#   192.168.101.1    192.168.102.1
#         |                |
#      nsim_a .2        nsim_b .2           (joiners behind NAT)
#
# NAT flavor per side (--nat-a / --nat-b):
#   cone      — plain `masquerade persistent`: endpoint-independent
#               mapping (same public port for every destination) →
#               classifies Cone; filtering is conntrack (addr+port
#               restricted), the realistic punch-needing case.
#   symmetric — `masquerade fully-random`: a fresh public port per
#               connection tuple → classifies Symmetric.
#   none      — the joiner is expected to run publicly in nsim_wan
#               instead; no gateway/ns is created for that side.
#
# --drop-direct installs forward-hook drops on BOTH gateways for UDP
# addressed directly at the other side's public IP — kills punch
# trains and punched paths while leaving everything via R/X intact.
#
# Requires root. Idempotent-ish: always run teardown.sh first.
set -euo pipefail

NAT_A="cone"
NAT_B="cone"
DROP_DIRECT=0
PUBLIC_B=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --nat-a) NAT_A="$2"; shift 2 ;;
    --nat-b) NAT_B="$2"; shift 2 ;;
    --drop-direct) DROP_DIRECT=1; shift ;;
    --public-b) PUBLIC_B=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

WAN=nsim_wan

ip netns add "$WAN"
ip -n "$WAN" link set lo up
ip -n "$WAN" link add br0 type bridge
ip -n "$WAN" link set br0 up
ip -n "$WAN" addr add 10.99.0.1/24 dev br0
# R and X live in nsim_wan as extra addresses on the bridge — two
# DISTINCT public IPs, which is what lets the classifier tell cone
# from symmetric (it compares the mappings two destinations observe).
ip -n "$WAN" addr add 10.99.0.10/24 dev br0
ip -n "$WAN" addr add 10.99.0.11/24 dev br0
if [[ "$PUBLIC_B" == 1 ]]; then
  ip -n "$WAN" addr add 10.99.0.12/24 dev br0
fi

# one_side <letter> <gw_pub_ip> <lan_subnet> <nat_mode>
one_side() {
  local L="$1" PUB="$2" LAN="$3" MODE="$4"
  [[ "$MODE" == "none" ]] && return 0
  local GW="nsim_gw$L" NS="nsim_$L"
  ip netns add "$GW"
  ip netns add "$NS"
  ip -n "$GW" link set lo up
  ip -n "$NS" link set lo up

  # gateway ↔ wan bridge
  ip link add "gw$L-wan" netns "$GW" type veth peer name "gw$L-br" netns "$WAN"
  ip -n "$WAN" link set "gw$L-br" master br0 up
  ip -n "$GW" addr add "$PUB/24" dev "gw$L-wan"
  ip -n "$GW" link set "gw$L-wan" up

  # gateway ↔ private lan
  ip link add "gw$L-lan" netns "$GW" type veth peer name eth0 netns "$NS"
  ip -n "$GW" addr add "$LAN.1/24" dev "gw$L-lan"
  ip -n "$GW" link set "gw$L-lan" up
  ip -n "$NS" addr add "$LAN.2/24" dev eth0
  ip -n "$NS" link set eth0 up
  ip -n "$NS" route add default via "$LAN.1"

  ip netns exec "$GW" sysctl -qw net.ipv4.ip_forward=1

  local MASQ="masquerade persistent"
  [[ "$MODE" == "symmetric" ]] && MASQ="masquerade fully-random"
  ip netns exec "$GW" nft -f - <<EOF
table ip nat {
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "gw$L-wan" $MASQ
  }
}
EOF
}

one_side a 10.99.0.2 192.168.101 "$NAT_A"
one_side b 10.99.0.3 192.168.102 "$NAT_B"

if [[ "$DROP_DIRECT" == 1 ]]; then
  # Drop UDP routed straight between the two public mappings, on both
  # gateways' forward hooks. Traffic via R (10.99.0.10) / X (.11) is
  # untouched, so introduce/ack forwarding and the routed fallback
  # keep working — only the direct trains + punched path die.
  ip netns exec nsim_gwa nft -f - <<'EOF'
table ip filter {
  chain forward {
    type filter hook forward priority 0; policy accept;
    ip daddr 10.99.0.3 meta l4proto udp drop
  }
}
EOF
  ip netns exec nsim_gwb nft -f - <<'EOF'
table ip filter {
  chain forward {
    type filter hook forward priority 0; policy accept;
    ip daddr 10.99.0.2 meta l4proto udp drop
  }
}
EOF
fi

echo "natsim: topology up (nat_a=$NAT_A nat_b=$NAT_B drop_direct=$DROP_DIRECT public_b=$PUBLIC_B)"
