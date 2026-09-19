# Stage 7 — the packaged anchor, and surface completion

Plan: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` —
Stage 7 (previously DEFERRED; the product owner authorizes it now,
2026-09-16), §Non-goals ("serverless-only hosting is anchorless, not
UDP-blocked → Stage 7 packaged anchor"), §6 "Serverless", §7 registry
(`rtc_addr`, `rtc_stun_addr`, `rtc_bootstrap`), the Stage 4b report
(listener, credential v2, ACME, `anchor serve`/`credential mint`), the
Stage 6 report §6.12.2 (the announced STUN endpoint), and
`docs/releases/RELEASE_STEPS.md` plus the existing
`release-binaries-*.yml` / `release-npm-*.yml` / `release-pypi-*.yml`
workflows, which are the packaging conventions to follow, not replace.

Authorization: Stage 7 implementation on `LZL0/webrtc-transport`,
stacked on the Stage 6 STUN-endpoint head (`537c9c94f`) and the Stage 5
round-5 head, both under Kyra's review. Additive: a new binary/crate
and its workflows, binding surface, shared types; **no changes to the
transport, admission, leaf protocol or Stage 3–6 seams** beyond named
fixture hooks. Serverless (the §Follow-on) is **not** Stage 7 — it is a
separate plan and a separate authorization; do not start it. One commit
per slice, prefix `feat(net): stage 7 —`; report
`docs/internal/spikes/S7_REPORT.md` with the exit table, the
deployment walkthrough as actually executed, inverse ledger with raw
receipts, named gaps. Plan docs are not yours.

## 1. The packaged anchor — the item with product weight

**Goal (plan §Stage 7):** "a single-binary / container distribution of
a `webrtc`-enabled node preconfigured as an anchor (`serve_bootstrap`,
`serve_stun`, a pinned `rtc_addr`, invite minting), so that 'deploy a
browser-native Net app' means static assets plus one small always-on
process."

Deliver `net-mesh-anchor` (crate under `net/crates/net/anchor/` or a
feature-gated profile of `net-cli` — pick the one the release
workflows can carry with least new machinery and say why):

- **One binary, one config file, one command.** `net-mesh-anchor
  serve --config anchor.toml`: binds the RTC socket (`rtc_addr`), the
  STUN-only socket (`rtc_stun_addr`, Stage 6 §6.12.2 — distinct and
  announced), the bootstrap HTTPS/WSS listener (Stage 4b, with
  operator-PEM **or** ACME with the plaintext HTTP-01 ingress bound
  first), announces all three with the signed announcement fields, and
  serves the anchor directory. Public address from config
  (`public_addr`) or the port-mapping feature; refuse to start with a
  typed error if the two announced endpoints collide (the Stage 6
  spawn-time check) or if `rtc_addr` is unroutable and no
  `public_addr` is given.
- **Credential minting built in**: `net-mesh-anchor credential mint`
  (the Stage 4b v2 format, issuer-signed) and `inspect`, with the
  issuer identity persisted under the anchor's data dir so
  **credentials survive a restart** — this closes the Stage 4b named
  gap ("credentials pinned before an anchor restart are not proven
  reusable"): the anchor's Noise static key and issuer key are
  persisted and reloaded, witnessed across a real process restart.
- **Enrollment provider**: `anchor serve` today registers none (Stage
  4b boundary, kept honest). The packaged anchor ships a **local
  enrollment authority** (the plan's "enrollment authority local to
  each bootstrap-serving anchor") with a documented admission policy —
  approve-all-with-valid-credential by default, an operator-approval
  hook as the SDK already has (`serve_enrollment`'s B2c hook) — so a
  browser that completes bootstrap becomes an admitted node without
  the embedding application registering anything. Say exactly what
  authority this grants (device enrollment, not organization
  membership — plan §5 / review-log row 2).
- **Container**: a `Dockerfile` (distroless or alpine, non-root, the
  three UDP/TCP ports declared) and a `docker-compose.yml` example
  with static assets served beside it; a `release-binaries-anchor.yml`
  mirroring `release-binaries-cli.yml`, and a container publish
  workflow (GHCR) mirroring the repo's conventions. No secrets in
  images; the data dir is a volume.
- **Deployment walkthrough, executed**: `docs/` gets an "anchor
  deployment" page (mirrored to `web/` per the release-docs
  convention) that the report proves by *running* it: start the
  container from the built image, mint a credential, load the Stage 6
  demo page against it from a second host/netns, connect from
  Chromium and Firefox, see the counter flat once direct. That
  walkthrough is the exit witness, in CI (the natsim netns is the
  "second host").

## 2. Anchor-role parity in the bindings

`RtcConfig` + `RtcStats` (including `rtc_stun_addr`, `ice_direct /
ice_attempted` and the Stage 6 telemetry) through Node, Python and Go,
so a non-Rust process can *be* an anchor. Follow the existing FFI
rules exactly (AGENTS.md: single cdylib, header mirror `go/net.h`,
`abi_stability_*` tests, the export baseline regenerated **in the same
commit with the reason**); cross-language golden vectors for the config
and stats shapes; a parity test per binding that serves bootstrap and
answers a STUN binding request from a native client.

## 3. Shared generated types for `sdk-ts` / `@net-mesh/browser`

The two TS packages keep separate hand-maintained types; Stage 5's R11
ABI mismatch was the symptom. Generate the shared wire/config/error
types from one source (the Rust types via `wasm-bindgen`/`tsify` or a
checked-in schema the golden vectors already pin) and make both
packages consume them; a CI step fails on drift. No behaviour change;
the R11 real-package tests are the witnesses that nothing moved.

## 4. Deferred by the plan, and staying deferred

- **ICE-TCP passive candidates** on anchors — listener/framing/
  lifecycle are the caller's; not in this stage. State it.
- **DTLS-exporter shortcut** — stays deferred by S0c; not a
  performance lever.
- **Browser-side RedEX on IndexedDB** — separate plan.
- **Serverless control plane** — separate plan, needs its own
  authorization; the `ControlPlane` trait and D1 envelope from Stage 5
  are its hook and are not touched here.

## Exit criteria

- A browser-native Net app deploys as static assets plus the packaged
  anchor: the executed walkthrough above, green in CI, both engines.
- Credentials minted before an anchor restart still enroll after it.
- A bootstrap client that completes the exchange is an admitted node
  with no application-registered provider.
- Node, Python and Go can each run an anchor that a native client
  bootstraps against.
- `sdk-ts` and `@net-mesh/browser` share generated types; drift fails
  CI.

## Validation

Every witness inverse applied-red-reverted with raw receipts; the
walkthrough as a CI job with its own roster and floor; all prior
suites unchanged and green; bindings tests with cgo actually enabled;
`--lib` floors; the export checker with the deliberate baseline change
explained; consumer diff file by file; full AGENTS.md pre-push
checklist. Report only on all-green exact-head CI. No serverless work.
