#!/usr/bin/env bash
# natsim enrollment rows R2 phase 4: DIRECT FIRST, RELAY FALLBACK, in a real
# NAT lab.
#
#   nsim_wan (10.99.0.0/24)            .10 = the blind relay (`relay serve`)
#        |                             .12 = the cloud agent (public)
#   nsim_gwa .2 / public 11.99.0.2  (upnp mode + miniupnpd)
#        |
#   nsim_a 192.168.101.2               the device (`up --enroll --relay`)
#
# The gateway forwards nothing by itself (setup.sh --nat-a upnp). Three rows,
# each with fresh device and agent state:
#
#   B  relay DOWN, router mapping working: the device still starts (relay
#      reported unavailable, token still names it) and the agent joins
#      DIRECTLY through the mapping. Relay availability is not a prerequisite.
#   C  relay UP, router mapping working: the agent still joins DIRECTLY and the
#      relay splices nothing. Direct first.
#   A  relay UP, direct path forced to fail (no router mapping; the token names
#      the router's public address, which forwards nothing): the agent's join,
#      its attach and its joined `up` all complete THROUGH THE RELAY with no
#      flag on the agent's side. Evidence: every path field says `relay`, the
#      relay's counters show the splice and forwarded datagrams, and the
#      gateway's conntrack shows only the device's OUTBOUND flows to the relay
#      and no inbound flow from the agent to the device.
#
# Requires root, nftables, iproute2, conntrack, jq and miniupnpd (nftables
# backend). NET_MESH_BIN points at the built `net-mesh` binary.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NATSIM="$(cd "$HERE/.." && pwd)"
BIN="${NET_MESH_BIN:?set NET_MESH_BIN to the net-mesh binary}"
MINIUPNPD="${MINIUPNPD_BIN:-$(command -v miniupnpd || echo /usr/sbin/miniupnpd)}"
STATE="$(mktemp -d /tmp/natsim-enroll-relay.XXXXXX)"
DEVICE_BIND=192.168.101.2:7001
AGENT_BIND=10.99.0.12:7002
RELAY=10.99.0.10:3478
PUBLIC_A=11.99.0.2
PIDS=()
RELAY_PID=""

log() { echo "[relay-rows] $*" | tee -a "$STATE/scenario.log" >&2; }
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
  [[ -n "$RELAY_PID" ]] && kill "$RELAY_PID" 2>/dev/null
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
  sleep 0.5
  "$NATSIM/teardown.sh"
  chmod -R a+rX "$STATE" 2>/dev/null
  log "state and evidence in $STATE"
}
trap cleanup EXIT

: > "$STATE/config.toml"
chmod 600 "$STATE/config.toml"

nm() {
  local runner="$1" st="$2"
  shift 2
  $runner env -u NET_MESH_CONFIG -u NET_MESH_PROFILE \
    "$BIN" --config "$STATE/config.toml" --output json "$@" --state-dir "$st"
}

start_up() {
  local name="$1" runner="$2" st="$3"
  shift 3
  $runner env -u NET_MESH_CONFIG -u NET_MESH_PROFILE \
    "$BIN" --config "$STATE/config.toml" --output ndjson up "$@" --state-dir "$st" \
    > "$STATE/$name.out" 2> "$STATE/$name.err" &
  local pid=$!
  PIDS+=("$pid")
  for _ in $(seq 1 400); do
    if grep -q '"event":"ready"' "$STATE/$name.out" 2>/dev/null; then
      head -n 1 "$STATE/$name.out" > "$STATE/$name.ready.json"
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || fail "$name exited before ready: $(tail -n 20 "$STATE/$name.err")"
    sleep 0.1
  done
  fail "$name never became ready: $(tail -n 20 "$STATE/$name.err")"
}

stop_up() { nm "$1" "$2" down > /dev/null || fail "down failed for $2"; }

