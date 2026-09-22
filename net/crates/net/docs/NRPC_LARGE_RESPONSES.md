# Bounded native unary responses

Native unary callers set request flag bit 6 (`0x0040`) before admission-proof
signing. A supporting server may then split an oversized response. The packet
limit remains 8192 bytes; request bodies and general pub/sub are unchanged.
Small responses retain their existing encoding. A caller without this flag
receives the existing bounded Internal error for an oversized response.

## Fragment contract

Each fragment is an ordinary RESPONSE envelope with the same call identity and
reply route, status Ok, and exactly one header, `nrpc-response-fragment-v1`.
Its six-byte value is the total encoded response length (`u32`, little-endian)
followed by the zero-based fragment index (`u16`, little-endian). Body slices
are 4096 bytes except the final slice. The assembled bytes are the original
encoded response, including its status, headers and body—not just the body.
The fragment header is reserved and cannot be returned by application handlers.

Limits are 1 MiB per encoded response, at most 256 fragments, and 8 MiB of
incomplete response payload storage per caller node, plus bounded bookkeeping.
Lengths and indices are checked before allocation. Identical duplicates and
reordering are accepted; inconsistent totals, contradictory duplicates, invalid
lengths, trailing encoded bytes and nested fragment envelopes are rejected.
An unfragmented error may terminate a partial response; an unfragmented success
cannot replace it. Partial data never reaches an application as success.

Reassembly belongs to the pending unary call and is bound to its expected
authenticated peer and session. Completion, malformed input, cancellation and
deadline cleanup release its reservation. A live call also checks session
retirement and node shutdown every 100 ms, including calls without a deadline.
This checks the receive lifetime, not just the session ID: shutdown may retain
retired sessions in the peer table. Advisory inactivity alone is not retirement.
The normal caller deadline is not restarted for each fragment. No automatic
handler retry occurs.

Fragment sends remain on the request's session and do not fall back to a roster
route. Each credit-admission attempt checks shutdown and receive-lifetime
retirement, so a credit refund cannot revive a retired session's blocked send.
Large native replies do not enqueue individual fragments. The existing handler
task owns one response pump, with at most eight admitted pumps across all
services on a node. Admission failure sends a bounded Internal diagnostic
before any fragment is emitted. Small replies retain the existing bounded
drainer and do not consume these slots. Each admitted response retains at most
1 MiB of encoded fragment backing storage; encoding temporarily also holds the
handler's original response. This is not a bound on application-handler memory.

One monotonic deadline, set at transfer start, covers encoding and every send:
the remaining absolute request deadline, capped at 30 seconds even when the
caller chose no deadline. Each packet's credit stall is also capped at one
second. The fold keeps the call's cancellation token until delivery ends, so
CANCEL remains effective after the handler returns. Cancellation, session
retirement, shutdown, deadline expiry or the first send error stops the pump
and drops all remaining fragments and its admission slot. Old completion cannot
remove a newer call's cancellation entry if the call identity is reused.

Capacity, deadline and send failures attempt one same-session terminal error,
with at most 50 ms for that diagnostic. This is best effort: a dead or stalled
transport may also prevent error delivery. Missing delivery never completes
reassembly; callers should retain a deadline. These sender bounds do not
replace the caller's deadline or promise remote success. Real-network/platform
acceptance remains separate from local two-node and fault-injection witnesses.

The shared fixture at `tests/cross_lang_nrpc/golden_vectors_large_response.json`
pins the encoded response's prefix bytes as `encoded_prefix_hex` (status `u16`,
header count `u8`, body length `u32`, little-endian), plus the fragment envelope
values and the chunking, asserted byte-for-byte in Rust, Node/TypeScript, Python
and Go tests. The latter three are independent layout checks, not mesh
interoperability tests. Native bindings share the Rust implementation; these
tests do not add independent fragment transport support to pure-language
clients.
