# Net v0.37 — "Turbo Lover"

*Named after Judas Priest's 1986 single for a chrome-plated mesh that moves fast.*

---

## A browser tab is a node now

`@net-mesh/browser` is a new TypeScript package that runs a real Net node inside a page. It is not a client library: the page gets its own Ed25519 identity (kept in IndexedDB behind a non-extractable key), its own Noise session, and its own streams, channels, RPC calls, capability announcements and store replicas — the same protocol the native SDKs speak, compiled to WebAssembly from a new Rust crate, `net-mesh-leaf`.

To get there the wire layer itself had to become portable. `net-mesh-wire` is a new workspace crate holding the packet, session and reliability code with no tokio in it, and one cryptographic backend per target: the native one on servers, ChaCha20-Poly1305 in the browser. If you build on the Rust core nothing changes — the crate is internal, and the browser half of the transport is behind a `webrtc` feature that is **off by default**.

**How a page reaches the mesh.** A browser cannot accept a UDP packet, so it dials a native **anchor** over a WebRTC DataChannel. The anchor bootstraps signalling (that is how two browsers find each other) and answers STUN, and it relays only the pairs ICE cannot connect. The intended steady state is that traffic stops touching the anchor entirely: direct browser-to-browser, or browser-to-native, over a DataChannel. The way to state it is the way the plan states it — *anchors are how browsers find each other, not how they talk to each other* — and the anchor reports the ratio, so you can check: `net-mesh anchor stats` prints a direct-versus-attempted ledger, and CI asserts that the anchor's own forwarding counter goes flat while a direct pair keeps talking.

An anchor is an ordinary native node built with `--features webrtc` and the `rtc-bootstrap` feature that carries the anchor surface. It mints a signed bootstrap credential (`net-mesh anchor credential mint|inspect`) and serves it over HTTPS plus a WebSocket signalling listener (`net-mesh anchor serve`, `anchor ls`).

**One node per origin.** Two tabs on one site are two would-be nodes sharing one identity, which is a fight, not a feature. `openSession()` elects a leader per origin with a Web Lock; the leader runs the peer connection on the main thread and the other tabs drive it through the same API as followers. `role()`, `generation()` and the lifecycle events are how a page notices that the leader moved and its subscriptions were restored.

**Failures are typed, and the interesting one is honest.** Every rejection is a `LeafError` with a stable `.kind` — `wire`, `session`, `identity`, `not-leader`, `ice-timeout`, `udp-blocked`, `rtc-unsupported`, and so on. `udp-blocked` is the one worth reading about: an ICE timeout is reported as `ice-timeout` *unless* two observations hold together — the anchor answered its HTTPS bootstrap, and a STUN binding to the address that anchor published went unanswered. Only then is "your network blocks UDP" a claim the library is willing to make.

**The game store.** The same package ships an authoritative networked document — `defineStore()`, `hostStore()`, `joinStore()` — served to replicas over a mesh stream. One node hosts, everyone else holds a projection it was given. Actions are correlated transactions executed on the host; movement input is coalesced and unacknowledged, which is what makes 60 Hz feel like 60 Hz; snapshots are chunked, and a replica that loses a manifest or stalls mid-assembly recovers by asking again. `setAudience()` scopes what a given replica is allowed to see, and revoking an audience reaches the live update feed. `@net-mesh/browser/three` adds `bindEntities()`, which reconciles the store's entity map into any scene graph with `add`/`remove` — it imports nothing from `three`, so it drives a real `THREE.Scene` or a test double equally.

**What is proven, and where.** Real Chromium and real Firefox run the browser matrix against a real anchor in CI, including a NAT conformance matrix (cone, port-restricted, symmetric) behind actual NATs. The wasm node replays the cross-language wire fixtures inside the browser runtime, and the two-tab demo in `net/crates/net/examples/browser-demo/` sustains a direct 60 Hz link with a `--check` mode that makes five assertions on the anchor itself.

