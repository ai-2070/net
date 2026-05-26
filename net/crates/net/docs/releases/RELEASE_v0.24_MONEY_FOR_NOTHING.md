# Net v0.24 — "Money For Nothing"

*Named after the Dire Straits track that opened side two of Brothers in Arms in 1985 — the one Mark Knopfler wrote standing in a New York appliance store listening to a delivery guy heckling MTV, the one Sting flew in to harmonize the "I want my MTV" hook over a single take. Same wire, same semantics, same surface — money for nothing, and the bytes for free.*

## One audit pass, two wins, one binary that finally stopped paying for what it wasn't using

The v0.24 release is the result of one perf-audit pass against the nRPC hot path, one binary-size audit against the napi release artifact, and one structural follow-through on a footgun the size audit uncovered. The audit pass found two systemic costs that every nRPC call paid by default: a per-packet `tokio::spawn` + AEAD encrypt + `sendto` to emit a `StreamWindow` grant on each accepted inbound packet, and a per-reply roster lookup + ACL check + subnet filter + per-recipient `Vec<Bytes>` fan-out for response legs that already knew the caller. The size audit found `regex` — pulled into every binding's release artifact by an unread `compiled_patterns` field and a single matcher variant that most consumers never touched — was costing ~1.10 MiB on the napi `win-x86_64` cdylib alone, and that the feature-disabled fallback silently returned empty matches.

The release's organizing observation: when the substrate emits work that the caller has already computed, the work is free to skip. The grant drainer skips the per-packet `spawn` / encrypt / `sendto` by coalescing every `(session_id, stream_id)` grant in a per-mesh map that a single drainer task drains on a 1 ms tick. The direct-response fast path skips the roster lookup by caching the AEAD-verified `from_node` at the bridge layer and routing the reply through `publish_to_peer` directly. And the `regex`-gate skips the binary cost entirely for consumers who don't construct a `Regex` matcher — and when they do construct one against a regex-less build, the binary now tells them so loudly instead of silently returning nothing.

Below: the wins, grouped by where they fire.

---

## nRPC perf — drainer-batched grants, direct-send responses

`PERF_AUDIT_2026_05_19_NRPC.md` (the audit doc shipping alongside this release) flagged two costs that hit every nRPC call regardless of payload shape, contention, or transport. v0.24 closes both.

**T1.1 — StreamWindow grant batching via a per-mesh drainer.** The receive path previously emitted one wire grant per accepted packet: a `tokio::spawn` + AEAD encrypt + `sendto` round-trip per inbound packet, even for unary RPC where the response leg would have replenished credit on its own. v0.24 decouples emission from the receive path entirely:

- Per-`MeshNode` state: `pending_stream_grants: Mutex<HashMap<(session_id, stream_id), PendingStreamGrant>>` + a single `Notify`.
- Receive path now does one lock + insert + `notify_one` per accepted packet — no `spawn`, no encrypt, no `sendto`.
- A per-mesh drainer task (`spawn_stream_grant_drainer_loop`) wakes on the `Notify` or on a 1 ms safety-net interval, swaps the map out with `std::mem::take`, and emits one wire grant per unique `(session_id, stream_id)`.
- Same-key receives between drain cycles overwrite the value (latest-wins). Grants are **authoritative** — every emission carries the receiver's full `total_consumed` — so the latest entry subsumes every pending earlier one and the drainer never undercounts.

Supersedes a threshold-coalesce attempt (c38f01f5) whose `RxCreditState::take_pending_grant` heuristic deadlocked any sender configured with a `tx_window` smaller than the receiver's coalesce threshold. The receiver auto-creates streams with `DEFAULT_STREAM_WINDOW_BYTES` (64 KiB) regardless of the sender's config, so a sender opening a 512-byte stress stream (`sdk/tests/mesh_stream_backpressure.rs`) would stall waiting for a grant that wouldn't fire until 32 KiB of consumption. The drainer pattern has no threshold — every accepted packet enqueues, every drain cycle emits — so the deadlock can't recur.

**T1.2 — Direct-send RPC responses via `publish_to_peer`.** The four reply emit sites (unary, server-streaming, client-stream terminal, duplex chunks) previously built a `ChannelPublisher` and called `mesh.publish`, which runs the roster lookup + ACL check + subnet filter + per-recipient `Vec<Bytes>` alloc fan-out path before forwarding to `publish_to_peer` anyway. The response leg already knows the caller from the AEAD-verified inbound `from_node`.

