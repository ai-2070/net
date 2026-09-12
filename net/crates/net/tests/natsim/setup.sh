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
#   cone      — static 1:1 `snat to <pub>:<port>` for the joiner's own
#               port, giving genuinely endpoint-independent mapping
#               (the same public port for every destination) →
#               classifies Cone; filtering stays conntrack-based
#               (address-restricted), the realistic punch-needing case.
#               Plus an INPUT drop for unsolicited inbound on that port,
#               which is both what a restricted NAT does and what keeps
#               a simultaneous punch from poisoning the port mapping.
#               See `one_side` for why `masquerade persistent` alone is
#               NOT a cone NAT.
#   symmetric — `masquerade fully-random`: a fresh public port per
#               connection tuple → classifies Symmetric.
#   none      — the joiner is expected to run publicly in nsim_wan
#               instead; no gateway/ns is created for that side.
#
# --drop-direct installs forward-hook drops on BOTH gateways for UDP
# addressed directly at the other side's public IP — kills punch
# trains and punched paths while leaving everything via R/X intact.
#
# --rtc-port-a <port> additionally pins side A's SECOND (WebRTC) UDP
# socket to the same port on the gateway's public address, with the
# same INPUT drop. That is what lets a NAT'd anchor advertise an
# `rtc_addr` an outside client can actually reach: without the pin the
# RTC socket's mapping is whatever `masquerade` picks, which nobody
# can know in advance. Requires `--nat-a cone` — a symmetric NAT has
# no stable mapping to advertise, which is the point of symmetric.
#
# Requires root. Idempotent-ish: always run teardown.sh first.
set -euo pipefail

NAT_A="cone"
NAT_B="cone"
DROP_DIRECT=0
PUBLIC_B=0
RTC_PORT_A=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --nat-a) NAT_A="$2"; shift 2 ;;
    --nat-b) NAT_B="$2"; shift 2 ;;
    --drop-direct) DROP_DIRECT=1; shift ;;
    --rtc-port-a) RTC_PORT_A="$2"; shift 2 ;;
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
# `--rtc-port-a` pins a 1:1 mapping for side A's RTC socket; only the
# cone gateway installs pinned rules at all, and a symmetric NAT
# deliberately has no advertisable mapping. Validate before anything
# is provisioned rather than building a topology whose anchor
# advertises an address its own gateway never produces.
if [[ -n "$RTC_PORT_A" ]]; then
  if [[ ! "$RTC_PORT_A" =~ ^[0-9]+$ ]] || (( RTC_PORT_A < 1 || RTC_PORT_A > 65535 )); then
    echo "--rtc-port-a wants a UDP port (1-65535), got '$RTC_PORT_A'" >&2
    exit 2
  fi
  if [[ "$NAT_A" != "cone" ]]; then
    echo "--rtc-port-a requires --nat-a cone (got --nat-a $NAT_A)" >&2
    exit 2
  fi
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

