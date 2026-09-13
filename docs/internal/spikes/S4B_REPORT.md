# S4b — bootstrap credential + listener, TLS/CORS/Origin, the anchor surface, mDNS, and the browser harness

Stage 4b of
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
— §5 Layer 0 (the credential), §6 (mDNS host candidates), §11
(registry), and the Stage 4 bullets 4a did not take. Stacked on the
4a second-round head `51e1e011b`, which is green in CI.

Brief: `spikes/S4B_BRIEF.md`. Evidence base: S0b (trickle numbers,
the mDNS finding), S0e (bootstrap frame inventory), S4A_REPORT §12–13
(the §12 contract as landed natively).

**Discipline for this stage, stated up front.** 4b is additive: the
Stage 3 `rtc/` seams and the 4a admission/promotion code are not
modified. Everything 4b needed from the core is a **new** function,
and every one of them is named in §3 below. The one exception is
deliberate and was asked for by the brief: `rtc_bootstrap` on the
announcement now carries the configured listener URL instead of 4a's
synthesised placeholder.

---

## 1. Commits

| Commit | Slice |
|---|---|
| `953d2f3be` | 1 — the browser bootstrap credential (a new format) + `net-mesh anchor credential mint\|inspect` |
| `10af90aae` | 2 — the `rtc-bootstrap` listener over the production dialog path; `rtc_bootstrap` becomes the real URL |
| `1b0c4f876` | 7 — `net-mesh anchor ls\|serve`, the anchor registry, Deck's ANCHOR column, `RtcDriverHandle::selected_pair` |
| `805acc2c0` | 8 — the F7 proxy boundary, witnessed |
| `19c626499` | 6 — the anchor-behind-NAT `rtc_addr` natsim scenario |
| `31968594d` | lint follow-up for the three new surfaces |
| `9bfb86bb7` | CI: the two new RTC binaries get floors and pinned names |
| `54befc493` | the fold's new projections reach every in-crate test constructor |
| `caed88ac9` | 3+4+5 — the Chromium harness: the six §12 witnesses with a real browser, the MITM witness, the mDNS measurement, and the `webrtc-browser` CI job |
| `549e97f15` | this report |
| `32b91909a` | the Deck ANCHOR type no longer splits a doc comment (`empty_line_after_doc_comments`, denied) |

Interleaved on the same branch by the owner while 4b was in flight —
listed so the history reads correctly, not claimed as 4b work:
`fd3c03f09` (one feature graph per test family), `a70b33f17` (the
fold projections' last four constructors), `2a5ac182a` (a bare-mesh
build compiles again — nRPC surfaces cortex-gated), `4f07ae880`
(audit-defect record).

Slices 3, 4 and 5 (the Chromium harness, the MITM witness and the
mDNS measurement) are one body of work in `tests/rtc_browser/`; see
§6.

---

## 2. Exit criteria

| Stage 4 exit criterion | Status |
|---|---|
| Bootstrap credential → offer/answer → DataChannel → Noise against the pinned key → enrollment, with a scripted Chromium client | **Met** — real Chromium 149, executed twice on this host (§6.1) |
| MITM witness, correctly shaped (substitute the responder, not a field) | **Met** — a second anchor with a fresh keypair; inverse RED (§6.2) |
| Anchor behind simulated NAT publishes a working `rtc_addr` | Scenario landed; **not executed** — no netns on this host (§7) |
| Bootstrap budget rejections are typed and fast | **Met** — `BootstrapRefusal` with per-cause HTTP status / WS close code, per-source-IP ceiling checked before the credential |
| The six §12 witnesses | **Met with a real browser** (§6.1); 4a's native ones remain green |
| `rtc_bootstrap` names a real listener | **Met** (`RtcConfig::bootstrap_url`) |
| mDNS host-candidate question answered with a measurement | **Met, and it narrows the plan**: (a) peer-reflexive alone; (b) made things worse here (§6.3) |
| Browser-trusted TLS, no certificate-ignore flag in CI | **Met** — operator PEM and ACME HTTP-01; a real listener + verifying client is witnessed, and its paired negative (empty root store) fails |
| Explicit cross-origin policy, `Origin` validated on the WebSocket | **Met** |
| `net-mesh anchor` CLI + Deck surfaces anchors | **Met** |
| F7 boundary evidence | **Met** |