- Per-service `RpcOriginNodeCache` (`Arc<DashMap<origin_hash, from_node>>`) populated by the bridge from the inbound `from_node` at REQUEST receive time.
- New `publish_response_to_caller` helper consults the cache, then falls back to the mesh's global origin-hash reverse index, then finally to `mesh.publish` — preserving correctness for loopback / test paths that emit with `from_node==0`.
- Applied to every reply shape: unary, server-stream chunks, client-stream terminal response, duplex chunks.

**Benchmarks (May 19 audit hardware, 14900K, c128 client mesh):**

| benchmark            | baseline | v0.24 (drainer + direct-send) | delta |
|----------------------|----------|-------------------------------|-------|
| `nrpc_qps` c1/32B    | 69.6 µs  | 42.5 µs                       | -39%  |
| `nrpc_qps` c128/32B  | 1.84 ms  | 1.12 ms                       | -39%  |

Per-RT, T1.2 alone clips ~3-8 µs off the response publish path; the rest of the win comes from T1.1's elimination of the per-packet grant overhead. The c128 case scales the win proportionally because the saved syscalls compound across concurrent in-flight RPCs.

**Test posture.** All 36 nRPC integration tests + 41 session unit tests + the previously-failing `sdk/tests/test_sdk_send_with_retry_succeeds_through_backpressure` are green. The drainer is exercised under both the small-window stress path (the test that deadlocked the v1 threshold-coalesce approach) and the c128 saturation path.

---

## `regex` is now an opt-in Cargo feature (-1.10 MiB on every binding artifact)

`regex` was unconditional in `Cargo.toml` and pulled in by every consumer of `net-mesh` — the Node/Python/Go bindings, the CLI, downstream SDK users. Two consumers held references:

- `behavior::safety::SafetyEnforcer::compiled_patterns` — held but unread (marked `#[allow(dead_code)]`); the safety enforcer never wired the pre-compiled pattern fast-path that field was reserved for.
- `behavior::fold::capability_aggregation::TagMatcher::Regex` — live, but the variant is one of six matcher kinds; consumers who never construct a Regex matcher pay the binary cost for zero functional benefit.

The cost is non-trivial: **~1.10 MiB** on the napi `win-x86_64` release artifact (9.49 MiB → 8.39 MiB after gating). The same delta lands on every binding (Python wheel, Go cdylib, C ABI, CLI).

v0.24 makes `regex` optional and gates the live usage:

- `Cargo.toml`: `regex = { version = "1", optional = true }`; the previously-empty `regex = []` alias becomes `regex = ["dep:regex"]`.
- `capability_aggregation.rs`: the wire-format `TagMatcher::Regex` variant stays in the enum unconditionally — peers exchanging serialized matchers must keep working regardless of the receiver's feature set. The `CompiledMatcher::Regex` arm and its `matches_one` branch gate on `#[cfg(feature = "regex")]`.
- `safety.rs`: the `compiled_patterns` field and its initializer gate on the feature.

Consumers who want regex matching turn it on:

```sh
cargo add net-mesh --features regex
```

The Node / Python / Go bindings re-export the feature through their own feature lists; downstream binding consumers flip it in their `package.json` / `pyproject.toml` / Go build tags the same way they flip every other binding feature.

---

## `TagMatcherError::RegexNotBuiltIn` — explicit error, no more silent empty

The first cut of the `regex` gate routed `TagMatcher::Regex` to `MatchesNothing` on regex-less builds. Compiled cleanly, preserved the existing "invalid pattern → matches nothing" fail-closed contract — and silently returned empty results that looked indistinguishable from "no entries match this pattern." Operators couldn't tell whether their query was wrong or the binary couldn't evaluate it.

v0.24 replaces the silent fallback with a structured error and a loud panic at the call site:

- **New `TagMatcherError::RegexNotBuiltIn { pattern }`** carries the offending pattern + an actionable Display message ("Rebuild with `--features regex` or use a different matcher").
- **New `TagMatcher::validate(&self) -> Result<(), TagMatcherError>`** for proactive callers (RPC handlers, language-binding constructors, CLI parsers) that accept user-supplied matchers and want structured failure surfacing.
- **`compile()` panics** on the regex-less-build + `Regex`-variant combo with the same Display message. Callers that skipped `validate()` see the build-time-config mismatch loudly at first use rather than silently for the lifetime of the deployment.

Wire format is unchanged: `TagMatcher::Regex` stays in the enum unconditionally so peers can still exchange it. The doc on the variant calls out the gate and the validate-first contract.

Two new tests pin the behavior under `#[cfg(not(feature = "regex"))]`:

