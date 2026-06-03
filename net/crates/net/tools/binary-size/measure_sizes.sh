#!/usr/bin/env zsh
#
# measure_sizes.sh — measure shipped artifact sizes per feature combo.
#
# Builds the core crate (`net-mesh`) and the Node binding (`net-node`) at
# full-LTO release (`[profile.release]`: lto=true, codegen-units=1,
# panic=abort, opt-level=3) for each feature combo used in the README
# "Binary size" table, then records cdylib / rlib / staticlib sizes.
#
# Usage:
#   ./measure_sizes.sh               # core + node + default-set rows
#   SKIP_NODE=1 ./measure_sizes.sh   # core crate only (the Node binding's
#                                    # LTO link can be very slow; skip it)
#
# Output: size_results.txt (pipe-delimited), echoed to stdout.
# Re-run after dependency or feature changes to refresh the README table.
# Numbers are host-specific — note the target triple + date in the README.
set -e
cd "$(dirname "$0")/../.."   # -> crates/net (workspace root)
OUT=tools/binary-size/size_results.txt
: > "$OUT"
TDIR=target/release

mb() { awk -v b="$1" 'BEGIN{printf "%.2f MB", b/1048576}'; }

core() { # label, feature-list ("" = crate default set)
  local label="$1" feats="$2"
  if [ -z "$feats" ]; then
    cargo build --release -p net-mesh >/dev/null 2>&1
  else
    cargo build --release -p net-mesh --no-default-features --features "$feats" >/dev/null 2>&1
  fi
  echo "CORE|$label|$(mb $(stat -f%z $TDIR/libnet.dylib))|$(mb $(stat -f%z $TDIR/libnet.rlib))|$(mb $(stat -f%z $TDIR/libnet.a))" | tee -a "$OUT"
}

node() { # label, feature-list ("" = crate default set)
  local label="$1" feats="$2"
  if [ -z "$feats" ]; then
    cargo build --release -p net-node >/dev/null 2>&1
  else
    cargo build --release -p net-node --no-default-features --features "$feats" >/dev/null 2>&1
  fi
  echo "NODE|$label|$(mb $(stat -f%z $TDIR/libnet_node.dylib))" | tee -a "$OUT"
}

core "net"                                        "net"
core "net + redex"                                "net,redex"
core "net + redex + redex-disk"                   "net,redex,redex-disk"
core "net + redex + redex-disk + cortex"          "net,redex,redex-disk,cortex"
core "net + redex + redex-disk + cortex + netdb"  "net,redex,redex-disk,cortex,netdb"
core "net + nat-traversal"                        "net,nat-traversal"
core "net + nat-traversal + port-mapping"         "net,nat-traversal,port-mapping"
core "default (net,nat-traversal,cortex,meshdb,meshos,dataforts)" ""

if [ -z "${SKIP_NODE:-}" ]; then
  node "net"                    "net"
  node "net + compute"          "net,compute"
  node "net + compute + groups" "net,compute,groups"
  node "default (full stack)"   ""
fi

echo "DONE" | tee -a "$OUT"
