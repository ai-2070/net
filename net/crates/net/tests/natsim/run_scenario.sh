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
#   rtc_anchor_stun_endpoint   the same NAT'd anchor with BOTH of its
#                              announced endpoints pinned — `rtc_addr`
#                              and the separate `rtc_stun_addr` of
#                              §6.12.2 — each observed from outside
#                              the NAT with its own reply
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
# Side A's THIRD socket: the anchor's separate STUN endpoint,
# announced as `rtc_stun_addr` (§6.12.2). A different port from
# `RTC_PORT_A` because that is the rule the product's own default
# obeys — libwebrtc consumes datagrams arriving on an ICE port from
# an address configured as a STUN server — and because two endpoints
# on one port are one endpoint.
STUN_PORT_A=7103
# Stage 6 browser rows: the disposition the row asserts and the engine
# each side runs. `EXPECT` is passed to the runner so the verdict
# records what the topology was provisioned FOR, which is what makes a
# mis-wired row detectable (`tests/natsim/rows.rs`).
EXPECT="" ENGINE_A=chromium ENGINE_B=chromium
# Whether the harness grants the page's origin camera+microphone
# before opening it. `granted` on every conformance row, because
# Chromium withholds its interface enumeration from WebRTC until a
# media permission exists (S6_REPORT.md §6.12) — and `none` on the
# one leg that exists to measure the product in an ordinary browsing
# context that was never asked. Passed to the runner rather than
# inferred, and echoed into the verdict by the DRIVERS, so a row
# cannot acquire a permission it claims not to need.
MEDIA=granted
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
  rtc_anchor_stun_endpoint)
    # The NAT'd anchor with BOTH of its announced endpoints, and both
    # of them observed from OUTSIDE the NAT (Kyra's E1 item 2).
    #
    # `rtc_anchor_direct` above proves one mapping: the announced
    # `rtc_addr` is the gateway's, and a DataChannel reaches it. It
    # does not exercise the second endpoint Stage 6 §6.12.2 added —
    # the separate STUN socket announced as `rtc_stun_addr` — and the
    # browser matrix cannot, because its anchor sits on the simulated
    # internet with no NAT in front of it, where two local sockets
    # prove nothing about two externally reachable mappings.
    #
    # So: one anchor, inside the cone NAT, with two pinned 1:1 ports.
    # The client outside reads both addresses out of the anchor's own
    # signed announcement, gets a STUN reply from the second one
    # (carrying its own public tuple as the anchor saw it), and takes
    # its session onto a DataChannel at the first. Two mappings, two
    # replies, both announced by the product.
    NAT_A=cone; NAT_B=none; PUBLIC_B=1; MODE=rtc; OUTCOME_NODE=b
    SETUP_EXTRA+=(--rtc-port-a "$RTC_PORT_A" --stun-port-a "$STUN_PORT_A")
    A_EXTRA=(--rtc-bind "192.168.101.2:$RTC_PORT_A"
             --rtc-public "10.99.0.2:$RTC_PORT_A"
             --stun-bind "192.168.101.2:$STUN_PORT_A"
             --stun-public "10.99.0.2:$STUN_PORT_A")
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
  browser_cone_cone) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  browser_cone_portrestricted) NAT_A=cone-ar NAT_B=cone-pr MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  browser_portrestricted_portrestricted) NAT_A=cone-pr NAT_B=cone-pr MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  browser_cone_symmetric) NAT_A=cone-ar NAT_B=symmetric MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  browser_portrestricted_symmetric) NAT_A=cone-pr NAT_B=symmetric MODE=browser EXPECT=relayed ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  browser_symmetric_symmetric) NAT_A=symmetric NAT_B=symmetric MODE=browser EXPECT=relayed ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser ;;
  # DIAGNOSTIC, not a pinned row (S6_REPORT.md §6.12). Both browsers
  # sit directly in nsim_wan on the lab segment: no NAT, no gateway,
  # a real non-loopback interface. It bisects the one question logging
  # cannot answer — Chromium works against this anchor on loopback in
  # the browser matrix and fails here, and the two differences are the
  # NAT and the namespace. If it connects, the NAT is implicated; if
  # it fails, the namespace is, and neither answer needs Chromium
  # internals.
  # NOT named `browser_*`: `tests/natsim_browser.rs` cross-checks the
  # Rust row table against this file's `browser_*` arms and an eighth
  # arm would fail that guard - correctly, since this is a bisect and
  # not a conformance row.
  diag_wan_wan) NAT_A=none NAT_B=none MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted OUTCOME_NODE=browser NETNS_A=nsim_wan NETNS_B=nsim_wan ;;
  # The control: row 1 again, the other engine on both sides. It has
  # always run PERMISSION-FREE — Firefox has no media gate on
  # interface enumeration and Playwright cannot grant it camera or
  # microphone — so the arm records `none` rather than asking for a
  # grant the driver would silently not perform.
  browser_cone_cone_firefox) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=firefox ENGINE_B=firefox MEDIA=none OUTCOME_NODE=browser ;;
  # The PERMISSION-FREE leg: row 1 again, nothing granted.
  #
  # One variable against `browser_cone_cone`: `MEDIA=none`. The six
  # rows and the Firefox control all grant the page camera+microphone
  # because Chromium gates interface enumeration on a media
  # permission, and "the product calls no media API" is source
  # evidence about the product, not a measurement of the ungranted
  # browsing context. Behind the same two real NATs rather than on
  # loopback, because the enumeration this leg measures is what a
  # non-loopback candidate needs.
  browser_cone_cone_nomedia) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=none OUTCOME_NODE=browser ;;
  # The PERMISSION-FREE ROUTED leg: `browser_symmetric_symmetric`
  # again, nothing granted.
  #
  # `browser_cone_cone_nomedia` above answers "can an ungranted pair
  # go DIRECT". It cannot answer "can an ungranted pair use Net's
  # routed path", because on a row that solves direct the anchor's
  # per-pair application counter is flat BY DESIGN — that flatness is
  # the direct row's own assertion. And Net's fallback is not TURN:
  # it rides each leaf's authenticated session with the anchor, so
  # "it falls back to the anchor" is a claim about leaf-to-anchor
  # application delivery that has to be measured, not assumed.
  #
  # symmetric x symmetric is the pair ICE cannot solve, so this arm
  # drives the routed path with the SAME witness the granted relayed
  # rows use: nonce-correlated payloads observed by the receiver, and
  # the anchor's own per-pair forwarding counter moving in both
  # directions across the exchange.
  browser_symmetric_symmetric_nomedia) NAT_A=symmetric NAT_B=symmetric MODE=browser EXPECT=relayed ENGINE_A=chromium ENGINE_B=chromium MEDIA=none OUTCOME_NODE=browser ;;
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