---

## 3. New core surface (nothing else in the core moved)

All `#[cfg(feature = "webrtc")]`, all new:

| Symbol | Why it exists |
|---|---|
| `MeshNode::accept_bootstrap_offer` | An HTTP-originated Offer has no session to arrive on, so it cannot reach the `0x0D02` engine the way a routed peer does. Everything downstream — the shared dialog table, `handle_signal`, the R4-A completion owner, the fenced install, §12 admission — is the same code. |
| `MeshNode::apply_bootstrap_candidate` | The trickle socket's inbound half, through the same engine path. |
| `MeshNode::bootstrap_host_candidate` | The candidate string `trickle_local_candidate` builds, for a transport that is not `0x0D02`. |
| `MeshNode::end_bootstrap_dialog` | A browser that closes the trickle socket before the channel opens should not hold a budget slot until its deadline. |
| `MeshNode::rtc_max_provisional`, `rtc_public_addr` | Reads the listener needs so it does not re-derive §12 policy. |
| `MeshNode::rtc_anchors`, `DeckClient::rtc_anchors` | The anchor registry (§5). |
| `RtcDriverHandle::selected_pair` | The ICE observable the mDNS measurement needs (§6). |
| `RtcConfig::bootstrap_url` + `with_bootstrap_url` | The real announced URL. |
| `CapabilityMembership::{rtc_bootstrap, rtc_addr}` | Fold projections, `#[serde(skip)]`, exactly like 4a's `noise_pubkey`: filled locally from each node's own ingest, so nothing new travels in the fold envelope. |

The listener itself is **not** in the core. It lives in the SDK
(`net_sdk::rtc_bootstrap`, feature `rtc-bootstrap`) for the reason
the MCP adapter lives outside: the credential is an SDK type, and the
core must not grow an HTTP server. `tests/bootstrap_dep_boundary.rs`
holds that line by reading both manifests.

---

## 4. The credential (slice 1)

A new format, not invite reuse:

```text
NMBC | version:1 | lp(InviteToken) | anchor_noise_pubkey[32]
     | psk[32] | trust_domain[16] | psk_expires_at:u64 | lp(url)
```

- **Two lifetimes, in the format.** The invite nonce is single-use
  and short (`nonce_expires_at`); the PSK is standing and long
  (`psk_expires_at`). `validate_at` enforces both and names which
  half failed — "expired" alone sends an operator to the wrong knob.
- **The trust domain is derived, not labelled.** `TrustDomainId` is
  a domain-separated blake3 of the PSK, so an anchor compares a
  presented credential against the domain of the PSK it actually
  holds. A credential whose stored id is not the one its own PSK
  derives is *malformed*, not merely foreign. The id is one-way, so
  carrying and printing it reveals nothing.
- **The PSK never prints.** `Psk`'s `Debug` redacts, so any struct
  containing one is safe; the credential's `Debug` is hand-written
  and shows every other field.

10 unit witnesses + 8 CLI witnesses; see the inverse ledger.

---

## 5. The listener (slice 2) and the anchor surface (slice 7)

```text
POST /rtc/offer      {credential, node_id, sdp} -> {dialog, sdp, candidate}
GET  /rtc/trickle    WebSocket; candidates both ways as the 0x0D02 JSON
GET  /rtc/anchor     node id, live Noise key, rtc_addr, trust domain, capacity
GET  /.well-known/acme-challenge/{token}
```

- **An HTTP-originated Offer is an Offer**: same dialog table, same
  `SignalBudget`, same completion owner, same Provisional session.
- **The claimed `node_id` is a claim** and is treated as one: it
  rides into the Noise prologue, so a browser that cannot handshake
  as it fails, and §12 holds the session provisional regardless.