**Not in this release, deliberately.** No TURN server and no rescue for a network that blocks UDP outright — that surfaces as the typed `udp-blocked` failure rather than a hang. ICE-TCP candidates, native-to-native WebRTC, mDNS resolution on the anchor, and browser-side RedEX/Dataforts are all deferred. WebKit is recorded in CI but never gates; the store witnesses gate on Chromium and run `continue-on-error` on Firefox. Anchors are not packaged or published yet — you build them, and you build `@net-mesh/browser` with `net-mesh-leaf` from the repo, together (they share a wasm-bindgen boundary and must never be version-skewed).

---

## Streaming behind an organization boundary

Until now, a caller holding organization credentials could invoke a protected capability one way: a single unary call. Anything streamed had to go out unauthenticated or not at all — a streaming caller with an org intent was refused locally, before it left the process.

Now all four nRPC shapes work behind org admission — unary, server-streaming, client-streaming and duplex — in both roles, and in every SDK: Rust, Node/TypeScript, Python (sync *and* a new `AsyncOrgClient`), Go, C, and the browser leaf. The verbs are the ones you would guess (`call_streaming`, `call_client_stream`, `call_duplex`, and `serve_org_streaming` and friends), and `@net-mesh/sdk` forwards the same shapes so a TypeScript consumer compiles against them.

The interesting design decision is where admission happens: **before any handler effect, and bound to the exact session.** The signed proof is tied to the 32-byte Noise handshake hash of the connection it arrived on *and* to the call shape, so a proof captured from one session cannot be replayed into another, and a proof for a unary call cannot be replayed as a duplex opening. A stream gets a bounded lifetime — 300 seconds by default, with a provider cap of an hour — and per-call, per-caller and per-node byte ceilings. A request above the cap is refused, never silently clamped. Streams are retired on deadline, cancellation, a credential clamp, or a revocation-floor raise, and reopening after expiry is a new call rather than a resumption.

This landed on the `org-nrpc-merge` branch, and the structural part matters more than the feature: org admission was folded **into** the existing nRPC admission path rather than built beside it. Public and protected calls now share one bridge and one fold, with admission at the same seam, so there is exactly one place where a call is admitted and one place where a call's lifetime is owned. An independent review of the change came back as a hold with 79 findings; the repair pass closed all of them except one recorded trust-model residual (below), and added a CI guard crate that fails the build if the public org/streaming signatures change without the change being announced.

**Three known limitations ship with this, and all three fail silently.** They are owner-bound decisions, filed rather than fixed, and they are the kind of thing you want to know before you rely on the feature:

- **`MeshNode::start()` can return success while starting nothing.** The call is idempotent, but it must not be made while an `accept()` is in flight. If it is, the start is refused, the started flag is rolled back, and the receive loop is never spawned — the only trace is a warning in the log, because `start()` returns `()`. A node in that state answers nothing on the mesh while its own sends work perfectly. The refusal is retryable: once the `accept()` returns, a fresh `start()` proceeds normally.
- **The Python binding inherits that silence.** `NetMesh.start()` calls into the same path and returns `Ok(())` unconditionally, so a Python caller has no channel on which to observe the refusal at all.
- **Cross-org revocation floors are "absent means zero".** For a cross-org caller, the revocation floor is read from the provider's own revocation state — and floors exist there only if the provider's operators have imported the caller organization's signed revocation bundles. If they have not, which is the default, the floor is zero and the revocation check permits **every** cross-org caller for the life of its membership certificate. Import the bundles, or treat a cross-org grant as valid until its certificate expires.

One more honest note on the same-origin leaf: the follower id in a leader-proxy envelope is sender-claimed. Per-request, per-call and per-bridge ids are drawn from the CSPRNG, but a follower's claim about *which* follower it is remains a claim. It is recorded as a residual rather than fixed, because closing it needs a browser-side attestation the leader cannot forge.

---

## Selling an agent-to-agent task

An agent-to-agent service can now be *paid*. A provider states, per service, whether it is free or paid; a caller pays for one exact piece of work; and both sides keep durable records so that a crash between the money and the work reconciles instead of charging twice or running twice.

The ordering is the design. Everything that can be refused is refused **before a quote exists**: the brief is validated, the application's own preflight runs, capacity is reserved, and the provider mints an `admission_id` — and only then does money move. The sequence is `describe → prepare → purchase → submit → launch`, and `prepare` is deliberately uncharged, because it is what lets a caller *display a price* without spending anything.

