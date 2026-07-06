# Hermes V2: Operator Mesh + Tool Federation

**Relationship to the frozen native plan:** that doc is the reference doctrine (H-rules, migration ladder, publication tiers, failure semantics — all still binding). This is the sprint plan: ship the operator mesh and federate everything inside it. Phases 1–3 of the native plan are merged; this builds on delegated identity.

**The product, one sentence:** your agent, every machine you own — all tools, all agents, zero ceremony inside your trust graph.

---

## The boundary (the whole security model, three lines)

**You control what you can revoke.** Operator mesh = everything reachable from your root via the delegation graph. Foreign = every other root. Binary — no semi-trusted middle class, ever. Your buddy's homelab is foreign. The printer (via its proxy node) is yours.

Inside the boundary: compromise is possible, betrayal is not — a popped device is your incident and your revocation. Outside: betrayal by design is assumed — that's where consent, pins, spend policy, and attestation live. All boundary machinery sits *at or outside* the line, none inside.

Consequences: delegated keys never cross outward (foreign nodes receive requests and payments, never authority); foreign outputs are permanently untrusted context input (history raises exposure caps, never content trust); payment is the boundary's native protocol (inside the mesh money is meaningless — it's all you).

**Topology axes are not trust:** tags route (`region:office`, "print this in the living room"), subnets scope (gossip/observation bounded to the operator subnet), **keys decide**. Derivation is one-way: delegation graph → subnet membership, never the reverse. Tags are self-asserted labels; nothing about authority ever reads them.

---

## Phase 1 — Enrollment (the actual first deliverable)

One handshake, multiple transports. New device generates its own keypair locally (keys never travel); presents pubkey + proof-of-invite; operator side signs a delegation; device attaches to a peer. The signature *is* enrollment — nothing else is. Channels/QR/LAN are signaling for the *request*, never the admission.

- [ ] SDK primitives: `mesh.invite(name, tags) -> token`, `mesh.join(token)`, `mesh.approve(request)`, `mesh.revoke(device)`, `mesh.devices()` — human commands (`hermes mesh ...`) are frontends over these (H9)
- [ ] **Invite token** (the workhorse): rendezvous address + root *fingerprint* + nonce + short TTL, single-use. Not a key — pre-authorization to *ask*; leaked = someone can request for 10 minutes, visibly, deniably
- [ ] **Mutual fingerprint verification:** token carries the root fingerprint so the joining device confirms it's joining *your* mesh (evil-twin invite defense); shown both sides
- [ ] **QR = same token, encoded** — ships with the string, zero extra machinery
- [ ] Enrollment prompt does naming + tags in-flow: "name this device" (`pc`), optional `region:*` — this is where the machine-namespaced tool vocabulary is born, not a config step later
- [ ] Approval renders through existing Hermes approval surfaces (Telegram/Discord/desktop) — enroll the VPS from your phone
- [ ] **Revocation UI ships same day:** `mesh.devices()` + revoke. Revocability is the trust model; no roach motel
- [ ] **Revocation propagation is defined, not vibes:** revocation is a signed mesh fact; verifiers cache delegation status with a short TTL and re-check on cache miss or security-relevant calls; revoked devices fail closed at next policy check. **In-flight work is not guaranteed to abort** (session-abort where reachable, but the guarantee is at the invocation boundary). Bounded-staleness by design — same availability-over-durability religion as the journal
- [ ] Device-tier proxy enrollment: dumb devices (printer) enroll via a host proxy on the always-on node — approval reads "PC hosts device-tier proxy for 'HP printer'", delegation scoped down
- [ ] **Deprecate the shared-identity-file pattern** (`load_operator_identity` copied to each box): root stays on one machine/keychain; devices hold delegations. A mesh where every node *is* the root has no revocation story
- [ ] Stretch: LAN discovery + tap-to-approve (AirPods-pairing feel; both devices screened, same subnet)

**Acceptance:** fresh machine to enrolled-and-named in under 90 seconds via invite string; revoke kills its access on next invocation attempt; both fingerprints displayed during join; recorded.