- **`GET /rtc/anchor` publishes the live key for comparison.** A
  browser pins the credential's key; if it pinned this response
  there would be no MITM protection at all, which is why the MITM
  witness substitutes the responder.
- **TLS is browser-trusted or nothing.** Operator PEM or ACME
  HTTP-01 on the same listener, order cached on disk. No
  self-signed variant exists, and no ignore-certificate flag exists
  on either side. rustls is the 0.23/ring version `net-payments`
  already pins, built with an explicit provider and never installed
  process-globally; `cargo tree -d` shows one rustls.
- **CORS is `AllowOrigin::list`.** A single statically echoed origin
  is a wildcard wearing one origin's name — the inverse ledger has
  that mutation.
- **`Origin` on the trickle socket is a route layer**, so an axum
  extractor rejection cannot answer a foreign origin before the
  check runs.
- **Budget**: per-source-IP ceiling checked *before* the credential
  (cheapest check, and the one an attacker is spending), plus the
  §12 provisional bound read from the node.

The operator surface: `net-mesh anchor credential mint|inspect`
(default build), `anchor ls` (feature `webrtc`), `anchor serve`
(feature `rtc-bootstrap`, which refuses to start without a
browser-trusted certificate path and requires `--allow-origin`).
Deck grows an ANCHOR column fed by the same registry.

---

## 6. Chromium harness, MITM, mDNS (slices 3–5)

`net/crates/net/tests/rtc_browser/` — a wasm leaf over the
**production** `net-mesh-wire` crate, a page, and a native runner
that issues the certificate, starts every node and listener,
launches Chromium and drives the witnesses. One command
(`run.ps1` / `run.sh`), exit 0/1. Landed as `caed88ac9`.

**Verified twice on this host**: by the harness author and
independently by me. `9 witness(es), 0 failed`, exit 0, against
headless Chromium 149.0.7827.55.

### 6.1 The six §12 witnesses, with a real browser

Each observable is on the **anchor**; the browser is the thing under
test, not the reporter.

| Witness | Anchor observable |
|---|---|
| `enrollment_exchange_promotes_this_session` | `admission_promoted` +1 **and** the same `peer_session_id` still installed. `peer_is_provisional == false` alone is not accepted — a reclaimed session is also not provisional. The browser itself decrypted the `Admitted` `JoinOutcome`. |
| `provisional_announcement_is_refused` | `admission_refused_announce` 0 → 1, on a **real** signed announcement taken from a live identity node, so it is not refused for a node-id mismatch. |
| `provisional_subscribe_to_an_unrelated_channel_is_refused` | `admission_refused_subscribe` 0 → 1. Its control is the enrollment witness, where the SAME codec's subscribe to the peer's own reply channel is ACKed. |
| `provisional_call_to_another_service_is_refused` | `admission_refused_deliver` 0 → 1 **and** a real registered `app.orders.place` provider ran 0 times. |
| `provisional_transit_is_refused` | `admission_refused_transit` 0 → 1 for a third-party `dest_id`. |
| `local_envelope_accepted_redirected_denied` | Same session, same codec, two envelopes in order: local leaves the counter unchanged, redirected moves it. The ordering is the point — S0e's named attack is that `relay_addr` and `dest_node_id` are independent. |
| `enrolled_without_authority_is_still_denied` | On the session the first witness promoted, a real `serve_rpc_protected(OwnerDelegated)` provider behind a real installed `NodeAuthority` ran 0 times. |

### 6.2 MITM

A second anchor with a **fresh Noise keypair**, its own real
listener, the same certificate and the same PSK — so the credential
is accepted, the offer answered and the DataChannel opened — and the
browser then handshakes against the key the **credential** pins.
Result: the handshake fails (`timeout: noise msg2`, the impostor
cannot open msg1) and the impostor installs nothing
(`peer_count` 0 → 0, provisional 0 → 0).

Inverse `--inverse mitm-pins-the-live-key`: the browser pins the
impostor's own key → witness RED, impostor `peer_count` 0 → 1,
harness exit 1. The witness can fail, and what it detects is a
successful impostor install.

### 6.3 mDNS (§6's question), measured

