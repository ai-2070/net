# Stage 4b — bootstrap listener, browser credential, TLS/CORS/Origin, Chromium harness, mDNS

Plan: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` —
Stage 4 (the bullets 4a did not take), §5 Layer 0 (the credential),
§6 (mDNS host candidates), §11 (registry: `rtc-anchor`, `rtc_bootstrap`,
`rtc_addr`), §12 (admission — already implemented in 4a; 4b consumes it,
does not rework it). Spikes S0b (trickle numbers, mDNS finding) and S0e
(bootstrap frames) are the evidence base. Read all of those before code.

Authorization: the product owner authorizes 4b **implementation** on the
`LZL0/webrtc-transport` branch, stacked on the 4a second-round head
(`091c99424`) while Kyra reviews it. Same rules as every stacked stage:
strictly additive, new modules and feature-gated arms, so a 4a re-review
fix cannot conflict. Stage 3 `rtc/` seams and the 4a admission/promotion
code are **not yours** to change; if 4b needs a hook there, add it as a
new function and say so in the report. One commit per numbered slice
below, prefix `feat(net): stage 4b —`. Report
`docs/internal/spikes/S4B_REPORT.md` with the exit-criteria table,
per-inverse ledger, named gaps. Plan docs are not yours.

## 1. The browser bootstrap credential (§5 Layer 0)

A **new format**, reviewed as such — not invite reuse:

```
InviteToken (root, rendezvous, nonce, expires_at)
 + anchor Noise static pubkey (X25519, the key Layer 0 pins)
 + mesh PSK (NKpsk0 admission secret, the browser trust domain's own)
 + anchor bootstrap URL
```

Deliver: the type in the SDK (`sdk/src/enrollment.rs` neighbourhood, a
new module), a canonical byte encoding with a version byte and a
self-describing prefix (the repo's pattern: `NMO1`-style tag + tagged
fields, no serde-derived ambiguity), a base64url string form for
handing to JavaScript, `mint` (CLI: `net-mesh anchor credential …`,
in `cli/`) and `parse` with strict bounds, and the **two lifetimes
stated in the format**: the invite nonce single-use, the PSK standing;
`expires_at` bounds the nonce, and the credential carries the PSK's
trust-domain id so a credential for one domain cannot be presented to an
anchor of another. Witnesses: round-trip; every tampered field refused;
expired refused; wrong-domain refused; the PSK never appears in logs or
`Debug` output (redacting `Debug` impl, tested).

## 2. Bootstrap listener: HTTPS + WebSocket (Stage 4 bullet 3)

Feature `rtc-bootstrap` (implies `webrtc`): `axum` over the payments
crate's **pinned rustls** version — `cargo tree` must show one rustls.
Endpoints:

- `POST /rtc/offer` — body: credential string + SDP offer; the anchor
  validates the credential (slice 1), charges the §12 bootstrap budget
  (already in core; call it, do not re-implement), creates the engine
  dialog through the **same** production path 4a's `spawn_dialog_completion`
  consumes (an HTTP-originated Offer is an Offer; the dialog owner is the
  same code), returns the SDP answer.
- `GET /rtc/trickle?dialog=…` upgraded to WebSocket — trickle stays
  (S0b: 22 ms vs 150 ms); candidates both ways as the `0x0D02` message
  JSON, same codec; `Origin` header validated against the configured
  allow-list; unknown dialog → close with a typed code.
- `GET /rtc/anchor` — the anchor's announcement fields (`noise_pubkey`,
  `rtc_addr`, capabilities), so a browser can verify the credential's
  pinned key against the live anchor.

TLS: browser-trusted, two paths — **operator-supplied** cert+key
(PEM paths in config) and **ACME** (`instant-acme` or the payments
crate's choice if it has one — check first; HTTP-01 on the same
listener). CI must not pass `--ignore-certificate-errors`; the harness
uses a local CA it installs into Chromium's NSS/`certutil` store (or
`--host-resolver-rules` + a CA-issued cert for a fake hostname). Cross
origin: explicit CORS allow-list from config; no wildcard; preflight
handled; credentials never in a query string.

`rtc_bootstrap` on the announcement becomes the **real** listener URL
(4a synthesized `https://<addr>/rtc`; carried gap closed here).

## 3. mDNS host candidates (§6)

Chromium hides host IPs behind `<uuid>.local` candidates. Choose from the
plan's three and justify with a measurement: (a) accept the
**peer-reflexive** candidate str0m learns from the browser's inbound
binding request; (b) rely on the browser's **server-reflexive**
candidates gathered against the anchor's own STUN (Stage 3 responder);
(c) an mDNS client on the anchor. (a)+(b) are the expected answer and add
no dependency. The Chromium harness runs **without**
`--disable-features=WebRtcHideLocalIpsWithMdns`; the report states which
candidate pair formed (from `str0m`'s selected-pair event), on loopback
and on one real interface.

## 4. Chromium harness (exit criterion 1)

`net/crates/net/tests/rtc_browser/` — a Playwright (Node) runner driving
a real Chromium against a native anchor started by the test: credential
→ `POST /rtc/offer` → trickle over `wss` → DataChannel open → Noise
NKpsk0 **in the browser** (the S0a wire crate compiled to wasm; the SDK-TS
mirror is Stage 5 — here a minimal page that uses the wasm build's
handshake and framing) → the enrollment exchange → promotion observed on
the anchor (`peer_is_provisional == false`). This is the **six §12
witnesses with a real browser** replacing 4a's native stand-in: permitted
exchange completes; provisional denied announcement / unrelated channel /
other service / forwarding; local envelope accepted, redirected denied;
promotion binds the exact session; bounds; enrolled-without-authority
still denied. CI: a `webrtc-browser` job with Chromium from Playwright's
cache, `--no-tests=fail`, the floor and names pinned like the other RTC
jobs.

## 5. MITM witness, correctly shaped (exit criterion 2)

Substitute the **actual responder** with one that does not hold the
credential-pinned static private key (a second anchor with a fresh
keypair, serving the same URL via the harness's host rules) and assert
the Noise handshake fails in the browser **and** nothing is installed on
the impostor. Mutating a key in the HTTP response proves nothing and is
not the witness.

## 6. Anchor behind NAT publishes a working `rtc_addr` (exit criterion)

Via `public_addr` config or the port-mapping feature; the NAT simulator
(`natsim.yml`) gets one scenario: anchor behind simulated NAT, native
client outside, direct DataChannel forms using the published `rtc_addr`
as the STUN target. Green run first, then the assertion.

## 7. `net-mesh anchor` CLI + Deck

`net-mesh anchor serve` (config → listener + announcement tags),
`net-mesh anchor credential mint|inspect`, Deck lists anchors with
`rtc_addr` / `rtc_bootstrap`. CLI tests under `cli/tests/` per the
crate's convention.

## 8. Carried gaps to decide, not defer

- **F7 `proxy.rs`**: Kyra holds it as a documented non-reachability
  boundary. Add the witness that proves it: an RTC-originated frame
  cannot reach the proxy path (counter zero under attempt), and record
  it in the report as the boundary's evidence.
- The **§12.5 policy items** the agent proposed in `S4A_REPORT.md`
  §13.4 (install-time `max_provisional`, aggregate bootstrap-byte
  ledger, enrollment deadline with handler cancellation, subscribe
  bounds, incarnation-keyed signal queues) are **owner-pending**. Do
  not implement them in 4b unless the owner accepts; if accepted, each
  is its own commit with its own witness.

## Validation

Every 4b witness inverse applied-red-reverted with hash check; all RTC
binaries and the browser job `--no-tests=fail --retries 0`; `--lib`
default and `webrtc`; default-feature build unaffected (`rtc-bootstrap`
off: no axum/rustls in `cargo tree` for the default build — assert it in
a test like the wire-crate boundary test); export checker; consumer
diff file by file; `cargo tree -d` shows one rustls. Reply with the
candidate hash, the report, and the validation list. Then stop — no
Stage 5.