- `matcher_regex_without_feature_validate_returns_explicit_error` — surfaces the structured error.
- `matcher_regex_without_feature_aggregate_panics_with_actionable_message` — surfaces the panic message.

The existing `matcher_regex_with_invalid_pattern_matches_nothing` test runs under `#[cfg(feature = "regex")]` only — its premise (regex crate compiles and then rejects a bad pattern) requires the feature.

---

## `async-nats` 0.49 — `PublishErrorKind::MaxPayloadExceeded` classified as fatal

The Renovate-driven `async-nats` 0.23 → 0.49 bump added a new `PublishErrorKind::MaxPayloadExceeded` variant, which broke the exhaustive match in `JetStreamAdapter::is_transient_error`. v0.24 classifies the variant as fatal alongside `StreamNotFound` and the `WrongLast*` family — oversized payloads will not become recoverable on retry, and retrying would loop until an operator intervenes (the same production-down scenario that drove the `Other → fatal` classification in v0.20.2).

No SDK-surface changes. The classification matrix grows one structural-fatal row:

```
                              transient?  retry?
TimedOut                          yes       yes
BrokenPipe                        yes       yes
MaxAckPending                     yes       yes
StreamNotFound                    no        no
WrongLastMessageId                no        no
WrongLastSequence                 no        no
MaxPayloadExceeded   (new)        no        no
Other                             no        no  (logged before return)
```

Operators who hit the new variant in production logs are getting a hard signal that a producer is exceeding the stream's `max_msg_size` config — the fix is upstream (chunk the payload, raise the stream limit), not in the retry loop.

---

## Test hygiene

