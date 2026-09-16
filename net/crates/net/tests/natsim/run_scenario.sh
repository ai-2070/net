#!/usr/bin/env bash
# Orchestrate one natsim scenario end-to-end: provision the
# namespaces, launch helper nodes inside them, wait for the
# initiator's verdict, tear everything down, and print the outcome
# JSON path on a `NATSIM_OUTCOME_PATH=` marker line (the
# `tests/natsim.rs` wrappers parse that).
#
#   run_scenario.sh <scenario> [state_dir]
#
# Scenarios (NAT_TRAVERSAL_V2_PLAN.md Stage 4 matrix):
#   cone_cone_punch            both cone      → punch succeeds
#   symmetric_cone_punch       A sym, B cone  → 1 attempt, fallback
#   symmetric_symmetric_skip   both sym       → matrix skip, relay
#   dropped_keepalives         both cone + direct-UDP drop → fallback
#   relay_upgrade              A cone-NAT'd (lower id), B public,
#                              auto-upgrade migrates off the relay
#   rtc_anchor_direct          A cone-NAT'd anchor with a pinned RTC
#                              socket, B a public native client that
#                              upgrades the relayed session onto a
#                              DataChannel reached at A's MAPPED
#                              `rtc_addr` (needs a `webrtc` helper)
#   browser_*                  the Stage 6 NAT conformance matrix: two
#                              headless browsers behind simulated NATs
#                              plus one anchor, six rows plus a Firefox
#                              control. No mesh helper runs; the anchor,
#                              the page and both browser drivers are the
#                              `tests/natsim/browser` runner. See
#                              `tests/natsim/rows.rs` for the table and
#                              the expected disposition per row
#
# Requires root (netns + nft). The helper binary must already be
# built: NATSIM_NODE_BIN or target/debug/examples/natsim_node.
set -euo pipefail

SCENARIO="${1:?usage: run_scenario.sh <scenario> [state_dir]}"
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE_DIR="$(cd "$HERE/../.." && pwd)"
BIN="${NATSIM_NODE_BIN:-$CRATE_DIR/target/debug/examples/natsim_node}"
STATE="${2:-$(mktemp -d /tmp/natsim.XXXXXX)}"
mkdir -p "$STATE"
# Root-only while the run is live (helpers all run as root inside
# their namespaces; mktemp's 700 default is right — a 777 dir under
# /tmp would hand any local user write access and a symlink surface,
# cubic P3). Relaxed to read-only for others at the end, so the
# non-root `cargo test` wrapper can read the outcome file this
# script's last stdout line points at.
chmod 700 "$STATE"

# The mesh helper. The Stage 6 `browser_*` rows run NO mesh helper at
# all — their nodes are browser leaves and their anchor is the browser
# runner — so requiring it there would refuse a scenario that does not
# use it.
if [[ "$SCENARIO" != browser_* ]]; then
  [[ -x "$BIN" ]] || {
    echo "natsim: helper not built: $BIN (cargo build --example natsim_node --features net,nat-traversal)" >&2
    exit 2
  }
fi