The purchase binds to that one reservation. The existing quote already carries an `input_hash` that participates in the quote id, and the provider now expects that hash to commit to the purchase — the offer, the service, the revision, the task id, the prompt, the context references and the tags. The consequence is the one that matters: the same proof replayed under a different owner, or against a different reservation, computes a different expected hash at the provider and is refused before anything runs. Redemption is idempotent *per purchase hash*, which is exactly what lets a provider that crashed between its own payment write and its journal write reconcile on retry instead of charging twice.

Two new services are added beside the existing verbs and neither one touches the payment gate: `net.a2a.describe` publishes what a provider is offering, and `net.a2a.prepare` is the uncharged step above. The paid path is refused with the same application error and failure header a paid tool already used, with reasons a caller can branch on: `missing_quote`, `binding_required`, `binding_rejected`, `input_binding_mismatch`, `no_reservation`, `admission_revoked`, `retired`, and `journal_unavailable` (the one that is worth retrying). Everything non-financial — an unknown service, a stale revision, bounds exceeded, `Busy` — stays an ordinary in-body rejection.

**What a provider refuses to start with.** A paid service with no pricing document, a free service that carries pricing terms, or a paid entry without a payment gate and an admission journal all refuse at serve time. That is the point: a service configured as paid can never come up in a state where it would serve for free. A catalog of only free services needs no gate and no journal, and links no payment code at all — but free means *no payment*, not *no policy*: free services still run the application's preflight and still enforce their in-flight limit.

**Four things an operator has to know.**

- **The unresolved-financial queue is never pruned. By design.** Three classes of record — paid but never launched, launched with no recorded outcome, and an admission revoked after payment — accumulate until a human resolves them, and neither result retention nor the ordinary pruning calls touch them. `unresolved()` (Rust) and `a2a_unresolved()` (Python) are the queue; `resolve(...)` and `a2a_resolve(...)` are the only exits. A store that quietly discarded "money moved, or may have" would be worse than one that grows. Wire both into whatever you already page on.
- **The journal's lock is a local-filesystem contract only.** It takes an advisory lock on a `<path>.owner` sidecar and holds it for the journal's lifetime, and a second writer — in this process or another — is refused at serve time rather than interleaving. Advisory locking over NFS or SMB is not dependable, so a journal on a network share is not protected by that guarantee. Back the sidecar up with the journal, and delete it with the journal.
- **`reservation_retention_secs` has no default.** It is a required field of every offer, committed into the signed offer, and it should be orders of magnitude beyond any quote's lifetime. It is the window in which a caller who paid but never submitted still holds its admission; after it, a paid submit is answered `no_reservation` and reconciliation is manual. Note also that retention is a policy applied *when you prune*, not an automatic sweep — nothing runs it for you.
- **Retiring a revision has no supported in-catalog procedure.** Changing a service's `revision` after a prepare strands paid quotes as stale, and the obvious mitigation — keep the old revision in the catalog — does not work, because catalog lookup resolves exactly one revision per service id. Publish the new revision under a new service id and retire the old one after its reservations drain, or accept the refund obligation. There is no automatic refund.

**Where it exists.** Rust and Python both ship the whole surface: the Python binding gained `serve_a2a_configured`, `a2a_unresolved`, `a2a_resolve`, the caller's `prepare_task` / `purchase_task` / `submit_task`, and the attempt store behind them. Node has the free path only — `submitTask` gained an optional caller-chosen `taskId` and optional `service`/`revision` addressing — and paid A2A is deferred in both directions there. Go has no A2A surface at all. Settlement still runs against the mock facilitator; real rails, escrow, refunds, metering and resumable execution across a restart are all deferred. The tests that prove the crash behaviour prove it against injected faults rather than a machine that actually lost power, and two paths are unproven end to end: the protected paid lifecycle from an installed wheel, and a Python caller driving it through the org authority in a second process.

---

## Joining without a shared secret

The CLI used to be a collection of one-shot commands that each built a node, did a thing, and died. It now owns a node. `net-mesh up` starts one long-lived node for a profile in the foreground, `net-mesh node status` asks that node what it is, and `net-mesh down` stops that exact instance — one owner per profile, enforced by a lifetime lock and an authenticated loopback control endpoint. The PSK is generated on first start or read from a protected source (`--psk-from file:<path>` or `stdin`); a literal PSK is never accepted on the command line and never printed.

