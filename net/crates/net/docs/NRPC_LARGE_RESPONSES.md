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
retirement every 100 ms, including calls without a deadline. The normal caller
deadline is not restarted for each fragment. No automatic handler retry occurs.

Fragment sends remain on the request's session and do not fall back to a roster
route. The sender can wait up to one second for packet credit before declaring
a send failure; this is a credit-stall bound, not a new application deadline.
The existing bounded response drainer can still overflow under load; failed or
missing delivery cannot complete reassembly and remains subject to the caller's
deadline. Queue-pressure/fault-injection and real-network acceptance remain
separate from the successful local two-node size tests.

The shared fixture at `tests/cross_lang_nrpc/golden_vectors_large_response.json`
pins the byte layout in Rust, Node/TypeScript, Python and Go tests. The latter
three are independent layout checks, not mesh interoperability tests. Native
bindings share the Rust implementation; these tests do not add independent
fragment transport support to pure-language clients.