# Per-scenario knobs. `OUTCOME_NODE` names the side that writes the
# verdict: `a` for every punch/upgrade scenario (A is the initiator),
# `b` for the RTC scenario, where the *client* outside the NAT is the
# one that drives the upgrade and therefore the one with a verdict.
NAT_A=cone NAT_B=cone SETUP_EXTRA=() PUBLIC_B=0 MODE=punch OUTCOME_NODE=a
# Extra helper args per side, resolved with the scenario.
A_EXTRA=() PUBLIC_B_EXTRA=(--auto-upgrade)
# Side A's RTC (second) socket, when the scenario runs one. The port
# is pinned 1:1 by the cone gateway (`setup.sh --rtc-port-a`), which
# is the only reason the anchor can advertise it.
RTC_PORT_A=7101
RTC_PORT_B=7102
# Stage 6 browser rows: the disposition the row asserts and the engine
# each side runs. `EXPECT` is passed to the runner so the verdict
# records what the topology was provisioned FOR, which is what makes a
# mis-wired row detectable (`tests/natsim/rows.rs`).
EXPECT="" ENGINE_A=chromium ENGINE_B=chromium
# The Stage 6 browser runner: anchor + HTTPS bootstrap listener + page
# origin + both Playwright drivers, one binary, inside nsim_wan.
BROWSER_BIN="${NATSIM_BROWSER_BIN:-$HERE/browser/target/release/natsim-browser-matrix}"
case "$SCENARIO" in
  cone_cone_punch)          NAT_A=cone;      NAT_B=cone ;;
  symmetric_cone_punch)     NAT_A=symmetric; NAT_B=cone ;;
  symmetric_symmetric_skip) NAT_A=symmetric; NAT_B=symmetric ;;
  dropped_keepalives)       NAT_A=cone;      NAT_B=cone; SETUP_EXTRA+=(--drop-direct) ;;
  relay_upgrade)            NAT_A=cone;      NAT_B=none; PUBLIC_B=1; MODE=upgrade ;;
  rtc_anchor_direct)
    NAT_A=cone; NAT_B=none; PUBLIC_B=1; MODE=rtc; OUTCOME_NODE=b
    SETUP_EXTRA+=(--rtc-port-a "$RTC_PORT_A")
    # A is the anchor: it runs an RTC socket on the pinned port and
    # is told its own mapped address, which is what it announces as
    # `rtc_addr`. No `--target`, so it is the responder here.
    A_EXTRA=(--rtc-bind "192.168.101.2:$RTC_PORT_A"
             --rtc-public "10.99.0.2:$RTC_PORT_A")
    # B is the client: public, its own RTC socket needs no advertised
    # address (it IS reachable), and it drives the upgrade. No
    # `--auto-upgrade`: the only direct path under test is the
    # DataChannel, and a UDP punch racing it would decide the verdict.
    PUBLIC_B_EXTRA=(--target a --mode rtc --rtc-bind "10.99.0.12:$RTC_PORT_B")
    ;;
  # --- Stage 6 browser NAT conformance matrix -----------------------
  # Two headless browsers behind simulated NATs, one anchor. Every arm
  # is ONE line in a fixed shape because `tests/natsim/rows.rs` parses
  # it: the Rust table and these arms are cross-checked by
  # `tests/natsim_browser.rs`, which runs on every platform, so the
  # provisioned topology and the asserted disposition cannot drift.
  #
  # `cone-ar` (address-restricted) and `cone-pr` (port-restricted) are
  # both endpoint-independent in mapping and differ only in filtering
  # — see setup.sh. That difference is why ar x symmetric solves and
  # pr x symmetric cannot.
  browser_cone_cone) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  browser_cone_portrestricted) NAT_A=cone-ar NAT_B=cone-pr MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  browser_portrestricted_portrestricted) NAT_A=cone-pr NAT_B=cone-pr MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  browser_cone_symmetric) NAT_A=cone-ar NAT_B=symmetric MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  browser_portrestricted_symmetric) NAT_A=cone-pr NAT_B=symmetric MODE=browser EXPECT=relayed ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  browser_symmetric_symmetric) NAT_A=symmetric NAT_B=symmetric MODE=browser EXPECT=relayed ENGINE_A=chromium ENGINE_B=chromium OUTCOME_NODE=browser ;;
  # The control: row 1 again, the other engine on both sides.
  browser_cone_cone_firefox) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=firefox ENGINE_B=firefox OUTCOME_NODE=browser ;;
  *) echo "unknown scenario: $SCENARIO" >&2; exit 2 ;;
esac

# Pre-flight the feature the scenario needs, BEFORE any namespace is
# touched. Without this a helper built with the default features runs
# happily with no RTC socket and the scenario fails 120 s later as an
# empty verdict with nothing naming the cause.
if [[ "$MODE" == rtc ]]; then
  CAPS="$("$BIN" capabilities 2>/dev/null || true)"
  case "$CAPS" in
    *'"webrtc":true'*) ;;
    *) echo "natsim: $SCENARIO needs a helper built with the webrtc feature \
(cargo build --example natsim_node --features net,nat-traversal,webrtc); got: ${CAPS:-<no output>}" >&2
       exit 2 ;;
  esac
fi

# Same discipline for the browser rows: refuse BEFORE provisioning
# when the pieces a browser row cannot run without are missing. A row
# that provisions namespaces and then discovers there is no runner
# fails 120 s later as an empty verdict, naming nothing.
if [[ "$MODE" == browser ]]; then
  [[ -x "$BROWSER_BIN" ]] || {
    echo "natsim: $SCENARIO needs the browser runner: $BROWSER_BIN" >&2
    echo "  cargo build --release --manifest-path $HERE/browser/Cargo.toml" >&2
    echo "  (or set NATSIM_BROWSER_BIN)" >&2
    exit 2
  }
  command -v node >/dev/null || {
    echo "natsim: $SCENARIO needs node on PATH (the Playwright driver)" >&2
    exit 2
  }
  [[ -d "$HERE/browser/driver/node_modules" ]] || {
    echo "natsim: $SCENARIO needs the driver's dependencies installed:" >&2
    echo "  (cd $HERE/browser/driver && npm install && npx playwright install --with-deps chromium firefox)" >&2
    exit 2
  }