Then the part that removes the shared secret entirely. `up --enroll` makes that same process the enrollment owner, and `invite create` mints a `netmesh-join_` link. A clean device runs `join <token>` and then `up`, and it is on the mesh: direct attachment is tried first, the relay is the automatic fallback. Links are bearer secrets unless you bind them with `--for`, and `--require-approval` holds issuance until `invite approve`.

**One link, several relations.** A single join link can carry a subnet attachment, an organization membership, or a channel credential — and a device already on the mesh can add one relation standalone with `org invite|join`, `subnet invite|join` or `channel invite|join`. Each relation keeps its own authority: org membership is root-signed at `org approve`, a subnet attachment is a delegated leaf presented over a session, a channel credential is a root-anchored token chain. Nothing in a join confers more than it says — no dispatcher rights, no channel admin, no wildcard, no subnet route or export.

**Removing and leaving are different verbs, on purpose.** `org remove` and `subnet remove` apply a root-signed floor at the nodes you name, and each named node reports back from its own signed attestation, so `complete` means all of them persisted it — not that a broadcast went out. `org members` and `subnet members` show you the difference between what was *issued* and what was *observed here*; there is no global roster to lie to you. `leave` is local and durable, survives a restart, and **revokes nothing**. A verifier has one active subnet attachment at a time, and switching is explicit (`--switch`, `subnet activate`).

Channels get their own operator loop: `channel serve`, `status`, `publish`, `leave`, plus `invite`/`join` for adding a channel to a device that is already joined. Subscription and publish readiness are reported from the live session and the node's own gate — never implied by the fact that you hold a credential.

An enrolled device can run its consumer halves as that device: `wrap --joined <state-dir>` and `mcp serve --joined <state-dir>`.

**Deferred, and worth knowing before you plan around it.** There is no `--detach`: a node runs in the foreground, so supervision is systemd's job, or launchd's, or a Windows service. PSK sources cover `file:` and `stdin` only — no KMS. There is deliberately no `channel members` roster. And removal reaches only the nodes you name; there is no enforcement-point inventory, so "removed" is a claim about those nodes and not about the mesh.

---

## Getting through NAT

Two nodes that cannot see each other now have somewhere to meet, without either of them sharing a secret with the meeting point. `net-mesh relay serve` runs a **blind** relay: it forwards ciphertext between registrations and holds no PSK, no issuer key, no mesh credential, and no way to read what it carries. It answers on UDP and splices TCP on the same port.

A joiner tries the direct address first and falls back to the relay automatically — you can see which path it took, because the report names it (`attach_path: direct | relay | relay_tcp`, and `relay_transport: udp | tcp`). A relayed session is then upgraded to direct in the background, once a hint arrives, so the relay carries the beginning of a relationship rather than all of it. And if UDP to the relay is blocked outright, the node falls back to a plain TCP tunnel on port 443 carrying the same end-to-end ciphertext.

None of it changes the direct path, and none of it is on by default: the relay is opt-in, `relay_fallbacks` counts routed resolutions rather than failures, and a deployment that never runs a relay behaves exactly as before.

**Deferred.** The background upgrade to direct does not yet cover the "single punch" NAT classes (cone against cone, cone against symmetric) — those are deferred and re-tried every 30 seconds, and closing that needs end-to-end punch ids. Port-mapping install and renewal do not invalidate the announcement window, so peers learn late. Symmetric-against-symmetric port prediction is a deliberate non-goal. The TCP tunnel is plain TCP: it is not claimed to cross a proxy that inspects TLS, and a joiner's recovery of a dropped tunnel is not yet witnessed. `DEFAULT_RELAY` stays empty until somebody deploys a relay.

---

## The CLI grows up

The operator surface got the two things it was missing: **one timeout, honoured everywhere**, and **the ability to see what a command would do without doing it**.

