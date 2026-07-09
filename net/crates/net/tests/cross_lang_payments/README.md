# Cross-language payments golden vectors

Same pattern as `tests/cross_lang_mcp/consent_vectors.json`: one fixture,
four verifiers, updated in lockstep. Rust is the source of truth.

| Verifier | Path |
|---|---|
| Rust (source of truth) | `payments/tests/payments_golden_vectors.rs` |
| Node | `bindings/node/test/payments_golden_vectors.test.ts` |
| Python | `bindings/python/tests/test_payments_golden_vectors.py` |
| Go | `go/payments_golden_vectors_test.go` |

## Regeneration

```
cargo run -p net-payments --example gen_payments_fixtures
```

The emitter is fully deterministic (fixed seeds, fixed timestamps, RFC 8032
ed25519), so `git diff` after regenerating is the drift detector. Never
hand-edit `payment_vectors.json`.

## What the vectors pin

- **Envelope canonicalization** — every payment envelope has exactly one
  canonical byte encoding: a single JSON object, all keys sorted bytewise
  (known and unknown fields alike), compact separators, raw UTF-8,
  integers only (floats hard-fail). Signed payload = canonical bytes with
  the top-level `signature` key absent; signatures are ed25519 by the
  envelope's signer identity.
- **x402 byte-preservation** — the captured v2 fixtures under
  `fixtures/x402/v2.0/` must survive every binding byte-identically. x402
  documents travel inside envelopes as base64 of their original bytes,
  never as re-serialized JSON (envelope drift is a rejected-PR bug class).
- **CAIP confusion** — chain/asset id grammar plus distinct-but-confusable
  pairs; comparison is exact and case-sensitive, equivalence is registry
  policy.
- **Decimals mismatch** — registry cross-check: declared-and-mismatched
  decimals hard-reject; unregistered assets hard-reject.
- **Unknown-field preservation** — `payment_quote_with_unknowns` carries
  fields no v1 reader knows; they sort into canonical position and the
  signature covers them (stripping them breaks verification).

## Version pinning

x402 fixtures are pinned per spec revision — `fixtures/x402/v2.0/…`, never
"latest". New spec revisions add fixture sets; they don't replace them.
Pinned revision: `specs/x402-specification-v2.md` in
[x402-foundation/x402](https://github.com/x402-foundation/x402) at commit
`087922a5eecc06ea773636b75df205814ba295b5` (2026-05-29).

## Notes for verifier authors

- Envelope schema keys are ASCII, so every language's default string sort
  agrees with bytewise order. If a vector ever introduces non-ASCII keys,
  sort by UTF-8 bytes explicitly.
- Fixture timestamps stay below 2^53 so JS `JSON.parse` round-trips them
  losslessly. Runtime bindings never re-parse envelope JSON (they carry it
  opaquely), so real nanosecond timestamps are unaffected.
- Python: `json.dumps(v, sort_keys=True, separators=(",", ":"),
  ensure_ascii=False)`. Go: `json.Number` + `SetEscapeHTML(false)` +
  sorted keys. Node: recursive writer over `JSON.stringify` string
  escaping. Keep vector strings free of `\b`, `\f`, and U+2028/U+2029,
  where language escapers legitimately differ.
- **Failure-schematic tolerance** (`failure_schematic_vectors`): each
  language mirrors `FailureSchematic::from_header_bytes` — a full typed
  serde deserialize — accepting iff the header carries the tag AND the full
  schematic shape (required fields + present optionals both type-checked).
- **Duplicate keys are deliberately not a cross-language vector.** Behavior
  splits on whether the repeated key is a **known** field or an unknown
  extension. A duplicate **known** field (`object`, `reason`, …) makes the Rust
  reference (serde) **reject** the header in either order, whereas JS
  `JSON.parse` / Python `json.loads` / Go `encoding/json` silently collapse it
  (last value wins) and would need a bespoke parser to reject — so a known-field
  dup can't be pinned as a shared vector. A duplicate **unknown** key lands in
  the flattened `extra` map, which collapses last-wins in *every* language
  (Rust included), so there the four already agree. Either way it's malformed
  input — producers never emit duplicate keys (the serde serializer cannot) —
  so its handling is unspecified; do not add a vector that depends on it. Rust's
  behavior is pinned by
  `tool_payment::tests::duplicate_known_fields_reject_unknown_extras_collapse`.