# The browser rows need one filter the native helpers do not: the
# bootstrap listener that the pages actually talk to lives in the
# `net_sdk` crate, not in `net`, so `net::adapter::net=trace,net=info`
# excluded it entirely and the anchor's whole view of a dialog —
# `bootstrap offer accepted`, `trickle socket authorized by its attempt
# token`, and every typed refusal of the upgrade — was silently absent
# from `runner.log`. A browser reports a refused WebSocket handshake as
# a bare 1006 with no reason, so those lines are the ONLY place the
# reason exists.
#
# `str0m` and `is` (its ICE agent) are in for the same reason one level
# deeper. Our wrapper calls `add_remote_candidate`, which returns `()`:
# the agent can silently REJECT a candidate — component != 1, or a
# candidate-level `ufrag` that differs from the negotiated remote
# ufrag (`is/src/agent.rs`) — and report nothing to us. "Our wrapper
# reported success" is not "the agent retained the candidate", and the
# only place that difference is visible is the agent's own `debug!`.
# Its logs redact through `Pii`, so no credential rides along.
NATSIM_BROWSER_LOG="${RUST_LOG:-net::adapter::net=trace,net_sdk=debug,str0m=debug,is=debug,net=info}"

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

# The last boundary on the Chromium rows that no log can settle.
#
# Every account we have is now consistent and still contradictory:
# str0m creates a peer-reflexive candidate FROM the browser's check,
# nominates the pair, reports `Completed`, and `send_to` returns Ok —
# while the browser reports `sent=192 gotResponse=0 recvd=0` and no
# `addIceCandidate` error. Both can only be true if the answers do
# not reach the browser's namespace, or reach it and are discarded
# there. A packet counter inside each browser's own namespace is the
# only thing that separates those two, and it is what turns the next
# failure into a fact instead of another inference.
#
# `tcpdump -w` per namespace, UDP only, snaplen 200 (headers plus the
# STUN attribute prefix, not payload), killed by the EXIT trap with
# the rest. Absent tcpdump the row is unaffected — this is evidence,
# never a gate.
capture() { # capture <netns> <name>
  local ns="$1" name="$2"
  command -v tcpdump >/dev/null || return 0
  ip netns exec "$ns" tcpdump -i any -n -s 200 -U -w "$STATE/$name.pcap" \
    udp >"$STATE/$name.tcpdump.log" 2>&1 &
  PIDS+=("$!")
}