Chromium runs **without** `--disable-features=WebRtcHideLocalIpsWithMdns`
and does publish obfuscated candidates —
`candidate:… a88c4a5b-…-b95b.local 65488 typ host` — captured from
the page.

| Configuration | Result |
|---|---|
| loopback, no `iceServers` | **PAIR FORMED, 14–27 ms.** Browser `getStats`: local `prflx`, remote `host 127.0.0.1:59200`, nominated. Anchor `selected_pair`: `learned=peer-reflexive`. |
| real interface (192.168.50.161), no `iceServers` | **PAIR FORMED, 326–334 ms.** Anchor `selected_pair`: `learned=peer-reflexive`. |
| loopback **with** the anchor's own STUN as `iceServers` | **NO PAIR.** Chromium gathers a real `srflx` candidate, then `checking` → `disconnected`. |
| real interface **with** the anchor's STUN | **NO PAIR**, same shape. |

**The answer is (a), peer-reflexive, alone.** The anchor accepting
the peer-reflexive candidate it learns from the browser's inbound
binding request is sufficient on both loopback and a real
interface, and **(c) an mDNS client on the anchor is not needed**.

This **narrows the plan**: §6 expected "(a)+(b)". Option (b),
server-reflexive gathering against the anchor's own Stage 3 STUN
responder, is *available* — `serve_stun` demonstrably works, the
browser gathers a real srflx candidate against it — but on this
host adding it **prevented** the pair that forms without it, in
both run orders. That is reported as an observation with the
candidate lines that back it; it was **not root-caused** inside
str0m 0.23.1, and no claim about the cause is made here.

Anchor-side limitation, restated: `selected_pair` reports the
address ICE transmits to and whether it was ever signalled. Every
candidate **type** in this section is the browser's own
`getStats()`.

### 6.4 CI

A `webrtc-browser` job: Chromium from Playwright's cache,
`libnss3-tools`, `wasm-bindgen-cli@0.2.128` (equal to the leaf's
`=0.2.128` pin), then the harness, then a witness-inventory step
with the same discipline as the other RTC jobs — a parser
self-check on a known PASS line, zero `RTCB FAIL`, a floor of 9,
and all nine names pinned by exact grep. The mDNS evidence goes to
the job summary and the log is uploaded.

## 7. Named gaps

1. **The natsim scenario has never run.** Windows host, no network
   namespaces. What ran: `cargo check --examples` with and without
   `webrtc`; a temporary cfg relaxation to typecheck the new
   `#[ignore]`d test under both feature sets (restored verbatim);
   `bash -n` on both scripts; and the two no-root validation guards,
   executed for real. CI's `natsim.yml` is where it runs.
2. **ACME ordering has never run against a live directory** — no
   pebble here. The half that lives in this crate (the HTTP-01
   challenge route, the on-disk cache) is witnessed; the order is
   code review only.
3. **`selected_pair` cannot report a candidate *type*.** str0m
   0.23.1 exposes no nominated pair and emits no event for one
   (checked in `str0m-0.23.1` and `is-0.11.0`), so the anchor-side
   observable is "the address ICE transmits to, and whether we were
   ever told about it" (`signalled` vs `peer-reflexive`). The
   candidate type comes from the browser's own `getStats()`.
4. **The §12.5 policy items remain owner-pending** and were not
   implemented, per the brief: install-time `max_provisional`, the
   aggregate bootstrap-byte ledger, an enrollment deadline that
   cancels its handler, subscribe bounds, incarnation-keyed signal
   queues.
5. **The `webrtc-browser` CI job has never executed.** Only the
   Windows path ran here. Unverified until CI runs it: the Linux
   Chromium path Playwright installs, the `certutil -d sql:$HOME/.pki/nssdb`
   NSS install being picked up by that build, and whether a runner
   exposes a routable non-loopback IPv4 (if not, the harness skips
   the interface probes and says so rather than failing).
6. **The srflx/no-pair finding is an observation, not a diagnosis.**
   Nobody instrumented str0m to learn why a signalled
   server-reflexive candidate prevents a pair that forms without one.