`--timeout` is now one absolute budget per command, covering configuration, identity loading, attachment, and the network work — not a per-phase knob and not a decoration. The old advertised-but-unused 30-second global default is gone, and a command that cannot honour a timeout now *refuses* one instead of silently ignoring it. `wrap`, `mcp serve`, `transfer recv-blob`, live typegen and the remote aggregator verbs all take a startup or acquisition budget, and they tell you what happened to the child process or the partial file when the budget runs out.

`--inspect-target` runs the same resolution the command would run, and stops. It reports the paths, the store, the signer's fingerprint, the resolved remote target and the bind address that execution would use, without starting a supervisor, reading a secret, opening a socket, spawning a child or writing output. It is on nearly every verb now, including the ones that used to be "just print the paths".

**The breaking part is the temporary supervisor.** A set of read commands — `peer ls`, the audit/log/failure streams, capability reads, subnet and gateway and channel reads, admin/ICE, and the local aggregator listing — used to start a throwaway node, read from it, and exit zero. The snapshot was empty by construction, and an empty snapshot and a healthy idle cluster are the same document, which is how a monitoring script reported a healthy cluster having inspected nothing. 0.35 made `snapshot get` and `snapshot status` require `--local` for exactly this reason; those commands now join them, and all of them exit 2 without it. If you meant to observe a running deployment, use a surface that attaches to one (`net-mesh aggregator`, `net-mesh peer`, `net-deck`).

The rest of the CLI breakage is small but real, and all of it is listed under *Breaking changes* below: `aggregator ls` picks remote RPC from a complete profile target, ICE commits emit one JSON value instead of two, a malformed `--bind` on an attach verb is exit 2 rather than exit 6, and an explicitly named config file or profile that does not exist now fails instead of falling back. `netdb restore` also stops losing the state it restored — it persists the restored adapter for later opens, and refuses to reopen a restored store under a different origin.

---

## Capability sensing, and an organization that picks the ready node

An organization's calls used to land on providers in a deterministic order that knew nothing about whether anyone was ready. That changed, for the exact-provider case.

The Rust SDK now has a supported sensing surface on both sides. A provider publishes readiness: `sensing().provide(capability, evaluator)` registers an evaluator that answers "am I ready for this" and returns a registration that releases itself when it is dropped. A consumer watches: `sensing().watch(SensingQuery::new(capability))` gives you a snapshot of every provider you are authorized to use, each classified ready, not-ready, or unknown — with its estimated start and the caller's own route estimate — and `changed()` is a park that cannot miss a wake-up. "Exact sensing" is the honest name for it: the watch observes the specific set of providers the node is authorized to use, derived from installed organization authority and owner-private discovery, one lease per provider. It is not a rendezvous service, and it does not pretend to be one.

The consumer of that information is `OrgClient::call`. An organization call now senses which authorized providers are ready, ranks them by this caller's own route economics, and permutes an already-authorized candidate list so that ready providers come first. The word to hold on to is *permutes*: sensing never adds, removes or authorizes a candidate, only reorders ones that were already allowed. If nothing is ready, the call falls back to the original deterministic order and reports no new error.

This is off by default and ships dark behind `enable_sensing`; a node that never enables it mints an inert binding that does no sensing work and behaves byte-identically. The wire addition is appended and decodes on older builds. There are **no breaking changes** here.

**Deferred.** Sensing is Rust-only — there are no watch or sensed-call bindings in Go, TypeScript or Python. Provider-free and leader sensing (a node asking the mesh in general, rather than its own authorized set) is design-only. Cross-organization and granted-scope sensing is refused, not implemented. The warmed-pool and power-of-two-choices half of org load balancing is not built: the shipped sensed call does one bounded per-request projection rather than keeping a cached route set. The v0.36 note said organization load balancing "did not move"; the exact-provider half moved, and the warmed-pool half still has not.

---

## Bigger answers over RPC

A native unary RPC used to fail if its response did not fit in a single 8 KiB packet, which put a ceiling on anything that returned metadata, a schema, or a document. A caller can now negotiate responses up to 1 MiB — 128 times the packet limit, without raising the packet limit itself.