> **Phase 1 status (2026-07-06) — Slice A (the enrollment crypto/data-structure core) landed; Slice B (the wire + orchestration + frontends) is the remaining headline.** Sliced like Phase 3 (SDK crypto core first, in-process + fully unit-tested, before any transport or 2-machine infra).
>
> **Slice A — `net_sdk::enrollment` + the delegation primitive (done).** The invite → join → approve handshake as pure, in-process types: **`InviteToken`** (a pre-authorization to *ask* — mesh root anchor + rendezvous + single-use nonce + short TTL — **not a key**; a leaked invite only lets someone submit a request, visibly, for minutes), **`JoinRequest`** (the device generates *its own* keypair locally and only its `EntityId` travels; it signs a domain-separated, length-prefixed challenge over `device ∥ name ∥ tags ∥ nonce ∥ root`, proving it holds the key and binding the request to *this* mesh), and **`EnrollmentAuthority::approve`** (fail-closed: wrong-mesh → expired → wrong-nonce → bad self-signature → single-use replay, in an order that never burns a legit invite on a garbage request, then signs the grant). The grant is a `root → device` **`DelegationChain::derive_device`** — a new delegation primitive that delegates to an **externally generated** device key (vs Phase 3's seed-derived machine/gateway), plus **`extend_delegate`** so the enrolled device locally extends `root → device → gateway` keeping `DELEGATE`. This is the concrete **deprecation of the shared-identity-file pattern**: the root stays on one machine, each device holds a delegation to *its own* key, and **revocation reuses the Phase-3 model unchanged** — `net identity revoke <device>` bumps the device's floor and kills its gateway subtree without touching a sibling (the "revoke kills its access" acceptance clause, at the crypto layer). **Design note:** the token carries the **full** root `EntityId` (not just the plan's literal "fingerprint"), because the device needs the real root pubkey to anchor-verify the returned delegation and to bind it cryptographically to the invited mesh; `enrollment::fingerprint` renders that root as a short human-comparable string (`A1B2-C3D4-…`) shown both sides for the evil-twin/eyeball check. **Tests:** 3 delegation + 9 enrollment unit tests (happy path, expired, wrong-nonce, single-use replay, tampered-request-doesn't-burn-the-invite, foreign-mesh, canonical byte round-trips with magic separation + truncation/trailing rejection, end-to-end enroll→extend→revoke). Only new dep: `getrandom` (version-matched to core, the nonce CSPRNG). `cargo test`/`clippy --lib -D warnings`/`rustdoc -D warnings` all green.
>
> **Slice B — the wire + orchestration + frontends (deferred; the <90s acceptance lives here).** (1) The **rendezvous transport** the invite's `rendezvous` addresses and the round-trip that carries a `JoinRequest` out and an `Enrollment` back; (2) the SDK **`mesh.invite/join/approve/revoke/devices`** orchestration over Slice A + a **machine-shared device registry** (name/tags/entity-id/enrolled-at, mirroring the `RevocationStore` file discipline) backing `mesh.devices()`; (3) the base64 **invite *string*** + QR encoding at the transport edge; (4) the **`net mesh …` CLI** and the **PyO3 + plugin** surface (both-or-neither like the delegation binding); (5) **approval routing** through Hermes's existing Telegram/Discord/desktop surfaces; (6) **device-tier proxy** enrollment (printer via a host proxy, delegation scoped down). **Deferred to infra:** LAN discovery + tap-to-approve, and the real **fresh-machine-to-enrolled-in-<90s** + **revoke-denies-on-next-invoke** 2-machine acceptance.
>
> **Slice B1 landed (2026-07-06) — the device registry (transport-independent).** `net_sdk::devices` (`DeviceRegistry` / `DeviceRecord`, re-exported) is the operator's **machine-shared inventory** of enrolled devices backing `mesh.devices()`: `record` (upsert) / `list` / `get` / `mark_revoked` / `remove`, under the `RevocationStore`'s file discipline (cross-process advisory lock on a stable sidecar + atomic temp+rename write, lock-free reads). **Inventory, not authority** — `revoked_at` is display metadata; the enforcing floor stays in `RevocationStore`, so `mesh.revoke` bumps both. No key material stored (H8). 8 file-store unit tests; clippy `--lib -D warnings` + rustdoc `-D warnings` clean.
>
> **Slice B1b landed (2026-07-06) — the operator facade (transport-independent).** `net_sdk::operator::OperatorEnrollment` composes the three stores (authority + device registry + revocation store) into the operator-side `mesh.*` surface: **`invite`** (mint + track the outstanding invite by nonce), **`approve(request)`** (look up the referenced invite by nonce, run the fail-closed enrollment checks, record the device, retire the invite — single-use), **`revoke(device)`** (bump the `RevocationStore` floor to generation 1 — matching `net identity revoke` — **and** stamp the inventory in one call), plus **`devices` / `forget` / `pending_invites`**. The one primitive it intentionally omits is **`mesh.join`** — the networked *device* side; the transport calls `approve()` and ships back `enrollment.chain`. 7 tests incl. an **end-to-end revoke → deny** (enroll → device extends to a gateway → gateway chain verifies → `revoke` → the persisted floor applied to a fresh registry makes the chain fail). clippy + rustdoc clean.
>
> **Slice A + string codec + B2a landed (2026-07-06) — the invite string + the enrollment RPC contract (transport-independent).** (i) **Invite string:** `InviteToken::encode()/decode()` — a self-describing `net-invite:<base64url>` string (the copy-paste / QR artifact `mesh.join` consumes), whitespace-tolerant, rejecting a missing prefix / bad base64 / malformed bytes. (ii) **Enrollment RPC protocol:** `JoinOutcome` (`Admitted { chain }` | `Rejected { code, message }`) with a canonical codec + stable `reject::*` codes; **`JoinOutcome::into_chain`** is the device-side verification — it parses the admitted grant and confirms it **anchors at the invited mesh root and binds to this device** (defending the joiner against a rogue operator returning a chain for a different mesh/key). (iii) **Server handler:** `OperatorEnrollment::handle_join_request(bytes) → bytes` — parse the request, `approve`, answer `Admitted`/coded `Rejected`; never errors out of band. Decided: the transport is **direct-addressed nRPC** — a joining device knows the operator's `node_id` from `invite.root.node_id()`, so it `connect`s to the invite's `rendezvous` addr (peer pubkey = `invite.root`) and `Mesh::call(node_id, "net.mesh.enroll", request_bytes)`; the response is the `JoinOutcome`. Design chosen because it reuses the mesh's own handshake + NAT machinery + the capability-invoke request/response pattern rather than a parallel channel. Round-trip tested in-process (server bytes → device `into_chain`), plus malformed / unknown-invite / single-use.
>
> **Remaining B2b + frontends:** the live wiring — `Mesh::serve_enrollment(operator)` (register `handle_join_request` via `serve_rpc`) + `Mesh::join(invite_string, name, tags)` (`connect` + `call` + `into_chain`) + a 2-node `ClusterHarness` test; then the **`net mesh …` CLI** / **PyO3** / **plugin** frontends (both-or-neither), **approval routing** through Hermes's surfaces, and **device-tier proxy**. **Infra:** the `<90s` enroll + revoke-denies-on-next-invoke 2-machine acceptance, LAN discovery.

## Phase 2 — Federate everything (in-root)

- [ ] Every enrolled Hermes announces its full local toolset as native capabilities — terminal, files, computer use, browser, all of it. No per-tool ceremony, no category grants in-root (supersedes the frozen plan's three-level model *inside the boundary*; tiers still govern the boundary and beyond)
- [ ] **The enforcement hook ships anyway:** invoke-path policy check exists and currently answers allow-all for same-root — a *preset*, not a missing code path. Allowlists later = flipping a default, not surgery
- [ ] **Provider-side approvals are the only in-root gate:** the executing machine's Hermes runs its existing exec-approval flow exactly as if the request were local. The mesh adds reach, not authority — one toll booth, where it always was
- [ ] **Approval prompts route to the operator, not the requesting model:** mesh-originated dangerous calls surface through the gateway approval path (Telegram/desktop/etc.), never as text back into the calling agent's loop. If a target machine has no reachable operator surface, dangerous calls fail closed with a structured `approval_unreachable` — headless boxes need their approval channel configured, not their guardrails skipped
- [ ] Machine-namespaced model UX: `pc/terminal.run`, `mac/files.read`; capability tags (`gpu:true`) for placement-by-need; identical built-ins dedup by descriptor hash (three machines ≠ three `read_file`s in the prompt)
- [ ] **Dedup collapses presentation, never provider-local semantics:** filesystem/terminal/desktop tools are provider-local by definition (`pc/read_file` ≠ `mac/read_file` — different disks); only `provider_equivalent`-flagged capabilities failover-route, and account/credential context stays in the descriptor hash (two GitHub accounts never collapse — frozen-plan rule, restated here because federate-everything makes it live)
- [ ] Hundreds of tools = context problem, not security problem: total on the mesh, curated in the prompt via Hermes's existing tool_search deferral
- [ ] **Schemas by content hash:** announcements carry `schema_hash` (Datafort ref), consumers fetch-once/cache/invalidate-on-change; schemas signed by the *defining* node, byte-preserved through any forwarder — re-serialization is the drift bug. Gossip cost O(unique schemas), not O(tools × nodes)
- [ ] Non-negotiables unchanged even in-root: keys unrepresentable, profile journal crown-jewels, secrets in per-machine keychains
- [ ] **Brains stay separate:** federation shares tools, not profiles. One Hermes per machine, own memory. The moving/warm brain is the migration feature (frozen plan Phase 8), orthogonal — anyone reading "operator mesh" as "synced memory" is rediscovering multi-master

**Acceptance:** Mac Hermes lists and invokes `pc/*` including a dangerous tool that triggers the PC's own exec approval; kill a duplicated capability's provider mid-session, next call routes to another node; lid-close/reopen the Mac — tools vanish and reappear cleanly at assembly boundaries.

## Phase 3 — A2A open in-root

- [ ] Every enrolled agent reachable by every enrolled agent, zero pairing ceremony (pairing was designed for strangers; strangers are foreign by definition)
- [ ] Scope discipline from the frozen plan holds: A2A = parallelism (PC grinds while Mac talks to you) and trust boundaries; same-root *sequential* work uses direct capabilities — don't brief the amnesiac colleague
- [ ] Task lifecycle incl. day-one cancellation; context travels as Datafort refs because the other agent doesn't share your memory
- [ ] Scratch streams ephemeral, results promoted home explicitly (frozen plan rules)

**Acceptance:** couch test — Mac agent hands PC agent a long job, keeps chatting, cancels mid-run, PC demonstrably stops; result lands as an artifact ref.

## Metrics
Enrollment time (target <90s); same-root reach success; lid-close recovery; dedup ratio (unique schemas / total announced); parallel cancel round-trip; revocation-to-denial latency.

## Non-goals
Foreign *publication* (your tools to other roots — where attestation/exposure machinery lives, later ring); allowlist UI (hook ships, UI doesn't); memory sync (migration is the feature, multi-master is never); marketplace/payments beyond the consume hooks in Appendix A; LAN discovery beyond stretch.

## Risks
| Risk | Mitigation |
|---|---|
| Enrollment UX misses 90s and the whole plan stalls behind it | It's Phase 1 with a stopwatch acceptance; invite-string flow is deliberately the dumbest thing that works |
| Allow-all preset hardcoded instead of hooked | Acceptance includes flipping the preset to deny in a test and watching an invoke fail |
| Prompt bloat despite dedup | tool_search deferral + dedup ratio metric; measured, not assumed |
| Shared-identity files linger after delegation enrollment ships | Deprecation warning on load + migration note; a root-on-every-box mesh silently defeats revocation |
| Tag spoofing treated as authority somewhere | Grep-level review rule: policy code never reads tags; tags exist only in routing/display paths |

---

## Appendix A — Foreign tools: consume-only

V2's entire foreign story is *calling* other roots' capabilities. Publishing outward: out entirely.

- Foreign invocation passes the boundary machinery that already exists: consent (`requires_approval` on anything credentialed/unknown), spend policy hooks (payments optional via the x402 track when priced), exposure caps
- Foreign descriptors and outputs are untrusted context input, permanently — signed identity tells you *who*, never *whether they're lying*
- **Injection posture at consume time:** foreign tool descriptions/results enter context as data; anything shaped like an instruction in them gets no authority (the model can be fooled — the policy engine can't be asked nicely). Worst case stays "asked the policy engine, on the record"
- No delegated key ever crosses outward; foreign nodes get requests and (optionally) payments
- Acceptance when built: invoke one foreign capability behind a consent prompt; deny path works; nothing about the operator mesh's allow-all leaks across the boundary

## Probable future concern (noted, not planned)

**Wake-on-LAN as a house-node capability** (`device.wake`): "grab the file from the mac" while it sleeps. Fits the reach use case, cheap on the always-on node, retrofit-annoying — but not V2. Revisit when the lid-close asymmetry actually bites in daily use.