fi

PIDS=()
cleanup() {
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  "$HERE/teardown.sh"
}
trap cleanup EXIT

"$HERE/teardown.sh"
SETUP_ARGS=(--nat-a "$NAT_A" --nat-b "$NAT_B" "${SETUP_EXTRA[@]}")
[[ "$PUBLIC_B" == 1 ]] && SETUP_ARGS+=(--public-b)
"$HERE/setup.sh" "${SETUP_ARGS[@]}"

# Log level for the helpers. This MUST reach trace: the rendezvous drop
# paths that decide a punch's fate — the forged/non-coordinator
# introduce drop, and every branch of `unsolicited_introduce_permitted`
# (reflex-IP mismatch, per-source train budget, global concurrent-train
# ceiling) — are all `tracing::trace!`. A `debug` default filters out
# exactly the lines the failure needs, which is what the first
# instrumented run did. Override with RUST_LOG=... to widen or quieten.
NATSIM_LOG="${RUST_LOG:-net::adapter::net=trace,net=info}"

launch() { # launch <netns> <logname> <args...>
  local ns="$1" log="$2"; shift 2
  # `ip netns exec` keeps the environment, but be explicit — this runs
  # under sudo from the test wrapper, where the ambient env is stripped.
  ip netns exec "$ns" env RUST_LOG="$NATSIM_LOG" "$BIN" "$@" >"$STATE/$log.log" 2>&1 &
  PIDS+=("$!")
}

# For the upgrade scenario C1 (only the lower node id initiates)
# must land on the NAT'd joiner A — it's the only side that can
# actually reach its peer directly. Generate both identities up
# front and hand A the lower one.
SEED_ARGS_A=() SEED_ARGS_B=()
if [[ "$MODE" == upgrade ]]; then
  K1="$("$BIN" keygen)"; K2="$("$BIN" keygen)"
  ID1="$(echo "$K1" | sed -n 's/.*"node_id":\([0-9]*\).*/\1/p')"
  ID2="$(echo "$K2" | sed -n 's/.*"node_id":\([0-9]*\).*/\1/p')"
  S1="$(echo "$K1" | sed -n 's/.*"seed_hex":"\([0-9a-f]*\)".*/\1/p')"
  S2="$(echo "$K2" | sed -n 's/.*"seed_hex":"\([0-9a-f]*\)".*/\1/p')"
  # node_id is a u64 that routinely exceeds i64::MAX (~half of keys).
  # Bash arithmetic `-lt` is SIGNED 64-bit and silently truncates /
  # mis-orders those values, so ~half the runs would hand A the HIGHER
  # id — failing the C1 "only the lower id initiates" gate so the
  # background upgrade never fires (the flaky relay_upgrade failure:
  # upgrade_loop_candidate=false, upgrades_attempted=0). Compare as
  # zero-padded fixed-width decimal strings: for equal widths, lexical
  # order == unsigned numeric order, and no value is ever parsed as a
  # (truncated, signed) integer.
  pad_u64() { printf '%020s' "$1" | tr ' ' '0'; }
  if [[ "$(pad_u64 "$ID1")" < "$(pad_u64 "$ID2")" ]]; then
    LOW="$S1"; HIGH="$S2"
  else
    LOW="$S2"; HIGH="$S1"
  fi
  SEED_ARGS_A=(--seed-hex "$LOW")
  SEED_ARGS_B=(--seed-hex "$HIGH")
fi

if [[ "$MODE" == browser ]]; then
  # The browser rows launch NO mesh helpers. Every native piece — the
  # anchor `MeshNode` with its RTC socket and STUN responder, the real
  # `serve_bootstrap` HTTPS listener, the page origin, the credential,
  # and one Playwright driver per NAT'd namespace — is in this one
  # binary, running inside nsim_wan on 10.99.0.10. It reaches the
  # browsers with `ip netns exec`, which works from any namespace
  # (setns needs root, which this script already has), so the drivers
  # AND their browsers run entirely inside nsim_a / nsim_b while their
  # stdio pipes stay attached here.
  ip netns exec nsim_wan env RUST_LOG="$NATSIM_LOG" \
    "$BROWSER_BIN" \
      --scenario "$SCENARIO" \
      --state "$STATE" \
      --nat-a "$NAT_A" --nat-b "$NAT_B" \
      --expect "$EXPECT" \
      --engine-a "$ENGINE_A" --engine-b "$ENGINE_B" \
      --anchor-ip 10.99.0.10 \
      --netns-a nsim_a --netns-b nsim_b \
      >"$STATE/runner.log" 2>&1 &
  PIDS+=("$!")
  echo "natsim: browser runner pid ${PIDS[0]} state $STATE"
