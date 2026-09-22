#!/usr/bin/env bash
# natsim enrollment row R1c: the DIRECT path with no manual router
# configuration and no relay anywhere in the topology.
#
#   nsim_wan (10.99.0.0/24)            .12 = the cloud agent (public)
#        |                             .10 / .11 exist as addresses only:
#   nsim_gwa .2 / public 11.99.0.2  (upnp mode + miniupnpd)
#                                       NO relay or helper process runs
#        |
#   nsim_a 192.168.101.2               the device (`up --enroll`)
#
# The gateway forwards nothing by itself (setup.sh --nat-a upnp). The device
# runs `net-mesh up --enroll` with defaults; it must ask the router for its
# TCP (enrollment) and UDP (mesh) mappings over NAT-PMP / PCP / UPnP. The agent
# then runs `net-mesh join` and `net-mesh up` from the WAN side and must attach
# DIRECTLY: the only forwarding is the router's own DNAT that miniupnpd
# installed on request, not any application-level relay.
#
# Negative control first: the same topology with `--no-port-mapping` and the
# public address forced into the token; the join must FAIL, proving the NAT
# really blocks the device without the mapping.
#
# Requires root, nftables, iproute2, conntrack, jq and miniupnpd (nftables
# backend). NET_MESH_BIN points at the built `net-mesh` binary.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NATSIM="$(cd "$HERE/.." && pwd)"
BIN="${NET_MESH_BIN:?set NET_MESH_BIN to the net-mesh binary}"
MINIUPNPD="${MINIUPNPD_BIN:-$(command -v miniupnpd || echo /usr/sbin/miniupnpd)}"
STATE="$(mktemp -d /tmp/natsim-enroll.XXXXXX)"
DEVICE_BIND=192.168.101.2:7001
AGENT_BIND=10.99.0.12:7002
# The upnp router's public alias (setup.sh): miniupnpd refuses RFC1918 ext_ip.
PUBLIC_A=11.99.0.2
PIDS=()

log() { echo "[enroll] $*" | tee -a "$STATE/scenario.log" >&2; }
fail() {
  log "FAIL: $*"
  jq -n --arg reason "$*" '{verdict: "fail", reason: $reason}' > "$STATE/verdict.json"
  exit 1
}

in_a() { ip netns exec nsim_a "$@"; }
in_wan() { ip netns exec nsim_wan "$@"; }
in_gw() { ip netns exec nsim_gwa "$@"; }

evidence() {
  in_gw nft list ruleset > "$STATE/$1.gw-ruleset.txt" 2>&1 || true
  in_gw conntrack -L > "$STATE/$1.gw-conntrack.txt" 2>&1 || \
    in_gw cat /proc/net/nf_conntrack > "$STATE/$1.gw-conntrack.txt" 2>&1 || true
}

cleanup() {
  set +e
  evidence final
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
  sleep 0.5
  "$NATSIM/teardown.sh"
  chmod -R a+rX "$STATE" 2>/dev/null
  log "state and evidence in $STATE"
}
trap cleanup EXIT

: > "$STATE/config.toml"
chmod 600 "$STATE/config.toml"

# nm <runner> <state-dir> <args...>: one net-mesh invocation, JSON output.
nm() {
  local runner="$1" st="$2"
  shift 2
  $runner env -u NET_MESH_CONFIG -u NET_MESH_PROFILE \
    "$BIN" --config "$STATE/config.toml" --output json "$@" --state-dir "$st"
}

# start_up <name> <runner> <state-dir> <up args...>: background `up`, wait for
# its readiness row, leave it running.
start_up() {
  local name="$1" runner="$2" st="$3"
  shift 3
  $runner env -u NET_MESH_CONFIG -u NET_MESH_PROFILE \
    "$BIN" --config "$STATE/config.toml" --output ndjson up "$@" --state-dir "$st" \
    > "$STATE/$name.out" 2> "$STATE/$name.err" &
  local pid=$!
  PIDS+=("$pid")
  for _ in $(seq 1 200); do
    if grep -q '"event":"ready"' "$STATE/$name.out" 2>/dev/null; then
      head -n 1 "$STATE/$name.out" > "$STATE/$name.ready.json"
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || fail "$name exited before ready: $(tail -n 20 "$STATE/$name.err")"
    sleep 0.1
  done
  fail "$name never became ready: $(tail -n 20 "$STATE/$name.err")"
}

stop_up() { # stop_up <runner> <state-dir>
  nm "$1" "$2" down > /dev/null || fail "down failed for $2"
}

# ---- topology + router -------------------------------------------------------
"$NATSIM/teardown.sh"
"$NATSIM/setup.sh" --nat-a upnp --nat-b none --public-b | tee -a "$STATE/scenario.log"