The caller opts in per call. The server splits an oversized response into 4 KiB fragments that the pending call reassembles, with the packet cap untouched and the transfer bounded on both sides (1 MiB per response, at most 256 fragments, 8 MiB incomplete per caller, eight node-wide transfer slots, a 30-second ceiling). A caller that does not negotiate gets an explicit small error naming the limit instead of a silent wait — which is the compatibility behaviour, and the reason the error exists: the previous failure mode was indistinguishable from a slow network, and the handler may already have completed.

Live typegen benefits first: schema capture grew from a packet to roughly 22 KB of metadata. Request-body fragmentation and large pub/sub messages are deferred.

Alongside it, reliable streams gained in-order delivery on the receive side: the receive path now holds and reorders frames into sequence and drops duplicates, where before an out-of-order frame could be delivered as if it were next. This is what the browser transport needed, and it is documented in `TRANSPORT.md` as the guarantee you get: FIFO within a stream, with no ordering promised across streams.

---

## Everything else in the box

- **A third skill, and examples that run.** `.claude/skills/` gained `net-browser` (the tab-as-node and the networked store), and the checked-example corpus grew from a two-route install check to eleven routes across five bindings, executed in CI and matched against a manifest. The plan's proposal to relocate example sources to a neutral directory was explicitly rejected in favour of a neutral *index*; the files stay where their build systems expect them. The README got a positioning rewrite, and `check-readmes.py` exists but is not yet wired into a workflow — a known gap rather than a claim.
- **A guard against silent API breaks.** `net/crates/net/guards/org_api_probe` pins the public org and streaming signatures and fails the build when one changes without the change being announced. It is the mechanism that makes the source breaks listed below safe to publish as a list.
- **One version for the workspace.** Every crate now inherits a single workspace version, checked in CI. The 0.36 cycle shipped a broken Python lower/upper bound by rewriting one line in the wrong place; this is the structural answer to that class of mistake.
- **CI and test infrastructure.** A new `natsim-enroll.yml` workflow runs the NAT matrix for the enrollment path. Test execution itself got cheaper — `debug = "line-tables-only"`, JUnit-based verification instead of re-running named witnesses, one feature graph per integration family — which is a developer-facing change with no user-facing surface.
- **The toolchain moves to Rust 1.98.1**, up from 1.97.1.

---

## Breaking changes

Grouped by who feels them. Everything not listed is additive.

**Operators and scripts**

- **Temporary-supervisor commands require `--local`** and exit 2 without it: `peer ls`, `audit`/`log`/`failures`, capability reads, subnet/gateway/channel reads, `admin`/`ice`, and the local `aggregator` listing. `snapshot get` and `snapshot status` already required it, since 0.35.
- **`aggregator ls` now selects remote RPC from a complete profile target.** A script that wanted the development-only in-process snapshot must add `--local`.
- **ICE commits emit one JSON value, `{preview, commit}`**, where they used to emit two consecutive values. `jq -s '.[1].commit_id'` becomes `jq '.commit.commit_id'`. Commit previews go to stderr, and `--yes` now skips the prompt on a TTY as well as on piped input.
- **A malformed `--bind` (or profile `bind`) on an attach verb is now exit 2, not exit 6.** Multicast and broadcast binds are refused on both paths.
- **An explicitly named config file or profile that does not exist now fails**, where it used to be ignored or fall back.
- **The global 30-second `--timeout` default is gone**, and a command or mode that cannot honour a timeout now rejects the flag (exit 2) instead of ignoring it.
- **`netdb restore` persists the restored adapter state**, so a later open sees the restored store instead of an empty one — and reopening a restored store under a different origin now refuses instead of reusing a counter. Older binaries must not open a restored store. Restoration is still not crash-atomic.
- **`net-mesh up` never accepts a literal PSK on the command line**; use `--psk-from file:<path>` or `stdin`. `up --enroll` refuses to run on a state that has already joined, and refuses `--psk-from` and `--identity`.

**Wire**

- **`TaskState::interrupted` does not decode on an older requester — in any language.** It is a new terminal state naming which ambiguity a restart left behind, and it is produced only by the configured A2A path. Because the decode happens in the embedded Rust before any JSON is produced, a previously built Python wheel or Node addon fails exactly the way a previously built Rust requester does. Only a binding built from this release or later passes the tag through. A deployment that serves only the legacy free path never emits one.
- Everything else on the wire is additive: the new uncharged A2A services, the optional A2A brief fields, the two payment request headers, the appended sensing variant, and the four optional announcement fields the browser transport adds.

