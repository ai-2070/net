#!/usr/bin/env sh
# The demo, from a clean checkout, in ONE command.
#
#   net/crates/net/examples/browser-demo/run.sh            # two windows, 60 Hz, by hand
#   net/crates/net/examples/browser-demo/run.sh --check     # headless, asserts, exits non-zero on failure
#
# Everything below is the documented build: the wasm leaf, the
# `@net-mesh/browser` bundle, the demo's two npm dependencies
# (three.js and playwright-core), and the host. Arguments are passed
# through to the host, so `--check --seconds 10` works.
set -eu

demo=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
net=$(CDPATH= cd -- "$demo/../.." && pwd)

step() { printf '== %s\n' "$1"; }

step 'the wasm leaf'
cd "$net/leaf"
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/net_leaf.wasm

step '@net-mesh/browser'
cd "$net/browser-ts"
npm install --no-audit --no-fund
npm run build

step "the demo's own dependencies (three.js, playwright-core)"
cd "$demo"
npm install --no-audit --no-fund

step 'the host'
exec cargo run --release --manifest-path "$demo/host/Cargo.toml" -- "$@"