if [[ "$MODE" == browser ]]; then
  # The browser rows launch NO mesh helpers. Every native piece — the
  # anchor `MeshNode` with its RTC socket, the standalone STUN
  # responder on the lab's second public address, the real
  # `serve_bootstrap` HTTPS listener, the page origin, the credential,
  # and one Playwright driver per NAT'd namespace — is in this one
  # binary, running inside nsim_wan on 10.99.0.10. It reaches the
  # browsers with `ip netns exec`, which works from any namespace
  # (setns needs root, which this script already has), so the drivers
  # AND their browsers run entirely inside nsim_a / nsim_b while their
  # stdio pipes stay attached here.
  #
  # `--stun-ip` is VESTIGIAL as of §6.12.2 and the runner ignores it.
  # The matrix used to run a separate STUN host at 10.99.0.11 because
  # libwebrtc drops every datagram arriving on an ICE port from an
  # address that port was given as a STUN server
  # (`UDPPort::OnReadPacket` returns before `GetConnection`), so an
  # anchor that is also the page's STUN server can never form a
  # candidate pair with Chromium — the whole of S6_REPORT.md §6.12.
  #
  # That separate host was a HARNESS search for a configuration that
  # works. The product now announces its own second STUN endpoint and
  # the leaf defaults to it, so these rows exercise what an integrator
  # actually gets. The flag stays only so an existing invocation does
  # not break.
  capture nsim_a browser_a
  capture nsim_b browser_b
  capture nsim_wan anchor_wan
  ip netns exec nsim_wan env RUST_LOG="$NATSIM_BROWSER_LOG" \
    "$BROWSER_BIN" \
      --scenario "$SCENARIO" \
      --state "$STATE" \
      --nat-a "$NAT_A" --nat-b "$NAT_B" \
      --expect "$EXPECT" \
      --engine-a "$ENGINE_A" --engine-b "$ENGINE_B" \
      --media "$MEDIA" \
      --anchor-ip 10.99.0.10 \
      --stun-ip 10.99.0.11 \
      --netns-a "${NETNS_A:-nsim_a}" --netns-b "${NETNS_B:-nsim_b}" \
      >"$STATE/runner.log" 2>&1 &
  PIDS+=("$!")
  # Its OWN variable: the captures are in `PIDS` too now, so index 0
  # is no longer the runner and a liveness check on it would watch a
  # tcpdump instead.
  RUNNER_PID="${PIDS[-1]}"
  echo "natsim: browser runner pid $RUNNER_PID state $STATE"
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
  if [[ "$MODE" == browser ]] && ! kill -0 "$RUNNER_PID" 2>/dev/null; then
    wait "$RUNNER_PID" 2>/dev/null; rc=$?
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
# The conntrack view is read through the SAME ladder as the per-gateway
# snapshot below — `conntrack -L` first, `/proc/net/nf_conntrack` as the
# fallback — and a namespace that can produce neither yields zeros
# rather than killing the script.
#
# `/proc/net/nf_conntrack` alone was wrong: the GitHub runner kernel is
# built WITHOUT `CONFIG_NF_CONNTRACK_PROCFS`, so `cat` exited 1, the
# pipeline failed under `set -o pipefail`, and `set -e` killed
# run_scenario.sh in the middle of writing this file. Every browser row
# therefore exited 1 after a perfectly good verdict had already been
# written — leaving a truncated `{"a":{"udp_flows":0,"udp_replied":0}`
# and no `NATSIM_OUTCOME_PATH=` line at all, which is what
# `tests/natsim.rs` reported as a bare non-zero status. `conntrack -L`
# is installed by the workflow for exactly this reason.
#
# Both renderings carry `[UNREPLIED]` on an unanswered flow and both
# spell the tuple `(src|dst)=<ip>`, so one awk program reads either —
# but they do NOT agree on where the protocol word sits.
# `/proc/net/nf_conntrack` leads with `ipv4     2 udp      17 …`, so
# `udp` has whitespace on both sides; `conntrack -L` leads with `udp`
# itself, at the start of the line. A matcher written for the proc
# format therefore matched NOTHING once the ladder above started
# preferring `conntrack -L`, and run 35056816488's Firefox row —
# `connectPeer settled as "direct" in 269 ms`, `acceptPeer as
# "direct"`, `ice_attempted=2 ice_direct=2` on both leaves and the
# anchor, `errors: []` — was failed by its own witness reporting
# `a: 0/0, b: 0/0` while the artifact dump three sections below listed
# the A<->B flow on BOTH gateways. Allow either position.
#
# `(src|dst)=<peer>` with a non-digit boundary so 10.99.0.3 never
# matches 10.99.0.30.
if [[ "$MODE" == browser ]]; then
  flow_side() { # flow_side <gateway-ns> <peer public ip>
    local ns="$1" peer="$2" raw="" out="" source="unreadable"
    # EACH READER TRIED ON ITS OWN, WITH ITS STATUS KEPT.
    #
    # The previous shape was `conntrack -L || cat /proc/... || true`,
    # which cannot distinguish "the table was read and holds no
    # matching flow" from "no reader ran at all": both produced an
    # empty `raw` and the awk below then printed zeros. A DIRECT row
    # fails loudly on zeros, but a RELAYED row reads zeros as its own
    # confirmation — so a namespace that could be read by neither
    # reader would have CONFIRMED both relayed rows while observing
    # nothing whatever. That is measurement failure counting as
    # observed absence, and it is Kyra's E1 witness-hardening item.
    #
    # `conntrack -L` exits 0 when it successfully dumps a table,
    # including an EMPTY one (it reports the count on stderr), and
    # non-zero when it cannot talk to the kernel or is absent. So the
    # command's own status is the right discriminator, and it is now
    # used as one instead of being swallowed.
    if [[ -e "/var/run/netns/$ns" ]]; then
      if out="$(ip netns exec "$ns" conntrack -L 2>/dev/null)"; then
        raw="$out"
        source="conntrack"
      elif out="$(ip netns exec "$ns" cat /proc/net/nf_conntrack 2>/dev/null)"; then
        raw="$out"
        source="procfs"
      fi
    fi
    printf '%s' "$raw" | awk -v peer="$peer" -v source="$source" '
        $0 ~ /(^|[[:space:]])udp[[:space:]]/ {
          if ($0 !~ ("(src|dst)=" peer "([^0-9]|$)")) next
          flows++
          if ($0 !~ /\[UNREPLIED\]/) replied++
        }
        END {
          printf "{\"udp_flows\":%d,\"udp_replied\":%d,\"source\":\"%s\"}",
                 flows+0, replied+0, source
        }
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

# The UPLOADABLE bundle: regular files only, with their own byte
# count printed.
#
# Two facts made this necessary. A successful natsim run uploaded
# nothing at all, so the retained green job log was the only evidence
# a row ever produced — not a packet or gateway archive. And an
# earlier upload of a browser profile silently produced an EMPTY
# artifact, because `actions/upload-artifact` refuses a tree
# containing unix sockets and the profile holds several. The state
# directory still holds those profiles (Firefox's persistent context
# lives in `browser/profile-*`), so uploading `$STATE` wholesale
# would reproduce exactly that failure.
#
# So the script assembles what is worth keeping — the verdict, the
# runner and page logs, the gateway snapshots, the per-namespace
# packet captures — as copies of REGULAR FILES in one directory, and
# prints the file count and total size. A bundle that came out empty
# says so here, in the job log, instead of being discovered as an
# empty artifact after the fact.
BUNDLE="$STATE/artifacts"
mkdir -p "$BUNDLE"
for f in "$STATE"/*.json "$STATE"/*.log "$STATE"/*.pcap; do
  [[ -f "$f" ]] || continue
  cp -- "$f" "$BUNDLE/" 2>/dev/null || true
done
BUNDLE_FILES="$(find "$BUNDLE" -type f | wc -l)"
BUNDLE_BYTES="$(find "$BUNDLE" -type f -printf '%s\n' 2>/dev/null | awk '{t+=$1} END {print t+0}')"
chmod 755 "$BUNDLE"
chmod 644 "$BUNDLE"/* 2>/dev/null || true
echo "natsim: artifact bundle $BUNDLE: $BUNDLE_FILES file(s), $BUNDLE_BYTES byte(s)"
if [[ "$BUNDLE_FILES" -eq 0 || "$BUNDLE_BYTES" -eq 0 ]]; then
  echo "natsim: WARNING the artifact bundle is empty — an upload of it would produce \
nothing, which is how a previous cycle lost its evidence" >&2
fi

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
