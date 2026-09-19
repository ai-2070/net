# Stage 4b repairs — Kyra's HOLD at `871e0138d` (R1–R8)

Packet: `C:/Users/chief/Downloads/KYRA_STAGE4B_REVIEW_PACKET/KYRA_STAGE4B_REVIEW.md`;
scratch probes and the three lane reports in
`C:/Users/chief/AppData/Local/hermes/cache/webrtc-4b-review-871e0138d/`
(`parent-listener-probes.rs`, `parent-candidate-probes.rs`,
`probe-browser-oracle.cjs`, `core-bootstrap-review.md`,
`credential-listener-review.md`, `browser-evidence-review.md`,
`operator-nat-review.md`). Reviewer reproduced her six Rust probes at
`f9ddd2543`: **0/6 pass**; the page's MITM verdict returns `ok: true`
on any exception (`page/app.js:205–217`), confirmed by source.

Credit retained: the production composition (HTTP offer → engine →
completion owner → DataChannel/Noise → provisional install), the
browser wasm over production wire/Noise, operator-PEM TLS, CORS/Origin,
credential mechanics, discovery projections, F7, the impostor *setup*.
Do not rework Stage 3/4a; do not implement the owner-pending §12.5
policies; no Stage 5. One commit per item, prefix
`fix(net): stage 4b repair Rn —`, then `S4B_REPORT.md` §11 with the
per-inverse table. **First commit:** land Kyra's six probes verbatim
into `sdk/tests/rtc_bootstrap_listener.rs` (assertions untouched;
private-constant imports may be exposed via a fixtures-gated re-export)
and pin them in the SDK RTC CI step. Run the full AGENTS.md pre-push
checklist before every push; report back only on green exact-head CI.

## R1 (P1) — an uncredentialed socket can retire another pending dialog

`sdk/src/rtc_bootstrap.rs:714–797`, `mesh.rs:25623–25636`. The trickle
WebSocket checks Origin and parses caller-chosen `node/dialog`; close
retires that tuple from the shared table, closes the endpoint, releases
the budget. Executed: a second plain TCP WebSocket, allowed Origin, no
credential, victim's ids → upgrade succeeded, closing it zeroed the
victim's open-dialog count.

Closure: the offer response returns an **attempt token** (32 random
bytes, minted per accepted offer, stored with the pending dialog; never
in a URL — sent as the first WebSocket frame or a header on upgrade);
trickle and retirement require the token for that exact `(node,
dialog, incarnation)`; unknown tuples are refused at upgrade, not on the
first candidate; a foreign socket's close and candidate injection leave
the victim unchanged (counted); the legitimate owner can trickle and
abandon. Kyra's probe plus the positive control.

## R2 (P1) — the listener's candidate path bypasses the native bounds

`rtc_bootstrap.rs:769–780`, `mesh.rs:25564–25600` vs. the native
decoder/budget at `signal.rs:37–51, 133–149, 216–237`,
`mesh.rs:36306–36350`. Executed via the production hook: an over-limit
`mid` accepted; 65 candidates accepted in one window where the native
`SignalBudget` refuses. Also `accept_bootstrap_offer` charges the shared
authenticated-peer budget under the **unverified** `claimed_node_id`
(`mesh.rs:25505–25514`).