else
  # Publics: X accepts R first (R dials it), then the joiners.
  launch nsim_wan x  public --name x --bind 10.99.0.11:7000 --state "$STATE" --joiners r,a,b
  launch nsim_wan r  public --name r --bind 10.99.0.10:7000 --state "$STATE" --joiners a,b --connect-to x

  if [[ "$PUBLIC_B" == 1 ]]; then
    # B runs publicly inside the wan namespace (no NAT).
    launch nsim_wan b joiner --name b --bind 10.99.0.12:7002 --state "$STATE" \
      --publics r,x "${PUBLIC_B_EXTRA[@]}" "${SEED_ARGS_B[@]}"
  else
    # Bind the concrete LAN IP (192.168.102.2), NOT 0.0.0.0. The
    # classifier's Open check does port-only matching on a wildcard bind
    # (classify.rs Finding B3), so a port-preserving cone NAT
    # (`masquerade persistent` keeps the source port) reflects back
    # `10.99.0.3:7002`, whose port matches the bind port, and the node
    # misclassifies as Open instead of Cone. A concrete bind IP forces
    # the full `reflex.ip() == bind.ip()` comparison, which the NAT'd
    # public IP fails → Cone, as the scenario expects. (Symmetric dodges
    # this because `fully-random` scrambles the port.)
    launch nsim_b b joiner --name b --bind 192.168.102.2:7002 --state "$STATE" \
      --publics r,x "${SEED_ARGS_B[@]}"
  fi

  # A's role: initiator for every punch/upgrade scenario, responder (the
  # anchor) for the RTC one, where the scenario already filled A_EXTRA.
  if [[ "$MODE" != rtc ]]; then
    A_EXTRA=(--target b --mode "$MODE")
    if [[ "$MODE" == upgrade ]]; then
      A_EXTRA+=(--auto-upgrade)
    fi
  fi
  # Concrete LAN IP (192.168.101.2), not 0.0.0.0 — see the B side above
  # for why a wildcard bind misclassifies a port-preserving cone NAT.
  launch nsim_a a joiner --name a --bind 192.168.101.2:7001 --state "$STATE" \
    --publics r,x "${A_EXTRA[@]}" "${SEED_ARGS_A[@]}"
fi

# Wait for the verdict from whichever side drives this scenario.
#
# For the browser rows there is exactly ONE process to wait on, and it
# can die before it ever writes a verdict — a missing engine, a driver
# that cannot start inside the namespace, a panic in the runner. When
# that happened the script exited with the runner's status and no
# output at all: the row failed naming nothing, which is the one thing
# a conformance matrix must never do. Check liveness while waiting and
# print what it said.
OUTCOME="$STATE/${OUTCOME_NODE}_outcome.json"
for _ in $(seq 1 240); do
  [[ -s "$OUTCOME" ]] && break
  if [[ "$MODE" == browser ]] && ! kill -0 "${PIDS[0]}" 2>/dev/null; then
    wait "${PIDS[0]}" 2>/dev/null; rc=$?
    echo "natsim: the browser runner exited with status $rc before writing a \
verdict; its log follows:" >&2
    tail -n 60 "$STATE/runner.log" >&2 || true
    exit 1
  fi
  sleep 0.5