# one_side <letter> <gw_pub_ip> <lan_subnet> <nat_mode> <joiner_port>
#          [rtc_port]
one_side() {
  local L="$1" PUB="$2" LAN="$3" MODE="$4" PORT="$5" RTC="${6:-}"
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

  if [[ "$MODE" == "symmetric" ]]; then
    # `fully-random` scrambles the public port per destination tuple —
    # a genuine symmetric NAT, and the punch is expected to fail.
    ip netns exec "$GW" nft -f - <<EOF
table ip nat {
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "gw$L-wan" masquerade fully-random
  }
}
EOF
    return 0
  fi

  # Cone. Two rules, and both are load-bearing — plain
  # `masquerade persistent` does NOT produce a cone NAT, which is what
  # made `cone_cone_punch` fail intermittently:
  #
  # 1. `snat to $PUB:$PORT` for the joiner's own source port pins the
  #    mapping. `persistent` pins the source *address*, not the port;
  #    port preservation is only netfilter's best-effort heuristic and
  #    it yields under tuple collision. Under collision the gateway
  #    silently allocated a fresh public port (observed: 45573) while
  #    the node had already advertised :$PORT through the rendezvous,
  #    i.e. the "cone" NAT degraded to symmetric mid-punch — under
  #    exactly the simultaneous-send condition the test exists to
  #    exercise.
  #
  # 2. The input drop is what removes the collision. When both ends
  #    fire at the synchronized `fire_at`, the peer's keep-alive can
  #    reach this gateway *before* the local outbound leaves. Addressed
  #    to the gateway's own public IP with no mapping yet, it lands in
  #    INPUT — and conntrack records it as a flow, claiming the tuple
  #    the local outbound then needs. Dropping it at filter priority,
  #    before the confirm hook at the end of the chain, means the entry
  #    is never inserted: the tuple stays free and the outbound
  #    preserves its port.
  #
  # Restricted-cone semantics are preserved, deliberately. Dropping the
  # unsolicited inbound is exactly what an address-restricted NAT does,
  # and there is no DNAT here — the peer becomes reachable only once
  # this side's own outbound has opened the mapping. A static DNAT
  # would also fix the race, but it would make the gateway full-cone:
  # the peer would be reachable unsolicited and `cone_cone_punch` would
  # pass without any hole being punched, which is not a test.
  #
  # Once the mapping exists, the peer's keep-alives match it as replies,
  # are un-NAT'd in prerouting and traverse FORWARD — never INPUT — so
  # this rule cannot drop legitimate punched traffic.
  #
  # The RTC socket (--rtc-port-<side>, when given) is pinned exactly
  # the same way, and for the same reason: it is a SECOND socket on an
  # ephemeral port by default, so its mapping would be whatever
  # `masquerade` picked — unknowable in advance and therefore
  # un-advertisable. An anchor can only publish an `rtc_addr` a client
  # can use if the gateway maps that port 1:1. The INPUT drop is
  # equally load-bearing here: the outside client's first ICE checks
  # arrive before the anchor's own outbound has opened the mapping,
  # and a conntrack entry created from that inbound packet would claim
  # the very tuple the pinned SNAT then needs. Dropping it keeps the
  # NAT address-restricted — the client becomes reachable only after
  # the anchor's own check leaves — which is the realistic case.
  #
  # The two RTC lines are built as variables rather than inlined with
  # `${RTC:+...}`: that expansion performs quote removal on its word,
  # so the interface name would reach nft unquoted (`oifname gwa-wan`).
  # A variable's value expanded in a heredoc keeps its quotes verbatim.
  local RTC_SNAT="" RTC_DROP=""
  if [[ -n "$RTC" ]]; then
    RTC_SNAT="oifname \"gw$L-wan\" udp sport $RTC snat to $PUB:$RTC"
    RTC_DROP="iifname \"gw$L-wan\" udp dport $RTC ct state new drop"
  fi
  ip netns exec "$GW" nft -f - <<EOF
table ip nat {
  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
    oifname "gw$L-wan" udp sport $PORT snat to $PUB:$PORT
    $RTC_SNAT
    oifname "gw$L-wan" masquerade persistent
  }
}
table ip filter {
  chain input {
    type filter hook input priority filter; policy accept;
    iifname "gw$L-wan" udp dport $PORT ct state new drop
    $RTC_DROP
  }
}
EOF
}

# The joiner ports are fixed by `run_scenario.sh` (a binds :7001, b
# binds :7002), which is what lets the cone gateways pin a 1:1 port
# mapping instead of gambling on netfilter's port-preservation
# heuristic. Keep these in sync with the `--bind` flags there.
# `--rtc-port-a` pins side A's *second* (RTC) socket the same way;
# `run_scenario.sh` passes the same port to the helper's `--rtc-bind`
# and `--rtc-public`.
one_side a 10.99.0.2 192.168.101 "$NAT_A" 7001 "$RTC_PORT_A"
one_side b 10.99.0.3 192.168.102 "$NAT_B" 7002

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

echo "natsim: topology up (nat_a=$NAT_A nat_b=$NAT_B drop_direct=$DROP_DIRECT public_b=$PUBLIC_B rtc_port_a=${RTC_PORT_A:-none})"