# miniupnpd's nftables backend writes into tables/chains named in its config;
# create them with hooks so its rules take effect, nothing else.
in_gw nft -f - <<'EOF'
table inet miniupnpd {
  chain forward { }
  chain prerouting { }
  chain postrouting { }
  chain forward_hook {
    type filter hook forward priority 0; policy accept;
    jump forward
  }
  chain prerouting_hook {
    type nat hook prerouting priority -100; policy accept;
    iifname "gwa-wan" jump prerouting
  }
  chain postrouting_hook {
    type nat hook postrouting priority 100; policy accept;
    jump postrouting
  }
}
EOF
cat > "$STATE/miniupnpd.conf" <<EOF
ext_ifname=gwa-wan
listening_ip=gwa-lan
ext_ip=$PUBLIC_A
port=0
enable_natpmp=yes
enable_pcp_pmp=yes
enable_upnp=yes
secure_mode=yes
system_uptime=yes
uuid=5f0e8b3a-1c2d-4e5f-8a9b-0c1d2e3f4a5b
upnp_table_name=miniupnpd
upnp_nat_table_name=miniupnpd
upnp_forward_chain=forward
upnp_nat_chain=prerouting
upnp_nat_postrouting_chain=postrouting
allow 1024-65535 192.168.101.0/24 1024-65535
deny 0-65535 0.0.0.0/0 0-65535
EOF
in_gw "$MINIUPNPD" -d -f "$STATE/miniupnpd.conf" > "$STATE/miniupnpd.log" 2>&1 &
PIDS+=("$!")
for _ in $(seq 1 50); do
  in_gw ss -uln 2>/dev/null | grep -q ':5351 ' && break
  sleep 0.1
done
in_gw ss -uln 2>/dev/null | grep -q ':5351 ' || fail "miniupnpd is not listening for NAT-PMP: $(tail -n 20 "$STATE/miniupnpd.log")"
log "router up: masquerade only, miniupnpd listening"

# ---- negative control: no mapping, the device is unreachable -------------------
start_up neg-device in_a "$STATE/neg-device" --enroll --no-port-mapping --bind "$DEVICE_BIND"
jq -e '.enrollment.port_mapping == "disabled"' "$STATE/neg-device.ready.json" > /dev/null \
  || fail "negative control did not disable mapping"
neg_token=$(nm in_a "$STATE/neg-device" invite create --addr "$PUBLIC_A:7001" | jq -r .token)
set +e
nm in_wan "$STATE/neg-agent" join "$neg_token" --yes --wait 5s > "$STATE/neg-join.out" 2> "$STATE/neg-join.err"
neg_code=$?
set -e
[[ "$neg_code" != 0 ]] || fail "negative control: the device was joinable WITHOUT a router mapping"
log "negative control: join without mapping failed as required (exit $neg_code)"
stop_up in_a "$STATE/neg-device"

# ---- the direct path -------------------------------------------------------------
start_up device in_a "$STATE/device" --enroll --bind "$DEVICE_BIND"
jq -e '.enrollment.port_mapping == "active"' "$STATE/device.ready.json" > /dev/null \
  || fail "port mapping not active: $(jq -c .enrollment "$STATE/device.ready.json")"
endpoint=$(jq -r .enrollment.endpoint "$STATE/device.ready.json")
mapped_tcp=$(jq -r .enrollment.mapped_tcp "$STATE/device.ready.json")
mapped_udp=$(jq -r .enrollment.mapped_udp "$STATE/device.ready.json")
[[ "$endpoint" == "$mapped_tcp" ]] || fail "token endpoint $endpoint is not the mapped address $mapped_tcp"
[[ "$mapped_tcp" == "$PUBLIC_A:"* && "$mapped_udp" == "$PUBLIC_A:"* ]] \
  || fail "mappings are not on the router's public address: tcp=$mapped_tcp udp=$mapped_udp"
evidence mapped
grep -q "192.168.101.2" "$STATE/mapped.gw-ruleset.txt" \
  || fail "no DNAT to the device in the router's ruleset after mapping"
log "router mapped tcp=$mapped_tcp udp=$mapped_udp on request"

token=$(nm in_a "$STATE/device" invite create | jq -r .token)
nm in_wan "$STATE/agent" join "$token" --yes > "$STATE/join.json" 2> "$STATE/join.err" \
  || fail "agent join failed: $(cat "$STATE/join.err")"
jq -e '.state == "joined" and .attached == true' "$STATE/join.json" > /dev/null \
  || fail "join did not attach: $(cat "$STATE/join.json")"
[[ "$(jq -r .contact "$STATE/join.json")" == "$mapped_udp" ]] \
  || fail "bundle contact $(jq -r .contact "$STATE/join.json") is not the mapped UDP address $mapped_udp"

start_up agent in_wan "$STATE/agent" --bind "$AGENT_BIND"
jq -e '.joined.attached == true and .psk_source == "joined"' "$STATE/agent.ready.json" > /dev/null \
  || fail "joined agent did not attach: $(jq -c .joined "$STATE/agent.ready.json")"

# Direct evidence: the agent's flows crossed the router's DNAT straight to the
# device's own sockets (reply source = the device's private address).
evidence joined
grep -E "src=10\.99\.0\.12 .*dst=$PUBLIC_A .*src=192\.168\.101\.2" "$STATE/joined.gw-conntrack.txt" \
  | grep -q "tcp" || fail "no DNAT'd TCP flow from the agent to the device in conntrack"
grep -E "src=10\.99\.0\.12 .*dst=$PUBLIC_A .*src=192\.168\.101\.2" "$STATE/joined.gw-conntrack.txt" \
  | grep -q "udp" || fail "no DNAT'd UDP flow from the agent to the device in conntrack"
log "agent reached the device directly through the router's mapping"

stop_up in_wan "$STATE/agent"
stop_up in_a "$STATE/device"
sleep 1
evidence stopped
if grep -Eq "dnat to 192\.168\.101\.2:7001" "$STATE/stopped.gw-ruleset.txt"; then
  fail "mappings still installed after the device stopped"
fi
log "mappings removed on shutdown"

jq -n --arg tcp "$mapped_tcp" --arg udp "$mapped_udp" \
  '{verdict: "pass", mapped_tcp: $tcp, mapped_udp: $udp, relay: "none in topology"}' \
  > "$STATE/verdict.json"
log "PASS"