7. **The browser is not an nRPC client.** Frame payloads are built
   natively with the production encoders; the browser owns the
   transport. Stage 5's `net-leaf` is where that changes. These
   witnesses therefore exercise the anchor-side §12 gates — which is
   what §12 is — and not a browser nRPC client's own behaviour.
8. **The mDNS measurement covers Chromium 149 headless on Windows
   only.** Firefox and Safari are untested, and the "real interface"
   run had no NAT between browser and anchor (that is exit criterion
   6's territory).
9. **`anchor ls` / `anchor serve` are feature-gated off by default**,
   so they do not appear in a stock `net-mesh --help`. That is
   deliberate (the default CLI carries no HTTP server), and it means
   the help-surface tests do not cover them.

---

## 8. Inverse ledger

Every mutation applied at the head that carries the slice, the named
witness run, then the tree restored.

| Inverse | Witness | Result |
|---|---|---|
| Listener answers the offer without the production dialog path | `an_http_offer_reaches_the_production_dialog_path_and_is_answered` | RED |
| Trust domain unchecked | `a_credential_from_another_trust_domain_is_refused_before_any_dialog` | RED |
| WebSocket `Origin` layer disabled | `the_trickle_socket_refuses_a_foreign_origin_before_upgrading` | RED |
| CORS echoes one static origin to everyone | `cors_names_one_origin_and_never_a_wildcard` | RED |
| Per-source-IP rate limit removed | `the_per_source_ip_rate_limit_refuses_typed_and_early` | RED |
| `axum` declared non-optional | `the_http_stack_is_optional_and_reachable_only_from_rtc_bootstrap` | RED |
| Fold drops the `rtc_addr` / `rtc_bootstrap` projections | `an_announced_anchor_is_listed_with_its_addresses_and_a_plain_peer_is_not` | RED |
| `rtc_anchors` ignores the `rtc-anchor` tag | same | RED |
| Deck's ANCHOR cell drops the `rtc_addr` preference | `the_anchor_cell_prefers_the_rtc_socket_then_the_bootstrap_host` | RED |
| Browser pins the impostor's live key instead of the credential's (`--inverse mitm-pins-the-live-key`) | `mitm_anchor_fails_the_handshake_and_installs_nothing` | RED (impostor `peer_count` 0 → 1) |
| The enrollment REQUEST is never sent (`--inverse skip-enrollment-request`) | `enrollment_exchange_promotes_this_session` **and** `enrolled_without_authority_is_still_denied` | RED ×2 |

Credential-format inverses are inside the witnesses themselves: the
tamper test flips **every byte** of the encoding and requires each
flip to either fail parsing or change the credential, and the
truncation test cuts at every length.

---

## 9. Validation at the final head

The AGENTS.md pre-push checklist, not `--lib`-shaped substitutes —
three CI reds on this branch came from targets a `--lib` or
`--test`-scoped command never builds (test constructors, a bench,
a lint that only fires on `--all-targets`).

| Command | Result |
|---|---|
| `cargo fmt -p <each member> -- --check` | pass. **`cargo fmt --all -- --check` cannot run on this host** — `os error 206`, the argument list is too long — so it ran per package; CI's `Format` job is the authority and is green |
| `cargo check --workspace --all-targets` | 0 errors |
| `cargo clippy --all-features --all-targets` with CI's `-A` set | 0 |
| `cargo clippy --features webrtc --lib --bins -- -D warnings` | 0 |
| `cargo clippy -p net-mesh-sdk --features rtc-bootstrap --lib -- -D warnings` | 0 |
| `cargo clippy -p net-deck --all-targets` / `--features webrtc --all-targets` | 0 / 0 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | 0 |
| `cargo test --lib --features "$UNIT_FEATURES"` / `+ webrtc` | **5779** / **5811** passed, 0 failed |
| Twelve RTC binaries, `--no-tests=fail --retries 0` | **106 run, 106 passed** |
| `sdk --features rtc-bootstrap`: `rtc_bootstrap_listener` + `bootstrap_dep_boundary` | 13 passed |
| `sdk --features net`: credential unit witnesses | 10 passed |
| `net-cli --test anchor_credential` | 8 passed |
| `sdk --test org_exact_sensing` (the repaired OA-6 binary) | 22 passed |
| Browser harness (real Chromium 149, Windows) | **9 witnesses, 0 failed**, exit 0, run twice |
| `cargo tree -d` for `-p net-mesh-sdk --features rtc-bootstrap` | no duplicate `rustls` / `axum` / `hyper` / `aws-lc` |

Per-binary counts vs CI floors: 5 / 7 / 22 / 4 / 8 / 5 / 2 / 12 / 26 / 11 / 2 / 2 = 106; every floor met, the two new binaries pinned.

---

## 10. CI — green

**Head `490568ecd`, run
[34737303326](https://github.com/ai-2070/net/actions/runs/34737303326):
52 jobs, 52 success.** That includes the **first execution of
`webrtc-browser`**, on Linux, which reports `rtc_browser: 9 passed,
0 failed (floor 9)` and `browser witness inventory complete` — so
the Stage 4b exit criteria are witnessed by CI, not only on the
author's host.

The mDNS measurement reproduced on the runner and **agrees with
§6.3 on a completely different network**:

```
[mdns] loopback/anchor-stun: NO PAIR — timeout: datachannel open
[mdns] loopback/no-stun:     PAIR FORMED in 20 ms; getStats local type=prflx;
                             anchor learned=peer-reflexive
[mdns] interface/no-stun:    PAIR FORMED in 18 ms (10.1.0.23);
                             anchor learned=peer-reflexive
```

Peer-reflexive alone, on both loopback and a real interface, with
mDNS obfuscation on — and the anchor's own STUN as an `iceServer`
again prevents the pair. Two hosts, two networks, same answer: plan
§6's "(a)+(b)" narrows to **(a)**.

### 10.1 What was red, and why — the record

Seven distinct breaks over the stage, each fixed at its cause, none
waived:

| Red | Cause | Fix |
|---|---|---|
| `E0063` in `src` test modules | the two new fold projections are fields ~25 `#[cfg(test)]` sites construct literally | `54befc493` |
| `E0063` in `tests/` | same, four more constructors | owner's `a70b33f17` |
| `E0063` in `benches/` | same, three benches — **no `--lib` or `--test` command builds a bench** | `fde741901` |
| `empty_line_after_doc_comments` (Deck) | a new type inserted between an enum's doc comment and the enum | `32b91909a` |
| `disallowed_methods` + single-arm `match` (F7 witness) | `std::sync::Mutex` where the workspace uses parking_lot | `5e6beb255` |
| `Format` | the same commit's import order, fixed locally by a later `cargo fmt` and never committed | `490568ecd` |
| `run.sh` / `check-witness-results.py` `Permission denied` | committed `100644`, run by path | `490568ecd` |
| `Rust SDK tests`: "no JUnit artifact" | the job runs from `net/crates/net/sdk`, but a workspace **member has no target dir** — nextest writes under `net/crates/net/target` | `490568ecd`, via `cargo metadata`'s `target_directory` |

Two witnesses were **load-dependent, not flaky**, and both were
diagnosed rather than retried:

- `a_missing_canonical_member_is_recovered_under_an_unchanged_expectation`
  (OA-6) rejection-sampled a node id below a random minimum with a
  fixed budget. The honest failure rate is `40/4136` ≈ **1 % per
  run**, not the naive `(1-1/41)^4096`: the floor is itself a
  minimum with a heavy tail. Now it draws the pool and *takes* its
  minimum — below-the-crowd by construction (`7f62646d5`).
- `routed_then_direct_then_loss_then_manually_restored_routed`
  failed with the initiator's `handshake timeout`. The cause is that
  the quiescence gate is evaluated **by each side independently**,
  and the witness waited only on A's view; on a loaded runner B's
  ack was still in flight, so B refused the upgrade as "busy". It
  now waits on both ends and names which end was busy on failure
  (`90dd0e36c`). No wait was widened in either fix.
