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

# Validate BEFORE touching any namespace, so a typo'd mode fails
# loudly instead of silently building a cone topology (everything
# unrecognized used to fall through to the cone masquerade).
for mode in "$NAT_A" "$NAT_B"; do
  case "$mode" in
    cone|symmetric|none) ;;
    *) echo "invalid NAT mode: '$mode' (want cone|symmetric|none)" >&2; exit 2 ;;
  esac
done
# --public-b replaces side B's NAT'd joiner with a public address on
# the bridge; combining it with a NAT'd B would build a contradictory
# topology (both a public B address AND a private NAT'd B namespace).
if [[ "$PUBLIC_B" == 1 && "$NAT_B" != "none" ]]; then
  echo "--public-b requires --nat-b none (got --nat-b $NAT_B)" >&2
  exit 2
fi

WAN=nsim_wan

# `ip netns add` registers namespaces at /var/run/netns/<name>;
# used to guard optional per-gateway steps (a side with NAT mode
# `none` never creates its gateway namespace).
netns_exists() { [[ -e "/var/run/netns/$1" ]]; }

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
  # Drop UDP routed straight between the two sides' public addresses,
  # on each gateway's forward hook. Traffic via R (10.99.0.10) /
  # X (.11) is untouched, so introduce/ack forwarding and the routed
  # fallback keep working — only the direct trains + punched path die.
  #
  # A side with NAT mode `none` has no gateway namespace (cubic P1:
  # this used to `ip netns exec` unconditionally and crash under
  # `set -e`). Guard each install on the namespace existing; a
  # one-sided drop still kills the punch — the other side's observer
  # never sees a train, so no ack is ever emitted — and kills direct
  # handshakes in that direction.
  if netns_exists nsim_gwa; then
    ip netns exec nsim_gwa nft -f - <<'EOF'
table ip filter {
  chain forward {
    type filter hook forward priority 0; policy accept;
    ip daddr 10.99.0.3 meta l4proto udp drop
    ip daddr 10.99.0.12 meta l4proto udp drop
  }
}
EOF
  fi
  if netns_exists nsim_gwb; then
    ip netns exec nsim_gwb nft -f - <<'EOF'
table ip filter {
  chain forward {
    type filter hook forward priority 0; policy accept;
    ip daddr 10.99.0.2 meta l4proto udp drop
  }
}
EOF
  fi
  if ! netns_exists nsim_gwa && ! netns_exists nsim_gwb; then
    echo "--drop-direct with no NAT gateway on either side has nothing to drop" >&2
    exit 2
  fi
fi

echo "natsim: topology up (nat_a=$NAT_A nat_b=$NAT_B drop_direct=$DROP_DIRECT public_b=$PUBLIC_B)"
