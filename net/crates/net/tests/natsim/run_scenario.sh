#!/usr/bin/env bash
# Orchestrate one natsim scenario end-to-end: provision the
# namespaces, launch helper nodes inside them, wait for the
# initiator's verdict, tear everything down, and print the outcome
# JSON path on the LAST line of stdout (the `tests/natsim.rs`
# wrappers parse that).
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
chmod 777 "$STATE"

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
  if [[ "$ID1" -lt "$ID2" ]]; then LOW="$S1"; HIGH="$S2"; else LOW="$S2"; HIGH="$S1"; fi
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
  launch nsim_b b joiner --name b --bind 0.0.0.0:7002 --state "$STATE" \
    --publics r,x "${SEED_ARGS_B[@]}"
fi

A_EXTRA=(--target b --mode "$MODE")
[[ "$MODE" == upgrade ]] && A_EXTRA+=(--auto-upgrade)
launch nsim_a a joiner --name a --bind 0.0.0.0:7001 --state "$STATE" \
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

echo "natsim scenario=$SCENARIO outcome:"
cat "$OUTCOME"
echo "$OUTCOME"
