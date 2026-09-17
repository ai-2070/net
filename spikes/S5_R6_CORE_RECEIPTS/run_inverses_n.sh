#!/usr/bin/env sh
# The two round-5 additions, same protocol as run_inverses.sh.
set -u
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
    python3 "$R/inverse.py" "$item" apply
    echo "--- INVERSE APPLIED; command:"
    echo "\$ CARGO_INCREMENTAL=0 cargo test --test rtc_repairs --features \"net webrtc fixtures\" $filter"
    cargo test --test rtc_repairs --features "net webrtc fixtures" "$filter" 2>&1
    echo "EXIT=$?"
    python3 "$R/inverse.py" "$item" restore
    echo "--- RESTORED identity (must equal the repaired identity above)"
    sha256sum $SRC
    echo "--- GREEN rerun, same command:"
    cargo test --test rtc_repairs --features "net webrtc fixtures" "$filter" 2>&1
    echo "EXIT=$?"
  } >"$log" 2>&1
  echo "$item -> $log"
  grep -E "^test result:|ok$|FAIL" "$log"
}

run_item R5-N1 sequence_ownership
run_item R5-N2 oversized_send