# start_relay <name>: `relay serve` on the WAN at $RELAY, wait for readiness.
start_relay() {
  in_wan env -u NET_MESH_CONFIG -u NET_MESH_PROFILE \
    "$BIN" --output ndjson relay serve --bind "$RELAY" \
    > "$STATE/$1.out" 2> "$STATE/$1.err" &
  RELAY_PID=$!
  for _ in $(seq 1 100); do
    grep -q '"event":"ready"' "$STATE/$1.out" 2>/dev/null && return 0
    kill -0 "$RELAY_PID" 2>/dev/null || fail "relay exited: $(tail -n 20 "$STATE/$1.err")"
    sleep 0.1
  done
  fail "relay never became ready: $(tail -n 20 "$STATE/$1.err")"
}

# stop_relay <name>: SIGTERM, then the `stopped` row with the relay's counters.
stop_relay() {
  kill -TERM "$RELAY_PID"
  for _ in $(seq 1 50); do
    kill -0 "$RELAY_PID" 2>/dev/null || break
    sleep 0.1
  done
  kill -0 "$RELAY_PID" 2>/dev/null && fail "relay did not stop on SIGTERM"
  RELAY_PID=""
  grep '"event":"stopped"' "$STATE/$1.out" > "$STATE/$1.stopped.json" \
    || fail "relay wrote no stopped row: $(cat "$STATE/$1.out")"
}

# join_as <row>: run `join` from the agent, save the result.
join_as() {
  local row="$1" token="$2"
  nm in_wan "$STATE/$row-agent" join "$token" --yes --wait 30s \
    > "$STATE/$row-join.json" 2> "$STATE/$row-join.err" \
    || fail "row $row: join failed: $(cat "$STATE/$row-join.err")"
  jq -e '.state == "joined" and .attached == true' "$STATE/$row-join.json" > /dev/null \
    || fail "row $row: join did not attach: $(cat "$STATE/$row-join.json")"
}

# ---- topology + router -------------------------------------------------------
"$NATSIM/teardown.sh"
"$NATSIM/setup.sh" --nat-a upnp --nat-b none --public-b | tee -a "$STATE/scenario.log"

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
in_gw ss -uln 2>/dev/null | grep -q ':5351 ' || fail "miniupnpd is not listening: $(tail -n 20 "$STATE/miniupnpd.log")"
log "router up: masquerade only, miniupnpd listening; relay address $RELAY"

# ---- row B: relay down, mapping working -> direct --------------------------------
start_up b-device in_a "$STATE/b-device" --enroll --bind "$DEVICE_BIND" --relay "$RELAY"
jq -e '.enrollment.port_mapping == "active"' "$STATE/b-device.ready.json" > /dev/null \
  || fail "row B: port mapping not active: $(jq -c .enrollment "$STATE/b-device.ready.json")"
jq -e '.enrollment.relay_state == "unavailable"' "$STATE/b-device.ready.json" > /dev/null \
  || fail "row B: relay should be unavailable: $(jq -c .enrollment "$STATE/b-device.ready.json")"
b_token=$(nm in_a "$STATE/b-device" invite create | tee "$STATE/b-create.json" | jq -r .token)
jq -e --arg r "$RELAY" '.relay == $r' "$STATE/b-create.json" > /dev/null \
  || fail "row B: token does not name the relay: $(cat "$STATE/b-create.json")"
join_as b "$b_token"
jq -e '.enroll_path == "direct" and .attach_path == "direct"' "$STATE/b-join.json" > /dev/null \
  || fail "row B: expected the direct path with the relay down: $(cat "$STATE/b-join.json")"
stop_up in_a "$STATE/b-device"
log "row B: relay down -> joined directly"

# ---- row C: relay up, mapping working -> still direct ---------------------------
start_relay c-relay
start_up c-device in_a "$STATE/c-device" --enroll --bind "$DEVICE_BIND" --relay "$RELAY"
jq -e '.enrollment.port_mapping == "active" and .enrollment.relay_state == "registered"' \
  "$STATE/c-device.ready.json" > /dev/null \
  || fail "row C: expected mapping active and relay registered: $(jq -c .enrollment "$STATE/c-device.ready.json")"
c_token=$(nm in_a "$STATE/c-device" invite create | jq -r .token)
join_as c "$c_token"
jq -e '.enroll_path == "direct" and .attach_path == "direct"' "$STATE/c-join.json" > /dev/null \
  || fail "row C: expected the direct path while the relay is up: $(cat "$STATE/c-join.json")"
