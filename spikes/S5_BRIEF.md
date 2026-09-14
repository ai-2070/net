# Stage 5 — `net-leaf` + `@net-mesh/browser`

Plan: `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` —
Stage 5 (scope + exit criteria), §7 (leaf profile), §8 (identity,
leader election, leader lifecycle), §9 (browser ↔ browser via the mesh
as signalling network — Stage 6 owns the end to end, Stage 5 owns the
leaf side), the Follow-on section (serverless Tier A, which the
`ControlPlane` trait must not preclude), the slice table row 5, the
dependency list (`wasm-bindgen`, `web-sys`, `wasm-bindgen-futures` —
leaf only). Spikes S0a (wire crate + wasm), S0b (main-thread RTC, no
worker), S0c (fragment at `MAX_PAYLOAD_SIZE`, batch per packet), and
the 4b harness (`tests/rtc_browser/`, whose page + wasm leaf is the
proof of concept Stage 5 replaces with a real crate and package).

Authorization: the product owner authorizes Stage 5 implementation
stacked on the 4b second-round head (`c40357404`, CI + natsim green)
while Kyra's 4b verdict is outstanding. Additive: new crates
(`net/crates/net/leaf/`, `sdk-ts` gains `@net-mesh/browser` as a
sibling package or sub-path — pick the repo convention and say why),
no changes to Stage 3/4a/4b core code except new fixture-gated hooks
named in the report. One commit per slice, prefix
`feat(net): stage 5 —`. Report `docs/internal/spikes/S5_REPORT.md`:
exit table, inverse ledger, sizes, named gaps. Plan docs are not yours.

## 1. `net-leaf` crate (Rust, wasm32 target)

Over `net-mesh-wire` (S0a): the leaf profile of §7 — Noise session,
framing, `ParsedPacket`, streams (reliable + fire-and-forget, the
consumer-side `seq` reorder the Stage 3 witnesses established),
dispatcher, channel subscribe/publish, nRPC client (`call`, typed
codec, the enrollment exchange as the first caller), fold announce,
capability query. Fragment at `MAX_PAYLOAD_SIZE` (S0c: silent drop
above it → fragment + `validate()` counter); one `Batch` per packet.
`wasm-pack`/`wasm-bindgen` build; `cargo check --target wasm32-unknown-unknown`
in CI; the `cross_lang_wire` fixtures replayed **inside** the wasm test
runner (`wasm-bindgen-test` in headless Chromium) — that is an exit
criterion. No `Instant::now()` (S0a: panics on wasm32) — the coarse
clock seam from Stage 2.

## 2. `ControlPlane` trait — from day one

Everything the leaf needs from an anchor that is not a data-path Net
packet: bootstrap offer/answer, candidate trickle, announcement
publish/subscribe, signalling-dialog transport for peers it has no
session with yet. `AnchorControlPlane` (DataChannel to a native anchor
via the 4b listener + credential) is the only v1 implementation. The
trait must not leak `PeerAddr::Rtc` or any anchor type. **And the
design question Kyra pinned:** the native §5 path needs a routed A↔B
Noise session *before* signalling; serverless Tier A cannot provide
one. Stage 5 must **specify** the session-independent signalling path
(a signed, self-authenticating `0x0D02` envelope the control plane can
carry without a prior session — or an explicit finding that the
follow-on is a refactor). Write it as a design section in the report;
implement only what v1 needs, but the trait's shape must admit the
specified path.

## 3. Identity + leader election + leader lifecycle (§8)

`EntityKeypair` + Noise `StaticKeypair` generated in wasm
(`getrandom` with `wasm_js`), stored in IndexedDB encrypted under a
non-extractable WebCrypto AES-GCM key — with the honest statement of
what that protects (the origin is the trust boundary) in the package
docs; custodial injection (same API shape as
`MeshNodeConfig::entity_keypair`). One node per origin: tabs contend
for a Web Lock; the holder runs the node **on the main thread** (S0b:
`RTCPeerConnection` undefined in workers); followers attach over
`BroadcastChannel`/`MessagePort` and see the same API — the follower
proxy is a real SDK surface (streams, nRPC futures, events).

**Leader lifecycle, specified and tested** (Kyra's §8 list): the
interruption budget; disposition of pending nRPC calls and in-flight
stream sends on leader loss (fail typed, never silently retried);
restoration of streams and channel subscriptions by the new leader
(re-bootstrap with the same identity = a rebind through the existing
address-independent identity binding); **stale-leader fencing** — a
suspended tab that resumes cannot present the identity alongside its
successor (lock generation + a fenced token every message carries).
Tests: close the leader; suspend/resume a tab (Playwright CDP
`Page.setWebLifecycleState`); two tabs sharing one identity without
evicting each other (exit criterion).

## 4. `@net-mesh/browser` TypeScript wrapper

Idiomatic TS over the wasm-bindgen surface: `connect(credential)`,
`call`, `openStream`, `subscribe`, `announce`, `query`, typed errors.
**Failure typing, corrected:** an ICE timeout surfaces as
`RtcError::IceTimeout`/unreachable; `UdpBlocked` only when the
narrower cause is actually established (a STUN binding to the anchor's
`rtc_addr` fails while the HTTPS bootstrap succeeded is the one piece
of evidence that distinguishes it — implement that probe, and only that
turns the type). Bundle built with the repo's TS toolchain; sizes
recorded (exit: wasm ≤ 1.5 MB gzipped; report both raw and gzipped for
wasm and the JS bundle, and the S0a baseline for comparison).

## 5. Playwright runner and matrix — Stage 5 owns it

Extend `tests/rtc_browser/` into the Stage 5 runner (or a new
`sdk-ts/browser/e2e/`; keep one runner, migrate the 4b witnesses into
it so there is a single browser job): **Chromium + Firefox** against a
native anchor: handshake; reliable round-trip; fire-and-forget loss
under injected DataChannel loss (the 4b runner's drop hook); nRPC to a
native service; a native peer's `find_best_node` returning the browser
node; two tabs sharing one identity; a prompt **typed** failure under a
UDP-blocked profile (Playwright's network conditions cannot block UDP —
use the natsim gateway or a firewall rule in the CI job, and say which).
Safari best-effort, recorded. All 4b witnesses stay green in the merged
runner.

## 6. Anchorless mock `ControlPlane` — genuinely anchorless

In the wasm test runner: an in-memory `ControlPlane` with **no
forwarding of pre-direct Net packets** behind it; every input
(identity, keys, admission) explicitly provisioned by the test; drives
the leaf through handshake and one direct browser ↔ browser session
(two leaf instances in one page, or two pages bridged only by the
mock's signalling). A mock that quietly relays Net packets is a
re-implementation of the anchor and does not count — the report says
what the mock carries, message by message.

## Validation

`cargo check --target wasm32-unknown-unknown -p net-leaf`; the wasm
test runner (cross_lang replay + the anchorless session) in CI; the
browser matrix green on Chromium and Firefox, floors and names pinned
like the RTC jobs; every witness inverse applied-red-reverted with hash
check; sizes recorded; default build untouched (`net-leaf` is not a
workspace default member if it would drag wasm deps into native
builds — assert like `bootstrap_dep_boundary`); full AGENTS.md pre-push
checklist every push; export checker; consumer diff. Report only on
green exact-head CI. No Stage 6.
