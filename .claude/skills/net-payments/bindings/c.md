# C — payments

Read `../bindings.md` first.

## There is no C payments surface

None. Not a header, not a symbol, not a library.

- No `net_payments.h`, and none of the ten shipped headers in
  `net/crates/net/include/` declares a payments function.
- No `net_payment_*` or `net_x402_*` symbol anywhere in the C surface.
- No payments cdylib alongside `libnet_rpc`, `libnet_org`, `libnet_meshdb`,
  `libnet_meshos`, `libnet_deck` and `libnet_mcp_ffi`.

The word "payments" appears once in `net/crates/net/include/README.md` and
nowhere else in the C tree.

Unlike Go, C does not even have a conformance test — there is no C golden-vector
verifier, so nothing pins the canonical-encoding regime from this side.

## This is `not exposed`, not `n/a`

Nothing about C makes a payments API unnatural. The existing C surface already
demonstrates the pattern such a binding would follow: a dedicated header, its
own guard, its own cdylib, an ABI-version handshake like
`net_rpc_check_abi_version`. It has not been built.

Filing it as `n/a` would tell a reader the gap is permanent and stop anyone
asking for it.

## If you need payments from a C process

Put the paid capability behind a Rust, Python or Node provider and have the C
process call it over the mesh as an ordinary capability — the C nRPC surface
(`net_rpc.h`, `libnet_rpc`) and the org auth surface (`net_org.h`, `libnet_org`)
are both available for that. The payment boundary lives where the payment code
is.

## Where to look

- `net/crates/net/include/README.md` — the header and library inventory, which
  is the fastest way to confirm this page is still true.
- `../../net-event-bus/bindings/c.md` — what C *can* do.