stop_up in_a "$STATE/c-device"
stop_relay c-relay
jq -e '.splices == 0' "$STATE/c-relay.stopped.json" > /dev/null \
  || fail "row C: the relay spliced while direct worked: $(cat "$STATE/c-relay.stopped.json")"
log "row C: relay up -> still joined directly; relay spliced nothing"

# ---- row A: relay up, direct forced to fail -> relay -----------------------------
start_relay a-relay
start_up a-device in_a "$STATE/a-device" --enroll --no-port-mapping --bind "$DEVICE_BIND" --relay "$RELAY"
jq -e '.enrollment.port_mapping == "disabled" and .enrollment.relay_state == "registered"' \
  "$STATE/a-device.ready.json" > /dev/null \
  || fail "row A: expected no mapping and relay registered: $(jq -c .enrollment "$STATE/a-device.ready.json")"
# The router's public address forwards nothing without a mapping: the direct
# path in this token is dead, exactly as in R1c's negative control.
a_token=$(nm in_a "$STATE/a-device" invite create --addr "$PUBLIC_A:7001" | jq -r .token)
join_as a "$a_token"
jq -e '.enroll_path == "relay" and .attach_path == "relay"' "$STATE/a-join.json" > /dev/null \
  || fail "row A: expected the relay path with direct dead: $(cat "$STATE/a-join.json")"
start_up a-agent in_wan "$STATE/a-agent" --bind "$AGENT_BIND"
jq -e '.joined.attached == true and .joined.path == "relay"' "$STATE/a-agent.ready.json" > /dev/null \
  || fail "row A: joined up did not attach through the relay: $(jq -c .joined "$STATE/a-agent.ready.json")"
evidence relayed
stop_up in_wan "$STATE/a-agent"
stop_up in_a "$STATE/a-device"
stop_relay a-relay
jq -e '.splices >= 1 and .forwarded_packets > 0 and .registrations_accepted >= 1' \
  "$STATE/a-relay.stopped.json" > /dev/null \
  || fail "row A: relay counters do not show the relayed session: $(cat "$STATE/a-relay.stopped.json")"
# Gateway evidence: the device reached the relay OUTBOUND over UDP (its mesh
# socket's registration) and TCP (the enrollment splice dial-back) ...
grep -E "^udp .*src=192\.168\.101\.2 dst=10\.99\.0\.10 sport=7001 dport=3478" \
  "$STATE/relayed.gw-conntrack.txt" > /dev/null \
  || fail "row A: no outbound UDP flow from the device's mesh socket to the relay"
grep -E "^tcp .*src=192\.168\.101\.2 dst=10\.99\.0\.10 .*dport=3478" \
  "$STATE/relayed.gw-conntrack.txt" > /dev/null \
  || fail "row A: no outbound TCP (splice) flow from the device to the relay"
# ... and nothing from the agent was DNAT'd to the device.
if grep -E "dst=${PUBLIC_A//./\\.} .* src=192\.168\.101\.2 " "$STATE/relayed.gw-conntrack.txt" > /dev/null; then
  fail "row A: a flow reached the device directly; the relay row is not isolating the relay path"
fi
log "row A: direct dead -> join, attach and joined up all went through the relay"

jq -n \
  --slurpfile b "$STATE/b-join.json" --slurpfile c "$STATE/c-join.json" \
  --slurpfile a "$STATE/a-join.json" --slurpfile relay "$STATE/a-relay.stopped.json" \
  '{verdict: "pass",
    relay_down_mapping_up: {enroll: $b[0].enroll_path, attach: $b[0].attach_path},
    both_up: {enroll: $c[0].enroll_path, attach: $c[0].attach_path},
    direct_dead: {enroll: $a[0].enroll_path, attach: $a[0].attach_path,
                  relay_splices: $relay[0].splices,
                  relay_forwarded_packets: $relay[0].forwarded_packets}}' \
  > "$STATE/verdict.json"
log "PASS"
