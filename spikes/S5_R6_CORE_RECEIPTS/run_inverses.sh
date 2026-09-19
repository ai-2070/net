#!/usr/bin/env bash
# Raw inverse receipts for the round-6 core items: for each item,
# mutate the repair away, run its witness (expect RED), restore,
# verify the file identity by sha256, run it again (expect GREEN).
set -uo pipefail
cd "$(dirname "$0")/../../net/crates/net" || exit 1
R=../../../spikes/S5_R6_CORE_RECEIPTS
SRC="src/adapter/net/mesh.rs src/adapter/net/rtc/fragment.rs"
export CARGO_INCREMENTAL=0

run_item() {
  item="$1"; filter="$2"; log="$R/$item.receipt.log"
  {
    echo "=== $item — inverse receipt ==="
    echo "--- repaired identity"
    sha256sum $SRC
    echo "--- applied repair diff (this item's hunks are named in inverse.py)"
    python3 "$R/inverse.py" "$item" apply || exit 3
    echo "--- INVERSE APPLIED; command:"
    echo "\$ CARGO_INCREMENTAL=0 cargo test --test rtc_repairs --features \"net webrtc fixtures\" $filter"
    cargo test --test rtc_repairs --features "net webrtc fixtures" "$filter" 2>&1
    echo "EXIT=$?"
    python3 "$R/inverse.py" "$item" restore || exit 4
    echo "--- RESTORED identity (must equal the repaired identity above)"
    sha256sum $SRC
    echo "--- GREEN rerun, same command:"
    cargo test --test rtc_repairs --features "net webrtc fixtures" "$filter" 2>&1
    echo "EXIT=$?"
  } >"$log" 2>&1
  echo "$item -> $log"
  grep -E "^test result:" "$log"
}

run_item R4-6 reassembly_loss
run_item R4-7 predecessor_fragment_group
run_item R4-8 retired_incarnation
run_item R4-1 mixed_producer
run_item R5-N1 sequence_ownership
run_item R5-N2 oversized_send
