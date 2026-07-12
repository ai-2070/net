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

[[ -x "$BIN" ]] || {
  echo "natsim: helper not built: $BIN (cargo build --example natsim_node --features net,nat-traversal)" >&2
  exit 2
}

NAT_A=cone NAT_B=cone SETUP_EXTRA=() PUBLIC_B=0 MODE=punch
case "$SCENARIO" in
  cone_cone_punch)          NAT_A=cone;      NAT_B=cone ;;
  symmetric_cone_punch)     NAT_A=symmetric; NAT_B=cone ;;
  symmetric_symmetric_skip) NAT_A=symmetric; NAT_B=symmetric ;;
  dropped_keepalives)       NAT_A=cone;      NAT_B=cone; SETUP_EXTRA+=(--drop-direct) ;;
  relay_upgrade)            NAT_A=cone;      NAT_B=none; PUBLIC_B=1; MODE=upgrade ;;
  *) echo "unknown scenario: $SCENARIO" >&2; exit 2 ;;
esac

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

launch() { # launch <netns> <logname> <args...>
  local ns="$1" log="$2"; shift 2
  ip netns exec "$ns" "$BIN" "$@" >"$STATE/$log.log" 2>&1 &
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

# Publics: X accepts R first (R dials it), then the joiners.
launch nsim_wan x  public --name x --bind 10.99.0.11:7000 --state "$STATE" --joiners r,a,b
launch nsim_wan r  public --name r --bind 10.99.0.10:7000 --state "$STATE" --joiners a,b --connect-to x

if [[ "$PUBLIC_B" == 1 ]]; then
  # B runs publicly inside the wan namespace (no NAT).
  launch nsim_wan b joiner --name b --bind 10.99.0.12:7002 --state "$STATE" \
    --publics r,x --auto-upgrade "${SEED_ARGS_B[@]}"
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

A_EXTRA=(--target b --mode "$MODE")
[[ "$MODE" == upgrade ]] && A_EXTRA+=(--auto-upgrade)
# Concrete LAN IP (192.168.101.2), not 0.0.0.0 — see the B side above
# for why a wildcard bind misclassifies a port-preserving cone NAT.
launch nsim_a a joiner --name a --bind 192.168.101.2:7001 --state "$STATE" \
  --publics r,x "${A_EXTRA[@]}" "${SEED_ARGS_A[@]}"

# Wait for the initiator's verdict.
OUTCOME="$STATE/a_outcome.json"
for _ in $(seq 1 240); do
  [[ -s "$OUTCOME" ]] && break
  sleep 0.5
done
if [[ ! -s "$OUTCOME" ]]; then
  echo "natsim: scenario $SCENARIO timed out; helper logs:" >&2
  tail -n 40 "$STATE"/*.log >&2 || true
  exit 1
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