done
if [[ ! -s "$OUTCOME" ]]; then
  echo "natsim: scenario $SCENARIO timed out; helper logs:" >&2
  tail -n 40 "$STATE"/*.log >&2 || true
  exit 1
fi

# The browser rows' INDEPENDENT disposition witness, written from the
# gateways' own conntrack tables before teardown.
#
# Neither endpoint is asked. A leaf reporting "direct" and an anchor
# reporting a flat forwarding counter are both statements by a party
# to the session; this one is the NAT's. And the discriminator is not
# "is there a flow to the peer" — ICE sends checks on EVERY row,
# including the two that cannot solve, so an outbound entry always
# exists. It is whether that flow was ever REPLIED to: an entry
# without `[UNREPLIED]` means packets crossed between the two public
# addresses in both directions, which is what direct physically means
# here, and its absence on both gateways is what relayed means.
#
# `/proc/net/nf_conntrack` rather than `conntrack -L`: it is
# per-namespace, always present when conntrack is loaded, and needs no
# extra package. `(src|dst)=<peer>` with a non-digit boundary so
# 10.99.0.3 never matches 10.99.0.30.
if [[ "$MODE" == browser ]]; then
  flow_side() { # flow_side <gateway-ns> <peer public ip>
    local ns="$1" peer="$2"
    if [[ ! -e "/var/run/netns/$ns" ]]; then
      printf '{"udp_flows":0,"udp_replied":0}'
      return 0
    fi
    ip netns exec "$ns" cat /proc/net/nf_conntrack 2>/dev/null |
      awk -v peer="$peer" '
        $0 ~ /[[:space:]]udp[[:space:]]/ {
          if ($0 !~ ("(src|dst)=" peer "([^0-9]|$)")) next
          flows++
          if ($0 !~ /\[UNREPLIED\]/) replied++
        }
        END { printf "{\"udp_flows\":%d,\"udp_replied\":%d}", flows+0, replied+0 }
      '
  }
  {
    printf '{"a":'
    flow_side nsim_gwa 10.99.0.3
    printf ',"b":'
    flow_side nsim_gwb 10.99.0.2
    printf '}\n'
  } >"$STATE/nat_flow.json"
  echo "natsim: gateway flow witness: $(cat "$STATE/nat_flow.json")"
fi

# Snapshot each gateway's NAT state, BEFORE the EXIT trap tears the
# namespaces down. This is the view the helper logs cannot provide: the
# endpoints only see "I sent N keep-alives and received none", while the
# question is what mapping the gateway actually installed for the punch
# destination — specifically whether the public port it chose for the
# peer-directed flow is the same one the peer was told to expect.
#
# The trains keep refreshing these entries for the whole punch window
# (UDP conntrack timeout is far longer than the 5 s deadline), so a
# snapshot taken right after the verdict still shows them.
# Section order is deliberate: the artifact dump prints each file's
# TAIL, so the punch-relevant conntrack summary goes LAST. Putting it
# first (as the first version did) meant a long `ip addr` / ruleset
# section pushed the one thing worth reading out of the captured window.
for gw in nsim_gwa nsim_gwb; do
  [[ -e "/var/run/netns/$gw" ]] || continue
  CT="$STATE/${gw}_conntrack_raw.txt"
  ip netns exec "$gw" conntrack -L 2>/dev/null >"$CT" \
    || ip netns exec "$gw" cat /proc/net/nf_conntrack 2>/dev/null >"$CT" \
    || echo "(no conntrack view available)" >"$CT"
  {
    echo "### addrs ($gw)"
    ip -n "$gw" addr 2>&1 | grep -E '^[0-9]+:|inet ' || true
    echo "### nft ruleset ($gw)"
    ip netns exec "$gw" nft list ruleset 2>&1 || true
    echo "### conntrack, all UDP ($gw)"
    grep -E 'udp' "$CT" || echo "(no udp entries)"
    # Dead last, and the whole point of the capture: the A<->B flows.
    # On gwb a healthy punch shows the B->A flow SNAT'd to sport=7002
    # (the reflex A was told); anything else is the port-mismatch
    # hypothesis confirmed. 10.99.0.12 is B when it plays the public
    # peer (relay_upgrade, rtc_anchor_direct) — for the RTC scenario
    # this is where the anchor's pinned RTC mapping shows up, and
    # `sport` other than the pinned port means the gateway did not
    # honour the 1:1 SNAT the anchor advertised.
    echo "### PUNCH-RELEVANT: flows mentioning 10.99.0.2 and (10.99.0.3|10.99.0.12) ($gw)"
    awk '/10\.99\.0\.2/ && (/10\.99\.0\.3/ || /10\.99\.0\.12/)' "$CT" || true
    echo "### (end $gw)"
  } >"$STATE/${gw}_nat.log" 2>&1
  rm -f "$CT"
done

# Open the artifacts read-only to non-root (no write bit anywhere)
# so the invoking `cargo test` process can read the outcome path
# printed below, and a human can inspect the helper logs.
chmod 755 "$STATE"
chmod 644 "$STATE"/*.json "$STATE"/*.log 2>/dev/null || true

echo "natsim scenario=$SCENARIO outcome:"
cat "$OUTCOME"
# Emit the outcome path on its own line with an unambiguous marker.
# `cat` above prints the JSON verbatim, and serde's `to_vec_pretty`
# ends the file with `}` and NO trailing newline — so lead with `\n`
# to guarantee the marker starts a fresh line instead of being glued
# onto the closing brace. The `tests/natsim.rs` wrapper greps for the
# `NATSIM_OUTCOME_PATH=` prefix rather than trusting "the last line".
printf '\nNATSIM_OUTCOME_PATH=%s\n' "$OUTCOME"