Closure: the HTTP/WS ingress builds the same `RtcSignalMsg` and runs
the same size validation and `SignalBudget::admit` as the native path
(one function, two callers), counted refusals identical; accounting
keyed by a pre-authentication identity the caller cannot choose (the
attempt token's id / the source IP + credential nonce), never another
peer's node id. Witnesses: size negative, frame-budget negative,
attribution (a credential holder cannot spend node X's budget), release
on retirement, normal trickle positive.

## R3 (P2) — recipients can extend their own credential deadlines

`bootstrap_credential.rs:220–237, 302–318, 343–356, 384–425, 590–607`;
`rtc_bootstrap.rs:593–614`. Executed: an expired credential → 400;
editing the two lifetimes to the future → 200 and offer accepted. Domain
consistency binds the PSK to the domain id, not the other fields.

Closure: issuer-owned validity — the credential carries an
**issuer signature** (Ed25519 by the anchor's/operator's issuing key,
whose public half the listener holds) over the canonical bytes
including both deadlines and the nonce; the listener verifies before
anything else. A MAC under the PSK cannot work (recipients hold it).
Witnesses: expiry-only tamper refused; fresh credential accepted;
foreign issuer refused; existing whole-byte tamper test retained. If
the owner prefers advisory deadlines, that is an explicit contract
change to request — not a silent acceptance change.

## R4 (P1/P2) — ACME cold start, cache ownership, validity

- **R4a** `serve_bootstrap` waits for the certificate before binding,
  and the listener is TLS-only, so HTTP-01 can never be served on a
  cold cache. Closure: bind a plaintext `:80` challenge ingress (or a
  configured port) **before** ordering, serve `/.well-known/acme-challenge/`
  from it, then bind TLS; demonstrate a cold-cache order against a
  local ACME directory (pebble in CI, `--features` gated) followed by a
  verifying TLS client. Reconcile the brief's "same listener" with
  HTTP-01's transport requirement in the report.
- **R4b** executed: requesting `different.example` returned the cached
  `localhost` certificate. `read_cache` gets only the directory.
  Closure: cache keyed by domain; qualify SAN and time window before
  reuse; the CLI default cache is per-domain.
- **R4c** one static acceptor, no renewal owner. Closure: a renewal
  task that re-orders at a configured horizon and swaps the acceptor
  (`ArcSwap`/`RwLock`), or an explicit documented rotation lifecycle
  with a reload path — tested either way.

## R5 (P2) — Debug over the raw credential; key file permissions

- **R5a** `OfferRequest` derives `Debug` over the raw credential string
  (`rtc_bootstrap.rs:253–262`); executed: the full PSK-bearing
  credential appears in `{:?}`. Closure: manual redacting `Debug`;
  Kyra's probe as the witness.
- **R5b** `rtc_bootstrap_acme.rs:65–80` writes the private key then
  chmods, ignoring failure. Closure: create with `0o600` from inception
  (`OpenOptions::mode` on Unix; on Windows document the ACL assumption),
  fail hard on protection error; a Unix fault-injection witness
  (`#[cfg(unix)]`, runs in CI).

## R6 (P2) — operator anchor listings have no live mesh source

`cli/src/commands/anchor.rs:229–241`, `cli/src/context.rs:132–139,
221–228`, `deck.rs:551–565, 792–803`, `deck/src/runtime.rs:76–89`. The
CLI context's Deck client starts with `mesh: None`; `rtc_anchors()`
returns empty. Closure: wire the actual collection path (the CLI
context attaches the node it runs, or the supervisor connection it
already uses for other listings); witness: the real CLI command against
an announcing anchor lists it, a plain peer is excluded.

## R7 (P1/P2) — browser witnesses that prove what they claim

- **R7a** missing browser schedules from the brief: hold an old browser
  enrollment through **replacement** (the delayed completion cannot
  promote the successor) and the **bounds** scenario (the already
  accepted per-session bounds, not §12.5 policy).
- **R7b** the MITM verdict: `page/app.js:205–217` returns `ok: true`
  on *any* exception. Closure: the negative test asserts, in order,
  offer accepted (HTTP 200), DataChannel open, Noise constructed and
  msg1 sent, failure **at the pinned-key boundary** (msg2 never
  authenticates / timeout after msg1), and impostor `peer_count`
  unchanged after settlement; an HTTP or ICE failure is a *test error*,
  not a pass. Keep the equivalent correct-key success control.
- **R7c** `runner/src/main.rs:1617–1632, 1663–1669` ignores the local
  Send result and reads an unchanged transit counter as "delivered".
  Closure: the local envelope carries a correlated payload whose
  **decoded local outcome** is observed on the anchor; the inverse
  (suppress `process_local_packet` at `mesh.rs:26579–26581`) goes red.
- **R7d** runner `1428–1453` ignores Send and checks zero handler
  calls. Closure: call-correlated admission/denial evidence at the
  completed boundary (the refusal counter for *that* call id, or the
  typed error the browser receives), no handler execution.
- **mDNS** the measurement PASS can be recorded from a nonempty log
  even if every mDNS-on attempt failed, then admission runs with mDNS
  off. Closure: the mDNS-on pair-formation is its own required step
  with the interface and pair evidence named; the fallback is
  diagnostic and cannot satisfy it.

## R8 (P2) — NAT scenario: target use and an actual run

`tests/natsim/run_scenario.sh:64–76`, `examples/natsim_node.rs`,
`tests/natsim.rs:356–415`. The scenario checks the published `rtc_addr`
and a direct RTC install with zero UDP punches; it does not show the
published address being **selected as the STUN target**, and it has
never run. Closure: the outside client's gather uses the announced
`rtc_addr` (observable: the anchor's STUN responder receives a binding
request from the client and the selected pair's remote is that
address); then obtain the exact-head Linux run in `natsim.yml` and cite
it — or ask the owner, explicitly, to accept the narrower criterion.

## Also record

- `anchor serve` registers no enrollment provider; the harness supplies
  one. Document the operational boundary; do not add a root enrollment
  service without the owner.
- Credentials pinned before an anchor restart are not proven reusable
  (core Noise identity regeneration predates 4b); state it; never "fix"
  by trusting an HTTP-returned key.

## Validation

Kyra's six probes green as committed tests; the browser job with the
corrected MITM/local/protected/mDNS verdicts and the two new schedules;
the pebble cold-start job; the natsim run URL; all RTC binaries and the
SDK RTC step `--no-tests=fail --retries 0`; every inverse
applied-red-reverted at the final head; the full pre-push checklist;
export checker; consumer diff. Reply with the candidate hash, §11, and
the list, only on green exact-head CI.