- **Perf-audit doc shipped in tree.** `docs/plans/NRPC_FLAMEGRAPH.md` lands alongside the perf wins — the flame-graph methodology, the 14900K bench rig config, and the before/after numbers are pinned in the repo so the next perf pass starts from a known reference frame.
- **The previously-failing backpressure test now passes.** `sdk/tests/mesh_stream_backpressure.rs::test_sdk_send_with_retry_succeeds_through_backpressure` was deadlocked under the v1 threshold-coalesce approach (small-window sender + receiver's default 64 KiB stream → no grant ever fired). The drainer pattern has no threshold, so the test passes.
- **`cargo clippy --features meshos,deck,aggregator --all-features --all-targets -- -D warnings` clean.** Strict floor from v0.20.2 stays armed across the feature-flagged regex split.
- **`cargo doc --features meshos,deck,aggregator --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`.** Intra-doc links across the new `TagMatcherError`, `validate()`, and `compile()` panic docs all resolved.
- **Feature-matrix CI.** Both the default (regex-on, matches every existing binding default) and the regex-off path (consumers who explicitly disable regex to trim binary size) run their unit + integration suites. The two regex-off-only tests run only in the regex-off job; the live-regex test runs only in the regex-on job.
- **Codecov coverage** unchanged in posture — ~90% substrate, informational on CI status.

---

## Breaking changes

### `regex` is no longer pulled in by default for direct `net-mesh` consumers

`Cargo.toml` flips `regex` from an unconditional dep to `optional = true`. The `regex = []` feature alias becomes `regex = ["dep:regex"]`. Direct `net-mesh` consumers who relied on transitive access to the `regex` crate via `net-mesh` need to depend on `regex` directly. Downstream consumers who construct `TagMatcher::Regex` must enable `--features regex` (or the matcher will return a structured error / panic per the new contract below).

The Node, Python, and Go binding default feature sets include `regex` — most users see no behavior change unless they intentionally trim the binding's feature list.

### `TagMatcher::Regex` on a regex-less build now errors or panics instead of silently matching nothing

The previous regex-feature-off fallback was `MatchesNothing` — invisible to the caller. v0.24 surfaces it:

- Callers using `TagMatcher::validate(&matcher)?` get `TagMatcherError::RegexNotBuiltIn { pattern }`.
- Callers who skip validation and pass the matcher straight to `Fold::aggregate` / `Fold::capacity_ranking` get a panic with the same actionable message.

The previously-pinned "invalid pattern matches nothing" contract still holds **with the feature on** — an invalid pattern (e.g. unbalanced parens) compiles to `CompiledMatcher::Regex { re: None }` and matches nothing, exactly as before. The behavior change is strictly for the feature-off path.

### `JetStreamAdapter::is_transient_error` classifies `MaxPayloadExceeded` as fatal

Wire-shape compatible — the `PublishError` envelope is async-nats's own type. Behavior change: an oversized publish previously hit the catch-all branch (and now panics at the exhaustive-match compile error if anyone has been pinning async-nats 0.49 without the variant arm). v0.24 classifies it as fatal so the retry loop terminates and the underlying misconfig surfaces.

### Per-packet `StreamWindow` grant emission is gone (internal-only break, observable as wire-rate)

The receive path no longer fires one grant per accepted packet. Operators watching wire traffic with `tcpdump` see fewer grant control packets per RPC — on a unary call, typically one terminal grant instead of N (one per inbound packet). Grants remain authoritative on every emission, so backpressure semantics are unchanged from the caller's perspective.

### Direct-send response routing falls back to `mesh.publish` only when no peer hint resolves

Internal-only break. The four `serve_rpc_*` reply emit sites now consult a per-service `RpcOriginNodeCache` and fall back to a global origin-hash reverse index before reaching `mesh.publish`. Loopback / test paths that emit with `from_node==0` still resolve through `mesh.publish` as before; production paths get the direct route. Downstream consumers who hooked the `mesh.publish` path for response-leg telemetry will see fewer events from that hook on the response side.

### `STREAM_GRANT_DRAIN_INTERVAL` is a new private constant

Hard-coded to 1 ms. Not exposed as a tunable. The constant lives in `adapter/net/mesh.rs` and is documented inline alongside the drainer; a future config tunable is a one-line plumbing change against the constant's call site.

### `PendingStreamGrant` is a new private struct

Internal to `adapter/net/mesh.rs`. Captures the AEAD session (cipher + packet pool + `next_control_tx_seq`) and the peer's wire address. Not exported; the per-binding APIs are unchanged.

---

## How to upgrade

1. **Rust consumers — update the dependency to `0.24`.** No source changes required unless you (a) construct `TagMatcher::Regex` directly and don't enable the `regex` feature, or (b) match exhaustively on `PublishErrorKind` in your own code. The former: add `validate()` ahead of compile, or enable the feature. The latter: add `MaxPayloadExceeded => ...` to your match.

2. **Operators with `TagMatcher::Regex` in their query mix — pin the `regex` feature explicitly.** Direct `net-mesh` consumers: `cargo add net-mesh --features regex`. Bindings: enable the binding's `regex` feature flag (Node + Python + Go default to on; downstream wrappers may differ). The structured `TagMatcherError::RegexNotBuiltIn` shows up in `validate()` results when the build is wrong; the panic at `Fold::aggregate` shows up at first use when validation is skipped.

3. **Operators with binary-size budgets — flip `regex` off explicitly.** The napi `win-x86_64` artifact drops 1.10 MiB. Downstream binding builds: pass `--no-default-features` and enumerate the features you do want (substitute the binding's own default list minus `regex`). Verify by grepping `cargo tree -e features` for `regex` — it should not appear.

4. **Operators watching `nrpc_qps` benchmarks — expect the -39% delta to land out of the box.** No config knobs to flip. The drainer's 1 ms interval is hard-coded; the response cache populates automatically on first inbound request per origin.

5. **Operators on async-nats 0.49 or later — the `MaxPayloadExceeded` classification fix is automatic.** A producer hitting the variant gets a hard fatal in the logs (look for the existing `JetStream publish` error tracing); previously this same path would loop forever silently as part of the catch-all. Upstream fix: chunk the payload or raise the stream's `max_msg_size`.

6. **Downstream consumers who hook `mesh.publish` for response-leg telemetry — re-wire to the substrate observer.** The four reply sites now bypass `mesh.publish` on the production path. The substrate observer surface from v0.23 (`setObserver` / `set_observer` / `SetObserver`) fires on every RPC reply and is the supported way to observe the response leg.

7. **No CI config change required.** Strict clippy floor stays armed; rustdoc warnings stay denied; the feature-matrix job runs both regex-on and regex-off paths. The Renovate config tracks async-nats minor bumps; future bumps that add `PublishErrorKind` variants will fail the exhaustive match at compile time, exactly as 0.49 did.

8. **Operators — bump the binary.** Pre-built `net-mesh`, `net-deck`, `net-aggregator-daemon` archives land for every supported target (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64). Wire format is unchanged from v0.23; mixed-version fleets handshake cleanly and the v0.23 `TypedMeshRpc.Regex` variant transmitted from a regex-on peer to a regex-off peer surfaces as the structured error on receive instead of silent empty.

9. **Downstream Go binding consumers — ABI version unchanged.** `NET_RPC_ABI_VERSION` stays at `0x0004`. No symbol additions in this release.

---

Released 2026-05-26.

## License

See [LICENSE](../../LICENSE).