**Rust API consumers**

- `RpcStreamingContext` is now `#[non_exhaustive]` and carries `org_admission`; `AdmissionContext.is_unary` became `shape`; `AdmissionDenied` gained variants (`ShapeMismatch`, `SessionBindingMismatch`, `ActiveCallOwned`, `ActiveStreamCapacity`, `DeadlineExceedsPolicy`, `Revoked`). These are the org-streaming source breaks, and the guard crate exists so the next one is announced rather than discovered.
- `ProviderChannel::quote` takes a trailing `input_hash: Option<&str>`; `RedeemDecision::Admitted` carries a `payer`; `CallerDecision::Failed` carries `quote_id`.
- A handler's application status code outside `0x8000..=0xFFFF` is clamped to `Internal`, and the `EarlyReturnDX` code moved from `0x007E` to `0x807E`.
- Behavioural changes on *public* (non-org) streaming, since the two paths now share one fold: a server-streaming fold enforces `deadline_ns`; a client-streaming or duplex deadline terminates as `Timeout` where it used to be `Cancelled`; an opted-in duplex response window is now honoured, which means it can stall instead of running unbounded; in-flight keys include the session id; and dropping a serve handle is now distinct from shutting the node down.

**Go and C**

- The organization ABI stamp moved to `0x0002` with exact equality, so Go wrappers and hand-written C consumers must be rebuilt against the current headers. The C surface gained the streaming call and serve entry points, and the Go binding gained the matching wrappers.

**Node / TypeScript**

- `submitTask` accepts an optional `taskId`, plus optional `service`/`revision` addressing a catalog entry — free-path only. A paid catalog entry refuses the uncharged submit verb before the executor runs. Paid A2A has no Node twin in either direction.
- The org/streaming verbs are additive: `callStreamingBytes`, `callClientStreamBytes`, `callDuplexBytes`, the `serveOrg*` family, and typed wrappers in `@net-mesh/sdk`.
- `@net-mesh/browser` is a new package with its own break list inside it, and one rule worth repeating: it must be upgraded together with `net-mesh-leaf`, because the two share a wasm-bindgen boundary that fails at the call site rather than at install time.

**Python**

- `CapabilityGateway` gains an `a2a_purchase_path` constructor keyword and the `prepare_task` / `purchase_task` / `submit_task` / `a2a_attempts` / `a2a_resolve_attempt` verbs; `PaymentProvider` gains `serve_a2a_configured` / `a2a_unresolved` / `a2a_resolve`. New exceptions: `PaymentRefused`, `JournalOwnedElsewhere`.
- Organization streaming arrives as `OrgClient.call_streaming` / `call_client_stream` / `call_duplex` plus a new `AsyncOrgClient`, with `serve_org_*` counterparts and `.pyi` parity.

---

## How to upgrade

1. **Doing nothing is a supported choice for most deployments.** The browser transport is off by default, sensing is off by default, the relay is opt-in, and the legacy free A2A path is byte-identical.
2. **Fix your CLI scripts first.** Add `--local` to every temporary-supervisor read command, add it to `aggregator ls` if you wanted the development snapshot, and rewrite any `jq -s '.[1]'` on an ICE commit. Then decide what a real `--timeout` should be per command — the flag now means what it says, and a command that cannot honour it will refuse it.
3. **If you sell A2A tasks:** put the journal on a local filesystem, treat `<path>.owner` as part of the store, and wire `unresolved()` / `a2a_unresolved()` into your paging. Choose `reservation_retention_secs` deliberately — it has no default. Upgrade every requester that polls a configured provider's status, in every language, before that provider serves paid work, or it will read `interrupted` as a decode error.
4. **If you use organization streaming:** rebuild Go and C consumers against the current headers, and take the Rust source changes above. Then decide how you feel about cross-org floors: if you do not import a caller organization's revocation bundles, you are permitting every cross-org caller until its certificate expires.
5. **If you run browsers:** build `net-mesh-leaf` and `@net-mesh/browser` from the repo, together, and stand up an anchor with `--features webrtc`. Treat `udp-blocked` as a real outcome rather than a bug — a network that blocks UDP is unsupported in this release, and the typed failure is the whole of the answer.
6. **If you run a relay:** it is a blind forwarder with no TLS on its TCP path and no secret of yours in it. Deploy it as you would any untrusted-but-critical hop, and leave `DEFAULT_RELAY` empty until you have one.
7. **Rust callers:** add the `input_hash` parameter, the new A2A fields, and the org-streaming source changes; the guard crate will tell you if you missed one.

---

## Dependency updates

The cycle is **1,482 non-merge commits over 1,105 files (+489,438 / −12,861)** from `v0.36.0` to the 0.37.0 bump — the largest cycle in the series, roughly twice the diff of the 0.34 release that held the record before it, and the first to add a workspace member for the portable wire layer.

- **New:** `net-mesh-wire` (the tokio-free, wasm-clean wire crate, now a workspace member and the core's dependency), the `net-mesh-leaf` browser node and the `@net-mesh/browser` package (both outside the root workspace, built together), and the two CI guard crates under `net/crates/net/guards/`.
- **Toolchain:** Rust `1.97.1 → 1.98.1`, same component set.
- **Rust dependencies:** the browser transport is most of the churn. It brought in the WebRTC stack (`str0m` 0.24.0 on the rust-crypto backend, `sctp-proto`, `dimpl`), a WebSocket client for signalling (`tokio-tungstenite`/`tungstenite`), an HTTP server for the anchor's bootstrap endpoint (`axum`, and `tower-http` 0.6.11 → 0.7.1), and certificate plumbing for it (`instant-acme`, `rcgen`, `rustls-pemfile`, the `x509-*`/`der-parser` family) over `aws-lc-rs` as rustls's crypto backend. The wasm side added `wasm-bindgen-test` and `minicov`, so the leaf's test runner and its coverage work in the browser runtime. Elsewhere: `portable-pty`, `serial2`, `shell-words`, `dunce`, `fs_extra` and `shared_library` for the CLI's new surfaces.
- **Notable bumps:** `rustls` 0.23.43 → 0.23.45, `hyper` 1.11.0 → 1.11.1, `reqwest` 0.13.4 → 0.13.5, `napi` 3.12.1 → 3.13.0, `wasm-bindgen` 0.2.127 → 0.2.129 (pinned exactly — it is the leaf/package boundary), `syn` 3.0.3 → 3.0.6, `uuid` 1.24.0 → 1.26.1, `redis` 1.5.0 → 1.7.1, `fastcdc` 4.0.1 → 5.0.0, `num-bigint` 0.4.8 → 0.5.1, `blake2` 0.10.6 → 0.11.0, `miniz_oxide` 0.8.9 → 0.9.1, `toml` 1.1.4 → 1.1.6, `dirs` 6.0.0 → 7.0.0, `pest` 2.9.0 → 2.9.2. Two crates left the graph entirely (`arrayref`, `tinyvec_macros`).
- **Web and npm:** Next 16.3.0 → 16.3.6, React 19.2.8 → 19.3.0, Sentry 10.70.0 → 11.0.0, Prisma 7.9.1 → 7.10.0, tRPC 11.18.0 → 11.19.0, better-auth 1.6.27 → 1.7.6, motion 13.1.0 → 13.4.4, plus the usual axios, TanStack and PostHog drift.
- **Build:** one workspace version (`[workspace.package]`), with a new `check-versions.py` CI guard that every published version agrees.
- **CI:** a new `natsim-enroll.yml` workflow, plus `natsim.yml`, `skills.yml`, `ci.yml`, `release-crates.yml` and `panic-probe-witness.yml` updated for the new crates, packages and matrices.
- **Docs and web:** the release notes, the CLI reference, `TRANSPORT.md`, `SENSING.md` and `ORGANIZATIONS.md` all moved with their tracks; the docs site gained a protected-streaming guide. The Claude-skills page still says "the two skills" and does not mention `net-browser` — a stale line, not a stale feature.

---

Released 2026-09-26.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) **OR** [Apache-2.0](../../LICENSE-APACHE), at your option.
