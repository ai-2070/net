# Net CLI V3 — enrollment and operator lifecycle implementation plan

> **For Hermes:** After V2 acceptance and explicit implementation authorization, use the subagent-driven-development skill for one accepted slice at a time, with independent review. This document authorizes planning, not production edits or protocol publication.

**Status:** V3 continuation authorized by the user on 2026-09-20. V3-0 source re-survey/design preparation started at `346b4b8bfe74ee8399a5f4191bfc7b86eb7f3a84`; its exit gate is **not passed**. V2's agreed implementation is complete, including generated protected-client coverage, and its exact-head acceptance is recorded in §1 (master merge `b525725a5`, all workflows green). The first narrow V3 code slice implements local invitation policy only; no working enrollment service or protocol publication is claimed. See the V3-0 decision record below before implementing wrappers.
**Goal:** An operator can start and stop a real long-lived Net node with an explicit protected PSK source or with no operator-supplied PSK, send a join link, let a new device join the intended mesh and independently scoped organization, channel and subnet relations, survive restart, voluntarily leave any selected relation, and selectively remove it where an authority-side revocation mechanism exists, with verifiable and honestly scoped enforcement.
**Architecture:** Thin Rust/Clap commands over reusable SDK enrollment, node-lifecycle and authority mechanisms, backed by an explicitly running node/operator service and durable local state. Enrollment, observation, removal, startup and shutdown refer to real identities, processes and enforcement points; temporary supervisors, inventory records, PID files and credential files never stand in for deployment effects.
**Tech stack:** Existing `net-cli` / `net-mesh` executable, Tokio, `net-mesh-sdk`, signed organization/subnet credentials, root-anchored channel `TokenChain`s, current native transport and optional bootstrap adapters. No new global control plane.
**Planning source snapshot:** `234c3685a285353d89fd92a540eb33a826c44194`, in `C:/Users/chief/orca/workspaces/net/net-cli`. V2 implementation was actively editing the checkout during inspection. This is a source survey, not runtime acceptance or the eventual V3 implementation baseline.

## 1. Relationship to V2

V2 makes the existing publisher/consumer experience truthful and usable. V3 removes manual enrollment plumbing and supplies the next bounded operator lifecycle. It does not supersede, reopen, or silently enlarge V2.

Before starting V3:

- V2's accepted scope is complete: restore safety/persistence, DOC-0, CLI-2A/2B target resolution, CLI-DX automation, live typegen, and its documented MCP/native capability journey.
- Record the accepted V2 commit and actual required exact-head CI results. Do not infer completion from the current plan's partial implementation notes.
- Re-survey affected APIs at that commit; reconcile names, features, and tests below without undoing V2 decisions.
- Inherit V2's `--inspect-target`, profile precedence, bind validation, explicit execution modes, deadline budget, stdout/stderr framing, confirmation and error-code contracts.
- Retain temporary-supervisor `--local` warnings for legacy commands. Adding narrow live enrollment operations does not turn every Deck command into remote administration.

### V2 exact-head acceptance (recorded 2026-09-23)

- **Accepted V2 commit:** `b525725a572261fe5a115fd1188197443f13c584`, the
  master merge of PR #1035 ("Net-CLI V2", head `728714ce5`, merged 2026-09-22
  15:24 UTC).
- **Required CI at that exact commit, all success:** CI run 35747146487, plus
  Skills 35747146495, Web 35747146318, Coverage 35747146510 and Docs API
  35747146548.
- **The PR head's own push run was red, and both failures are explained**
  (`CI` 35707535028 at `728714ce5`). The PR-event runs (natsim, Web, Skills,
  Coverage, Docs API, CLA) were green. `CI` itself was covered by the push run.
  1. *Go bindings:* the C-ABI export baseline check reported two exports not in
     the baseline, `net_blob_ref_hash` and `net_mesh_blob_adapter_publish`.
     That is a baseline-policy failure, not a test failure, and it was absent
     at the merge commit.
  2. *WebRTC feature (native driver):* `enrollment_storage::tests::exclusive_owner_and_restart_preserve_exact_bytes`
     reopened after `drop(owner)` and got `Busy` (5877 passed, 1 failed).
     - Cause: the lock is `flock`, owned by the open file description. A child
       forked by another test thread shares that description until its exec
       closes the CLOEXEC descriptor, so in the multithreaded test binary the
       lock can briefly outlive `drop`.
     - This is a test race, not a storage defect: cross-process ownership and
       reopen after exit are unaffected.
     - Fixed on the V3 branch: the test retries `Busy` within a 5 s bound and
       names the cause. Any other error, or a lock that never frees, still
       fails.
- **Master after the merge is not V2 evidence.** Later master CI runs
  (`e87943452`, `a88b7c301`, `c47fd448a`) are red after unrelated merges; they
  are outside V2 and are not claimed here.

V2's deferred transfer holder, generic unary RPC, remote Deck, and crash-safe NetDB replacement are **not** prerequisites and are **not** implicitly moved into V3. Organization streaming remains a separate parallel plan; V3 can prove enrollment with unary calls. Neither serverless capability providers nor serverless anchors are required.

## 2. The complete operator journey

1. On operator A, select the real identity, authority stores, trust domain and reachable listener. Optionally select a protected PSK source; otherwise let `up` generate and persist a fresh protected trust-domain PSK. Start the explicitly named long-lived node with `up`, opting in to enrollment with `--enroll` (which requires the issuer identity and ledger store). Enrollment runs inside that node's process, not as a separate service; the node's lifetime and whether enrollment is active are both visible through its local control endpoint.
2. Create a short-lived join link for the intended target. Mesh and organization invitations can carry explicitly authorized, independently evaluated channel and subnet scopes. A device already on the mesh can receive a standalone organization, channel or subnet link.
3. On device B, inspect the link locally, confirm the intended roots/scope, generate or load its own persistent identity, and explicitly redeem. Root private keys never leave A.
4. A validates B's proof of identity, the invitation and current policy, and issues exactly the preauthorized bundle. Creating the invitation is the inviter's authorization: by default there is no second human approval. Only invitations explicitly created with `--require-approval` wait for a subsequent operator decision.
5. B durably installs the returned configuration/credentials and proves each requested live effect separately. An organization certificate is not a channel subscription; a channel token is not a live subscription until the publisher accepts it; an installed subnet credential is not an admitted subnet session. If only part completes, report the individual stages and provide a same-identity resume path.
6. B starts an ordinary provider/caller using the saved enrollment/profile through existing SDK/CLI paths. The operator sees identity, credential state, actual observed admission, and the source/freshness of that observation.
7. An invocation without separately granted execution authority is denied. Apply the explicit existing dispatcher/capability/provider permissions and prove the authorized call reaches the exact provider.
8. Remove B from one selected subnet. The selected enforcement points refuse B's old credentials, including after reconnect/restart; unaffected device C still works. B's unrelated organization/capability/subnet authority is not silently revoked.
9. Revoke organization membership separately and prove its own admission consequences. Report remaining transport and independently granted access rather than claiming the device has disappeared from the entire mesh.
10. On a separately admitted device, voluntarily leave a selected mesh, organization, channel or subnet relation, including while its authority is unreachable. Prove local participation and automatic renewal/rejoin or re-subscribe stop across restart, unrelated relations remain intact, and authority notification is reported separately from revocation. Later rejoining requires explicit intent and current authorization.

**Completion is this loop, not a help tree.** A successful link mint followed by manual PSK copying, an inventory deletion with continuing admission, or a generated certificate without live acceptance does not complete V3.

## 3. Source-grounded reuse and gaps

Paths below are relative to `net/crates/net/`. Source presence is not a claim of independently tested behavior.

| Mechanism | Existing source | Reuse and missing boundary |
|---|---|---|
| CLI execution contract | `cli/src/main.rs`, `cli/src/context.rs`, `cli/src/target.rs`, `cli/src/config.rs` | Reuse accepted V2 resolution; do not build a second profile/context system. |
| Invite and join crypto | `sdk/src/enrollment.rs` | `InviteToken`, `JoinRequest`, `EnrollmentAuthority`, `DeviceEnrollment` already exist. `net-invite:` has a root, locator, nonce and expiry; it is not complete first-contact PSK provisioning or an org/subnet credential. |
| Operator lifecycle | `sdk/src/operator.rs`, `sdk/src/devices.rs`, `sdk/src/revocation.rs` | `OperatorEnrollment` composes pending invites, inventory and delegation floors. Pending/spent invitation state is in memory; durable receipt/retry and scoped bundles need work. Inventory is not enforcement. |
| Live enrollment | `sdk/src/mesh_enroll.rs` | `Mesh::join`, `serve_enrollment`, `serve_enrollment_auto`, and renewal exist. Native `Rendezvous` assumes the PSK was provided out of band. Service handles must stay alive. |
| Existing delegation grant | `sdk/src/delegation.rs` | `derive_device` mints `INVOKE_ACTION | DELEGATE`. That is a real agent-authority grant, **not** a harmless membership receipt. Never give it implicitly just to reuse `JoinOutcome`. |
| Signed bootstrap material | `sdk/src/bootstrap_credential.rs`, `sdk/src/rtc_bootstrap.rs`, `cli/src/commands/anchor.rs` | Existing browser credential v2 separates invite expiry from a standing PSK's lifetime and includes a signature/trust-domain binding. It is secret-bearing, and the browser listener is optional; it does not establish native CLI bootstrap or selective PSK eviction. |
| Organization provisioning | `cli/src/commands/org.rs`, `cli/src/commands/node.rs`, `sdk/src/org.rs` | Reuse signed membership, explicit grants, local adoption and monotone floor persistence. Offline issuance/adoption is not an org invite service or remote membership inventory. |
| Channel authority and participation | `src/adapter/net/identity/token.rs`, `src/adapter/net/channel/config.rs`, `src/adapter/net/mesh.rs`, `sdk/src/identity.rs`, `sdk/src/mesh.rs`, `cli/src/commands/channel.rs` | Core already has root-anchored `TokenChain`, canonical `ChannelName`/`ChannelHash`, remote acknowledged `subscribe_channel_with_chain`, local publish-chain installation/gating, and acknowledged unsubscribe. The SDK currently exposes only one `PermissionToken` in `SubscribeOptions`, not a full chain; it also lacks exact publish-chain removal. The current CLI only reads channel visibility/inventory. A token is authority, a subscription is remote runtime state, and publishing is local fan-out through the subject runtime's own gate. |
| Subnet authority | `src/adapter/net/subnet/auth.rs`, `src/adapter/net/subnet/admission.rs`, `src/adapter/net/subnet/control.rs`, `src/adapter/net/subnet/provision.rs`, `cli/src/commands/subnet.rs` | Reuse `SubnetRef`, verified credential sets, challenge-bound admission and signed control facts. Existing `SubnetRevocationFloor` is subtree/generation scoped, not a per-subject removal object. |
| CLI/security witnesses | `cli/tests/org_adopt.rs`, `cli/tests/org_grant.rs`, `cli/tests/subnet_issuance.rs`, `tests/channel_auth.rs`, `tests/channel_auth_hardening.rs`, `tests/channel_auth_origin_binding.rs`, `tests/channel_identity_readiness.rs`, `tests/subnet_auth_e2e.rs`, `tests/subnet_revocation.rs` | Starting points for compatibility and inverse evidence, not a substitute for real CLI enrollment/removal subprocesses. |

### Additional verified mechanism boundaries

- **Org per-member revocation already exists.** `src/adapter/net/behavior/org_revocation.rs` keys floors by `(OrgId, EntityId)`. `OrgRevocationStore::apply_bundle` verifies signed bundles, reloads persisted maxima under an interprocess lock, merges monotonically, persists, and publishes its live view. Raise above the membership certificate's generation. V3 needs a thin explicit apply/status path, not a replacement org revocation protocol.
- **Disk coordination is not live propagation.** Same-process, same-path org store handles share a live core. This does not make a CLI write automatically refresh another running process or remote verifier. `MeshNode::install_org_revocation_store` wires the live runtime to its installed store; the V3 service must drive that actual owner. Preserve `DurabilityUncertain` and poisoned-store errors: an uncertain durable write cannot become ordinary success.
- **Org adoption already has an owner.** `src/adapter/net/behavior/org_authority.rs` provides `NodeAuthority::adopt/open`; `sdk/src/org/provision.rs::install_org_authority_node` loads and installs it. Reuse that persistence rather than creating another membership directory schema.
- **Local subnet withdrawal is not revocation.** `MeshNode::withdraw_subnet_admission` clears one peer's challenges/context; valid retained credentials can re-admit. `subnet_context_for` is a real current-context observation hook. `known_subnets` is observer topology, not an admitted-member roster.
- **Gateway installation is not endpoint enrollment.** `sdk/src/subnet.rs::admin` already wraps control-fact application, gateway credentials and boundary declarations. Gateway credentials and boundary declarations are wholesale replacements, not append operations; adding B must not discard unrelated local authority. Reuse these advanced hooks only for their actual role.
- **Subnet path zero is real authority scope.** A `SubnetRef` combines authority and topology path; path `0` is that authority's entire hierarchy, not a missing/default selector. Reject omitted scope instead of silently selecting root-wide authority.
- **Channel identity is not the 16-bit wire hint.** `ChannelName::new` and `ChannelName::hash()` deterministically produce the canonical name and `u64 ChannelHash` used by token scope and full checks; `NetHeader.channel_hash: u16` is only a routing hint. Invitation creation computes the canonical pair locally and, for subscription, verifies it against the controlled publisher's actual `ChannelConfig`. Same-`u16` collisions are expected and must remain name/`u64` separated; refuse only a supplied canonical name/`u64` mismatch or an authority lookup that substitutes the hint.
- **A verified token is not enough without a configured root.** A `PermissionToken` proves only that its named issuer signed it. The publisher's `ChannelConfig.token_roots` anchors `TokenChain::verify_authorizes`; self-issued `issuer = subject` credentials fail unless that issuer is actually configured as a root.
- **Subscribe and publish are different execution paths.** Durable installation of an exact token chain is credential state. `SUBSCRIBE` is a remote operation: the subscriber targets a derived publisher `NodeId`, presents the full chain through core `subscribe_channel_with_chain`, and becomes live only after ACK. That ACK authenticates the routing ID, not the publisher's full `EntityId`; invitation metadata must not be relabeled reciprocal publisher authentication. `PUBLISH` is local fan-out: the subject installs a chain into its own runtime and a caller-requested `publish`/`publish_many` must clear that local runtime's `ChannelConfig` gate. There is no remote publisher accepting a publish. Join emits no synthetic application payload. `unsubscribe_channel` is live runtime withdrawal, not credential revocation or durable leave.
- **The channel issuer/control owner is new V3 work.** Current source has deterministic name hashing, local `ChannelConfigRegistry`, and `Identity::try_issue_token`; it has no remote config-resolution or token-root service. V3 adds a channel-issuance facet to the selected durable authority owner. In v1 it may issue subscription links only for a publisher runtime that owner controls and can inspect, proving the signing root is currently in that publisher's `token_roots`. Publish redemption never silently installs a trust root into the subject runtime; the subject's managed `ChannelConfig` must already trust the root or join reports that stage unavailable. Remote publisher configuration attestations are future work.

Four gaps must be resolved before user-facing success claims: **first-contact provisioning without hand-managed PSKs; membership-only enrollment without ambient execution grants; channel invitation/install/live-participation lifecycle without confusing unsubscribe with revocation; selective subnet revocation without collateral scope-wide removal.** A bounded SDK/core extension is allowed in the future accepted implementation slice when these existing mechanisms do not compose. Do not conceal a protocol gap with a CLI-only workaround.

## 4. Product surface and execution ownership

The following is **proposed V3 syntax**, not shipped commands. Final parser factoring may change at V3-0; preserve the user journeys and existing executable name. Use one implementation for link parsing/redemption, not separate mesh/organization/channel/subnet engines; each relation still dispatches to its own native credential and enforcement mechanism.

| Proposed surface | Meaning and execution owner |
|---|---|
| `net-mesh up [--psk-from <SOURCE>] [--enroll]` | Start one real long-lived node under the selected profile/identity. With no source, generate and durably protect a fresh random trust-domain PSK on first start and reuse it for that profile; never use a public, empty or all-zero PSK. Foreground by default; `--detach` explicitly transfers ownership to a managed child only after readiness. `<SOURCE>` is a protected `file:`, `stdin`, or configured `kms:` reference, never a literal PSK on argv. `--enroll` is the explicit opt-in that makes this node the enrollment owner: it loads the selected issuer identity and durable ledger store, taking the ledger's exclusive lock, and opens the PSK-free Noise enrollment listener in the same process. `up --enroll` refuses before binding if the issuer identity or ledger store is missing, invalid or already owned. Without `--enroll` the node mints nothing and exposes no enrollment listener. |
| `net-mesh down` | Ask the selected locally managed node instance to drain and stop through authenticated owner-only local control, then verify that exact instance exited. It does not revoke enrollment, delete identity, rotate the PSK, or stop unrelated SDK processes. |
| `net-mesh node status` | Report the selected managed instance, exact incarnation, readiness, bind/public endpoint, identity fingerprint and control-owner state without exposing the PSK or treating a stale PID/metadata file as liveness. |
| `net-mesh up --enroll [--relay <HOST:PORT> \| --no-relay]` | (R2, implemented.) Register the enrollment node with a blind relay, from the profile `relay` or a project default that stays empty until a relay is deployed. Tokens and bundles then carry the relay as the fallback path; joiners try direct first. Registration is retried in the background, so a relay being unavailable never blocks start-up or direct joins. |
| `net-mesh relay serve --bind <ADDR>` | (R2, implemented.) Run a blind UDP relay in the foreground, with TCP splices on the same port number. It holds no PSK, issuer key or mesh credential and is not a mesh member. |
| `net-mesh enrollment stop` / `enrollment start` (optional) | If offered, local control operations on the running `up --enroll` node that close or reopen only its enrollment listener; the node and its ledger ownership stay up. Not a separate process. `start` on a node launched without `--enroll` refuses. |
| `net-mesh invite create` | Create a mesh invitation through the running `up --enroll` node's local control endpoint, which records it in the ledger it owns before returning the link; optional exact `--subnet` scope. Refuses when no enrolling node is running; never opens the ledger itself. |
| `net-mesh invite inspect` | Offline, redacted parse/signature/expiry inspection where verifiable; never consumes a nonce or claims current authority. |
| `net-mesh invite revoke <invite-id>` / `invite status [<invite-id>]` | Through `up`'s local control endpoint: invalidate an unredeemed invitation, or report non-secret offer/claim/issuance state, at the ledger's single owner. This does not revoke previously issued credentials or a leaked standing PSK. |
| `net-mesh join <join-link>` | Enroll a new device into the stated mesh; persist and apply the selected bundle. Also support protected file/stdin input to avoid shell-history leakage. |
| `net-mesh org invite <org-ref>` / `org join <join-link>` | Explicit organization membership enrollment, optionally composed with mesh connectivity and subnet attachment. No owner replacement. |
| `net-mesh channel invite <channel-name> --rights <RIGHTS> [--subscribe-from <entity-id>]` / `channel join <join-link>` | Preauthorize one subject-bound channel chain, then install it and report each right through its actual execution path. `subscribe` requires a publisher runtime controlled by the issuer owner and names its intended full entity as metadata/routing selection; `publish` installs on the subject's local managed runtime and does not use that flag. Mixed rights do both independently. `<RIGHTS>` must contain `subscribe`, `publish`, or both; optional bounded `delegate` is never standalone. No implicit `ADMIN`, wildcard, root installation or capability invocation. Canonical name/`u64` are computed locally; no command accepts a `u16` hint. |
| `net-mesh subnet invite <subnet-ref>` / `subnet join <join-link>` | Explicit endpoint attachment invitation; standalone join reuses an already-connected identity. |
| `net-mesh leave <mesh-ref>` | Voluntary local departure from the selected mesh enrollment; disables its automatic attachment/renewal and stops its controlled runtime participation, without deleting device identity or claiming transport-secret revocation. |
| `net-mesh org leave <org-ref>` | Voluntary local deactivation of that organization enrollment and dependent use; not an owner transfer or issuer-side membership revocation. |
| `net-mesh channel leave <channel-name> --subscribe-from <entity-id>` / `channel leave <channel-name> --publish-local` | Leave the selected channel execution path, or specify both flags to leave both. Subscribe leave persists disabled intent and obtains acknowledged unsubscribe from the routing target. Publish leave conditionally removes only the exact managed local chain/cache incarnation. If several matching relations exist, require `--relation <id>` instead of broad deletion. Neither form revokes copied token bytes. |
| `net-mesh subnet leave <subnet-ref>` | Voluntary local withdrawal from that exact qualified subnet; unrelated memberships and authority remain unchanged. |
| `net-mesh enrollment status` / `enrollment devices` | Through `up`'s local control endpoint: whether enrollment is active on that node, and issuer inventory, with separately attributed live observations. Not an omniscient mesh roster. |
| `net-mesh org members <org-ref>` / `subnet members <subnet-ref>` | Authorized issuer inventory plus scoped enforcement-point observations, distinguishing issued from live-admitted and unknown. |
| `net-mesh org remove <org-ref> <entity-id>` | Revoke the selected membership relation using the actual organization floor/admission mechanism. |
| `net-mesh subnet remove <subnet-ref> <entity-id>` | Revoke the selected subject's covered attachment authority; requires the selective-revocation mechanism below. Not a synonym for list deletion or disconnect. |

Use full cryptographic subject identifiers for mutations, not ambiguous names, truncated fingerprints, or a routing `u64`. Display names are untrusted labels. A shortened display is not a signing/lookup identity.

There is deliberately no `channel members` claim in this slice. A publisher's live subscriber roster is ephemeral observation, not a durable authority inventory, and possession of an unexpired chain does not prove a current subscription. Channel status reports installed credential state, configured trust root, intended subscription target/routing ID, ACK observation, local publish-gate observation and unknowns separately.

For `subscribe`, `--subscribe-from` binds an intended full publisher identity into invitation metadata and derives the routing `NodeId`; the current ACK proves only that routing target. It is not observed reciprocal authentication of the publisher's `EntityId`. The current `TokenChain` binds subject, action, canonical channel hash, issuer/root, time, generation and delegation depth, not one publisher. Inspection/status must disclose both limits and same-root cross-publisher portability. `publish` has no remote publisher relation: the subject's local runtime is the publisher and its own configured root/gate decides.

Local operator commands must operate on the same durable authority state as the running service, through a locked local control path or authenticated bounded SDK service. Choose the minimum existing-compatible path at V3-0. Spawning a fresh `OperatorEnrollment` for every invite while the server owns a different in-memory map is invalid. Remote issuance/removal must never be exposed merely because a caller holds a PSK or membership certificate; keep remote mutation unavailable unless the exact management authority is implemented and tested.

**Enrollment hosting (user decision, 2026-09-23):** there is no separate `enrollment serve` process. `up` owns an ordinary Net node lifetime, and enrollment authority only when the operator explicitly passes `--enroll`, which refuses without a valid issuer identity and ledger store. The enrolling node owns the ledger lock, the PSK-free enrollment listener and the invitation state for its whole lifetime. `invite create`/`revoke`/`status` and `enrollment status` are clients of `up`'s owner-only local control endpoint; they never open the ledger or instantiate a coordinator themselves. `enrollment stop`/`start`, if offered, are control operations on that running node, not processes. Starting a PSK-authenticated node without `--enroll` must not mint invitations, expose management operations or grant application invocation rights; `down` stops the node and therefore its enrollment listener, without revoking anything already issued.

## 5. Non-negotiable authority and secret boundaries

### 5.1 Independent relations

| Relation | What it establishes | What it never implies |
|---|---|---|
| Transport configuration/session | Connectivity within a stated trust domain | Organization membership, subnet attachment, management or invocation rights |
| Mesh/device enrollment record | Issuer-approved device association and its named credentials | Generic agent delegation or universal mesh access |
| Organization membership | One device belongs to its owner org | Dispatcher rights, foreign-provider access, or permission to replace an existing owner |
| Channel token chain and accepted live action | Exact `PUBLISH`/`SUBSCRIBE`/bounded `DELEGATE` authority on one canonical channel under configured roots; separately, remote subscribe ACK or local publish-gate acceptance | Organization membership, reciprocal publisher `EntityId` authentication, subnet attachment, service invocation, `ADMIN`, wildcard access, or durable roster membership |
| Subnet credential and live proof | An exact subject can attach at a qualified target under current policy | ROUTE, EXPORT, administration, or capability execution |
| Dispatcher/capability grants | Their explicitly signed rights and scope | Bypass of provider-local admission |
| Inventory/control-channel visibility | Information observed or recorded | Authority to mutate or to manufacture admission proof |

Use current production identity bindings; document where `EntityId`, transport keys and routing IDs differ. Never substitute a self-asserted packet field for authenticated subject identity.

### 5.2 Link and bootstrap contract

- Join links are the primary interface; QR encoding, hosted landing pages, OS URL-handler installation, and mobile-specific UI are deferred.
- Explicit `join` performs redemption. Inspection, preview GET/HEAD, unfurling, completion and help must not redeem or approve anything. If no HTTP landing page ships, test the offline inspector and any existing bootstrap endpoints that a preview could reach.
- A versioned, integrity-bound invitation names issuer/root, allowed operation, exact target scope, expiry, single-use identifier, and optional intended device identity. Organization, channel and subnet authority must be independently verifiable; one issuer's signature does not grant another authority's rights. A channel invitation binds canonical name/hash and requested token rights. For `SUBSCRIBE` it also binds intended publisher metadata/routing selection without pretending the token or ACK authenticates that full `EntityId`; `PUBLISH` has no remote publisher target.
- **Invitation TTL: 24 hours by default (user-approved).** Compute the signed expiry from creation time plus 86,400 seconds; support an explicit `--ttl` override. Inspection, pending approval and failed redemption do not restart that clock. At or after expiry, an unissued invitation cannot issue credentials, including through late approval. The invitation remains single-use. This TTL is not a membership/grant/PSK lifetime and does not reclaim previously delivered secrets; committed same-device receipt recovery has its own bounded policy below. An override ceiling is not yet selected; the previously suggested seven-day maximum is not an accepted requirement.
- **Default: preauthorized invitation, secure redemption.** Creating a link authorizes its exact operation/scope for one redeeming identity within its expiry. This is the normal interactive and headless flow, not a special auto-approve shortcut. No second inviter prompt is required. An optional full intended-device identity restricts who can redeem; without it the link is bearer authorization and the first valid, durably committed claimant wins. Creation/inspection must disclose that risk. Possession of a link is not generic management or invocation authority.
- **Optional: request-and-approve.** `--require-approval` on invitation creation records a signed, durable policy requiring a subsequent operator decision for the exact claimant/intent. Neither the device nor a service-wide convenience flag may downgrade it. This mode remains useful when the inviter wants to verify an unknown device before issuing credentials. Do not route either mode through legacy `serve_enrollment_auto`: its delegation outcome is not V3 membership-only enrollment.
- Obtain first-contact transport material through authenticated redemption, after invitation validation, identity proof and durable authorization/claim commit (plus human approval only in the optional mode). V3-0 must prove a native CLI path from a clean device with no PSK. Scope the required adapter explicitly; do not require the parked serverless project. Secure delivery and human approval are separate decisions.
- Do not embed an existing private deployment's standing PSK in a supposedly harmless invite. Any chosen secret-bearing link mode requires explicit operator opt-in, a named trust domain, secret-file/stdin support and a clear warning: invite expiry/revocation does not expire or erase the standing PSK. Never call that mode approval-only transport access.
- No root private keys in links, device responses, examples, logs or generated profiles. Redact bearer strings, PSKs, audience secrets and request proofs from diagnostics/traces; only explicit secret export may emit credential material to a protected destination.
- Validate size, version, signature, expiry, operation, endpoint scheme/address and trust binding before connection or mutation. Do not follow redirects to an unpinned authority, fetch arbitrary URLs while inspecting a link, or claim a self-signed root is independently trusted. Human confirmation trusts the intended issuer; crypto preserves that binding thereafter.

### 5.3 Durable identity and redemption

Model the minimal durable transitions: preauthorized offer → identity-bound claim → issued receipt → device installed → live admission observed. The optional request-and-approve mode inserts pending approval → approved before issuance. Expired/revoked/denied and partial states are explicit, not exceptions hidden by a success message.

- Persist the device key before the first redeem request. A retry uses that key, not a regenerated identity.
- Persist the winning identity and exact scope/result before returning issued credentials. Same-identity retry returns the same committed result; another identity cannot redeem the same invitation. Bind the complete request intent, not only its nonce.
- Serialize competing redeem/revoke/expiry operations at the owner. Claim/issuance and optional approval completion recheck current policy and expiry. Do not hold a storage mutex across an unbounded human prompt. Invalid proofs cannot consume or reserve an invitation. Optional approval binds one verified claimant; replacing a denied claimant requires explicit operator action or a new invite, not automatic takeover by a racing request.
- Partial bundle issuance does not claim distributed atomicity. Record which authorities issued which mesh, organization, channel and subnet credentials; resume only the same approved intent and identity, rechecking current revocation. Never undo an already-issued grant merely by deleting local files.
- File failures leave a recoverable state and no final `joined` result. Profile publication must not point at missing identity/credential files. Avoid clobbering existing ownership, credentials or profiles; conflicting ownership is a refusal, not an implicit migration.
- Bound outstanding requests, receipts, pending approvals and retained history; fail closed on capacity. Retain replay protection for its required lifetime and require tested cleanup rules across restart.
- Ordinary renewal must not revive a removed member or expand scope. Re-enrollment after removal is a separate explicit, floor-aware issuance operation; never lower floors or switch identity automatically to make it succeed.

### 5.4 Managed node lifecycle and PSK sources

`net-mesh up [--psk-from <SOURCE>]` is the explicit local runtime owner missing
from the current one-shot CLI. It starts a production `MeshNode`, not a temporary
inspection supervisor. The selected profile/incarnation has one owner-only
control endpoint and one exclusive lifetime lock. A second `up` for the same
profile refuses with the live instance identity rather than starting a competing
node or trusting a PID file.

When `--psk-from` is omitted and the selected managed profile has no PSK yet,
`up` generates 32 bytes with the operating system CSPRNG, persists them through
the same protected-file owner used for other node secrets, and then starts the
node. A restart reuses that value; it must not silently create a new trust domain.
Concurrent first starts have one durable winner under the profile lifetime/store
lock. A generated PSK is never printed by default. Joining another node still
requires the accepted invitation/bootstrap flow or an explicit protected secret
export; “no supplied PSK” does **not** mean an unencrypted, unauthenticated,
empty, well-known or PSK-free wire mode. The current `MeshBuilder` requires a
32-byte PSK, so a genuinely PSK-free transport would be a separate protocol and
wire-security decision outside this CLI slice.

When a source is supplied, the initial source contract is deliberately narrow:

- `file:<path>` reads a bounded 32-byte raw or 64-hex PSK from a protected,
  regular, non-symlink file using the CLI's existing platform permission gates.
- `stdin` reads the same bounded form without echoing it or placing it in argv,
  environment, profile output or shell history. Detached startup must transfer
  the already-read secret to the child over a private inherited pipe and wait
  for child readiness; it must not rewrite the secret into a temporary file.
- `kms:<provider>:<opaque-ref>` resolves through an explicitly configured,
  allow-listed provider adapter using the process/workload identity. It means a
  retrievable secret-manager value, not a non-exportable signing-key handle and
  not arbitrary shell execution. Unsupported providers/features fail before
  node startup. Exact provider adapters and URI grammar must be pinned before
  implementation; this plan does not create a universal credential vault.

The selected source or generated-secret owner yields exactly one PSK value for
startup, validates it before binding, and scrubs transient copies after
constructing the runtime. Raw `--psk-hex` is not accepted by `up`. Existing
one-shot compatibility flags/profile fields remain a separate legacy surface
until an explicit migration is accepted. Diagnostics, target inspection,
readiness metadata and process listings expose only `generated` or supplied
source kind and a redacted reference/fingerprint, never the value. A
KMS/secret-manager fetch must not place cloud credentials in Net configuration;
provider-native workload identity remains outside Net.

Foreground is the default so containers, service managers and terminals own the
process honestly. `--detach` is explicit and returns success only after the child
has acquired the lifetime lock, bound its sockets, installed the selected
identity/configuration, started its receive/runtime loops, published its protected
control endpoint and acknowledged readiness. Parent exit, pipe closure, a stale
metadata file or a spawned PID is not readiness. Startup failure leaves no
success output and no child that later becomes live unexpectedly.

`net-mesh down` resolves the same profile and exact incarnation through the
protected local control endpoint, requests bounded drain/shutdown, and waits for
that instance's acknowledged termination. PID reuse cannot select a process.
Timeout is a nonzero partial result with the node still reported running or
unknown; force termination, if later offered, is an explicit separate mode and
cannot be described as a graceful stop. Repeated `down` is idempotent only for
the same recorded incarnation. Shutdown removes advertisements owned by that
runtime where possible, closes streams/listeners and releases the lifetime lock;
it preserves identity, enrollment, PSK source, stores and audit receipts. It
does not revoke already issued authority or claim secure erasure.

PSK rotation is outside `down`: changing the referenced value while a node is
running does not mutate the live trust domain. A restart or future explicit
rotation ceremony is required, with honest mixed-key behavior. `node status`
must distinguish `starting`, `ready`, `draining`, `stopped`, `stale metadata`
and `unknown`, and must verify the live control owner rather than infer health
from files.

## 6. Selective removal and propagation

### 6.1 What removal must mean

`subnet remove S B` affects B under the explicitly selected authority-qualified subnet scope S. Its preview names exact scope, subject, covered rights/credential incarnations, known enforcing nodes, and possible remaining access. Do not silently bump a subtree-wide floor and revoke siblings to simulate a per-device command.

The current subnet floor object has no subject field. V3 must therefore either prove an existing subject-selective production mechanism at its eventual baseline or implement a minimal authenticated subject-scoped revocation extension in the subnet authority layer. Define its namespace, signer authority, ordering, topology-epoch binding, generation/reissue semantics, persistence and verifier integration before freezing wire bytes. Reuse existing floor/control delivery and cache invalidation where semantically valid; do not introduce a global membership consensus service.

- Exact removal must cover all credentials that can still authorize the claimed action. Ancestor grants, multiple issuers, alternate paths and exported access require explicit accounting. If the selected issuer cannot revoke all applicable authority, report residual access or refuse the stronger claim.
- Decide and document treatment of ATTACH versus independently granted ROUTE/EXPORT before shipping. Default endpoint removal must not accidentally disable an unrelated gateway or another subject.
- Organization removal composes the actual membership revocation state; it does not clear all org grants or revoke foreign relationships by convenience. Prove independent org/subnet removal and preserve unrelated authority.
- Disconnecting a peer, pruning inventory, stopping renewal, or deleting B's files is not enforced removal. The removed device is uncooperative and keeps its old bytes in the test.
- Persist revocation before reporting committed; integrate with active admission/session checks and caches, not only future credential issuance. Old state or a restarted verifier cannot restore previously acknowledged authority.
- Repeated removal is idempotent; delayed removal for an old incarnation must not erase a separately authorized newer incarnation. Generation comparisons and identifier exhaustion fail closed.

### 6.1a Decision: subject floor (user-authorized 2026-09-23)

**Mechanism.** A root-signed **subject floor**. It is the only option that
matches "remove this subject from this scope" without revoking siblings or
waiting for expiry. The rejected alternatives were a subtree floor bump plus
reissue to every sibling (collateral revocation), and expiry-only removal
(residual access until the grant lifetime ends).

**Constraints, all binding on the implementation:**

1. **Exact authority and identity.** The floor is root-signed. It carries an
   authority-qualified scope, the full subject `EntityId` and explicit covered
   rights. No routing IDs or other ambiguous identifiers.
2. **Monotone, durable enforcement.** Accepted floors are persisted before
   anything reports them committed. Old facts, restarts, renewal and
   re-enrollment must never lower them.
3. **Precise scope semantics.** The design must specify how ancestor-scoped and
   delegated (`SubnetIssuerGrant`) credentials interact with the subject floor.
   A still-valid alternative credential must not silently defeat the removal
   the command claims.
4. **Explicit generation semantics.** Pin which credential generation is
   compared. Intentional re-admission requires authorized issuance at or above
   the floor; retrying `join` is not enough.
5. **Independent rights.** Define whether the command removes ATTACH only or
   more rights. It must never silently broaden to ROUTE/EXPORT.
6. **Honest propagation.** An owner commit is not fleet-wide enforcement.
   Report applied and pending enforcement points separately.
7. **Mixed-version safety.** An unsupported verifier must not silently ignore
   the new fact while the CLI reports a successful removal. Verify the wire-kind
   allocation before reserving a kind (e.g. 5), and update every affected
   decoder and fixture together.
8. **Collateral churn is disclosed.** Reusing the authority-wide
   `subnet_auth_epoch` invalidation can force *sibling* sessions to re-admit.
   Their grants stay valid, but they are not operationally "untouched". Prefer
   subject-scoped invalidation where practical; otherwise disclose and test the
   temporary churn instead of claiming zero collateral disruption.

**Decisive witness.** B loses access with its old credentials, including on a
real reconnect; C stays authorized in the same subnet; and the result survives
a verifier restart.

**Pinned design (source-surveyed 2026-09-23, before any code).** Each item
answers the constraint with the same number above.

1. **Artifact.** `SubnetSubjectFloor` v1, signed over the domain
   `net.subnet.subject-floor.v1`. Fields:
   - `authority` + `path`: the authority-qualified scope S;
   - `topology_epoch`;
   - `issuer`: must be a configured **root**; delegated issuers cannot sign it;
   - `subject`: the full 32-byte `EntityId`;
   - `rights`: strict, non-empty mask;
   - `minimum_generation`;
   - `revision`: ordering per `(scope, subject)`;
   - `issued_at`.

   It travels as control-fact kind **5** (`subject_floor`). The allocation was
   verified before reserving: `SubnetFactKind` uses 1..=4, nothing uses 5, and
   the only other subnet tag space (`SubnetCredentialSet` 1/2) is a separate
   decoder, told apart by exact length.
2. **Monotone and durable.**
   - The registry keeps one generation per
     `(authority, topology epoch, path, subject, right bit)`. It is **never
     lowered**.
   - A fact applies only with a strictly higher revision for its
     `(scope, subject)`. A replayed or reordered fact is an `applied: false`
     no-op.
   - Accepted floors of both kinds (subtree and subject) are persisted in a
     node-owned protected store (`EnrollmentStorage`: atomic replace, exclusive
     owner). The store holds the latest signed bytes per key.
   - It is written **before** `apply` returns `Ok(applied: true)`. If the write
     fails, the in-memory registry keeps the stricter state, since floors only
     remove authority, and `apply` returns `Err`, so nothing reports it
     committed.
   - At start-up the store is loaded, every fact is re-verified against the
     configured roots and re-applied **before** any admission. A corrupt store
     refuses start.
3. **Scope semantics.**
   - The floor is checked against the **admitted target (attachment) path**,
     not the grant's scope. A subject floor at S therefore applies to any B
     session attaching inside S, whichever grant it presents, including a grant
     scoped at an ancestor of S. An ancestor-scoped alternative credential
     cannot defeat the removal.
   - Delegated credentials: a `OneHop` leaf (issued through a
     `SubnetIssuerGrant`) is refused for a floored right inside S **regardless
     of its generation**.
   - Re-admission is therefore only through a root-direct grant (item 4).
     Holding an issuer grant does not let B re-issue itself back in.
   - B's authority outside S is untouched.
4. **Generation.** The **leaf** grant's `generation` (the subject's own
   credential) is compared with the floor's per-right generation. B is
   re-admitted to S only by a root-direct leaf with
   `generation ≥ minimum_generation`. Retrying `join` presents the old leaf and
   is refused.
5. **Rights.**
   - The floor covers exactly the rights it names. Admission is refused when B
     requests any covered right inside S.
   - Issuance defaults to **ATTACH only**. ROUTE and EXPORT are covered only
     when named explicitly. Nothing broadens silently.
   - Peer contexts are consulted at their attachment (the relay reads
     ingress/egress attachment; forwarding rights come from the gateway's own
     credentials). So the floor governs where B may be admitted, and says
     nothing about the gateway's own forwarding authority.
6. **Propagation.**
   - Apply returns `{kind, applied}` to the local caller only. On the channel
     path an old or unreachable verifier is silent.
   - No remote readback path exists yet. The CLI therefore reports
     **issued / signed**, not "removed", and names enforcement as `pending`
     until a verifier's own apply result is observed.
   - A management readback verb is a follow-up; this slice does not claim
     fleet-wide enforcement.
7. **Mixed versions.**
   - A pre-kind-5 verifier decodes tag 5 as `InvalidFormat`. On the API path
     that is an error returned to the caller (reported as `unsupported`). On
     the channel path it is dropped and logged, which is why item 6 never
     reports it as applied.
   - The fixture and every decoder are updated in one commit:
     `stable_kinds.json` `fact_kinds`, SDK `render_stable_kind_fixture`,
     `fact_kind_wire`, the DTO, and the Node, Python and Go kind tests.
8. **Invalidation is subject-scoped.** Applying a subject floor does **not**
   advance the authority-wide `subnet_auth_epoch`. It drops only the contexts
   whose `subject` is B, whose authority is A, whose attachment lies inside S,
   whose requested rights intersect the covered rights, and whose leaf fails
   the generation rule. Siblings keep their contexts and sessions; the witness
   asserts C's **same** context survives, not a re-admission. Subtree floors
   keep their existing authority-wide behaviour.

### 6.2 Honest scope of the result

A removal result separates:

1. **Authority committed:** the authentic, durable revocation at its owner, with revision/operation identifier.
2. **Delivered:** which named enforcement points received the update.
3. **Applied:** which points authenticated it and advanced the actual enforcement state.
4. **Observed denied:** which real protected operation was subsequently refused under that revision.
5. **Unresolved:** unreachable/stale/unsupported points and known residual authority.

An adjacent ACK is not an application receipt; an applied revision is not a probe result. A local revocation file is not global mesh convergence. Freeze the required verifier set for a bounded wait and identify the responder/instance/revision in its evidence. New peers must load the current required state before accepting the protected scope; unreachable peers remain explicitly unknown rather than counted as successful.

Default mutation completion may mean durable owner commit, with clearly named pending propagation. An explicit bounded wait succeeds only at its requested assurance level; timeout exits nonzero and preserves the operation receipt, without implying rollback or repeating the mutation. Do not invent a globally complete roster from observed announcements.

**Transport caveat:** Removing membership or subnet access does not take back a shared PSK already known to B. State whether B can still establish transport or access public/unrelated services. Whole-mesh transport expulsion requires its own verified session/admission or key-rotation mechanism; V3 must not claim it from an org/subnet removal.

### 6.3 Voluntary leave: local intent, not forced removal

`leave` is initiated by the participating device's local operator. `remove` is an authority operation against a potentially uncooperative subject. Both use the shared lifecycle/status machinery, but they have different postconditions.

- Resolve the selected enrollment and preview its dependent providers, callers, channel subscriptions/publish chains, subnet attachments and renewal tasks. Require the existing V2 confirmation contract for consequential changes; `--yes` bypasses the prompt, not scope validation. Never select all meshes/organizations/channels/subnets by default.
- Persist a scoped **left/disabled intent** before attempting authority notification. Fence in-flight join/renew/install callbacks so delayed success cannot reactivate it. Startup and renewal paths must honor this state; explicit later join is the only way to restore intent, and it still needs current authorization.
- Stop new work under the selected relation and deactivate its controlled runtime use, renewal and automatic reattachment. Define a bounded shutdown/drain policy for already-active work; do not replay calls or claim cancellation reverses completed remote effects. Remove affected advertisements/attachments where the runtime owns them.
- A CLI process cannot stop arbitrary independent SDK processes by editing a profile. Use the actual local runtime/control owner selected at V3-0. If an existing consumer cannot be fenced or acknowledged, report **local configuration disabled; runtime stop unconfirmed** and do not emit complete-left success. An explicitly offline mode may disable next-start use, but must disclose its weaker guarantee.
- Deactivate only the selected enrollment's credential references. Subscribe leave deactivates the exact `(intended publisher metadata, derived routing target, canonical channel, token-chain incarnation)` and acknowledges unsubscribe where controlled. Publish leave deactivates the exact `(local managed profile, canonical channel, chain incarnation)`. V1 permits one active managed publish chain per profile/channel; a conflicting install refuses rather than overwriting. Add conditional chain/cache removal hooks before claiming live publish stop: a stale leave cannot remove a successor, and an uncontrolled `TokenCache` fallback yields **runtime stop unconfirmed**, not success. Preserve other subscriptions/channels and shared references. Preserve the device key, unrelated memberships, retained application data, audit receipts and revocation maxima. No secure-erasure claim and no automatic deletion of user data.
- Leaving an org does not erase historical ownership or authorize adoption by a different org. Retain the existing single-owner guard; ownership migration remains outside this plan. Leaving a channel does not create a durable publisher-side revocation: copied token bytes remain usable until expiry or actual revocation. Leaving a mesh makes dependent use over that connection unavailable without pretending it revoked independent organization/channel/subnet credentials.
- Local departure must work while the authority is offline. Attempt a bounded, identity-authenticated notification when possible and retain a resumable receipt otherwise. Show **left locally; authority notification pending** separately from **authority notified**. No issuer private key is needed to stop one's own participation.
- Notification is advisory unless the authority explicitly commits a supported self-revocation request. Authenticate the subject and bind the request to exact relation/incarnation; it must never authorize removing another device. Reuse the removal mechanism for any actual revocation, with the same durability/propagation evidence. Do not make self-revocation a prerequisite for local leave.
- Retries are idempotent. A delayed leave notification or callback for the old enrollment must not deactivate or revoke a newer explicitly accepted enrollment. Where the existing revocation semantics cannot safely distinguish them, refuse stale self-revocation rather than raising a floor against the successor.
- Status must distinguish `active`, `left locally`, `stop unconfirmed`, `notification pending`, and authority-confirmed revocation without treating those labels as one shared authority state. Rejoin must not clear floors, bypass issuer approval or generate a new identity as an escape hatch.

### Leave acceptance boundary

Successful default leave means the durable local intent is disabled and the controlled live runtime has stopped the selected participation. Remote notification may remain pending and is explicitly reported. It does **not** prove all copies of the credential are unusable elsewhere. A required live-stop timeout is a nonzero partial result with its receipt retained; an authority-notification timeout alone does not undo completed local departure.

## 7. Ordered implementation slices

Implementers choose coherent factoring and review boundaries. Each code-bearing task starts with a failing witness, adds the smallest correction, runs the focused GREEN and relevant regressions, and records an independent inverse for authority/durability claims. Do not invent passing counts or write production APIs from these proposed names without checking the accepted baseline.

### V3-0 — rebase and close the concrete mechanism decisions

**Files to inspect:** V2 and all source paths in §3; `AGENTS.md`, `TESTS.md`, `.github/workflows/ci.yml`.
**Modify:** this plan's source/decision table only, after V2 acceptance.

#### Initial source re-survey — 2026-09-20

The user authorized proceeding with V3 after the V2 implementation handoff.
This record is preparatory design work, not a waiver or fabrication of V2's
acceptance gate. Candidate baseline: `346b4b8bfe74ee8399a5f4191bfc7b86eb7f3a84`.
There is **no accepted V2 SHA yet**. The user reported pushing the earlier
`7c8942dc1` journey head; the generated protected-client follow-through is
`9a4323d33` with receipt `346b4b8bf`. Do not assume those latter commits have
run CI. Cross-org generated-client journey coverage was explicitly deferred in
V2 and is not inherited as V3 release debt.

Paths in this table are relative to `net/crates/net/`. “Proposed” means a
design direction requiring the listed proof, not an implemented mechanism.

| Boundary | Re-survey finding | V3 direction / remaining decision |
|---|---|---|
| Operator ownership | `sdk/src/operator.rs::OperatorEnrollment` owns an in-memory `pending` map; `EnrollmentAuthority` separately tracks spent nonces. Inventory/revocation file persistence does not persist either invitation ledger. | Decided: the `up --enroll` node holds a lifetime lock on its selected authority store and owns durable invite/claim/receipt transitions. Mint/approve/revoke clients must talk to that owner; never instantiate a fresh coordinator. Root keys stay with that owner. Choose and test protected local IPC on Unix and Windows before adding mutation commands; remote management stays unavailable. |
| Native first contact | `sdk/src/mesh_enroll.rs::Rendezvous` contains address, Noise public key and routing ID, but explicitly assumes an out-of-band PSK. `Mesh::join` starts from an already-built mesh. | Product choice resolved: preauthorized, single-use invitations redeemed through an authenticated adapter, without a standing PSK in the link or a second approval by default. Optional `--require-approval` is invitation-bound policy. The concrete transport/control proposal below remains subject to security witnesses and V3-0 exit, not a shipped guarantee. No public/default PSK workaround or secret-bearing mode is selected for V3-1. |
| Existing browser bootstrap | `sdk/src/bootstrap_credential.rs::BrowserBootstrapCredential` is signed and secret-bearing. SDK HTTP/TLS dependencies and the CLI listener are gated by `rtc-bootstrap`, which also enables WebRTC. | Reuse verification/secret-redaction concepts, not a silent browser-feature dependency. This is not evidence for native secure redemption. Do not enable `rtc-bootstrap` globally to make a new default command appear to work. |
| Membership-only outcome | `sdk/src/enrollment.rs::JoinOutcome::Admitted` contains a delegation chain; `sdk/src/delegation.rs::derive_device` issues `INVOKE_ACTION | DELEGATE`. `InviteToken` itself has no issuer signature or operation/scope fields. | Preserve existing agent enrollment and `NMI1`/`NMO1` behavior. V3 needs a separately versioned integrity-bound invite and membership-only receipt/bundle, never an empty/fake delegation chain. Proposed receipt binds issuer, full subject, invitation/operation ID, exact requested relations, request-intent digest and committed result; finalize encoding after transport/store decisions. |
| Channel participation | Core channel auth verifies `TokenChain` against `ChannelConfig.token_roots`, exact subject, canonical `u64` channel hash, action, time, generation, revocation and attenuation. Core subscribe has acknowledged `subscribe_channel_with_chain`; SDK `SubscribeOptions` accepts only one `PermissionToken`. Publish is local fan-out through `set_publish_chain` plus the local gate. `published_chains` is one chain per channel hash, has no exact remove hook, and `TokenCache` fallback can still authorize. CLI `channel` is read-only. | Add a new durable channel-issuance facet to the selected owner. Compute canonical name/hash locally. In v1, subscription issuance supports only a publisher runtime that owner controls and whose config proves the issuer root is trusted; the intended full publisher identity remains metadata while ACK authenticates only derived routing ID. Export `TokenChain` through the SDK façade and add a full-chain subscribe method/options path with delegated multi-link and reconnect witnesses. Publish requires the subject's existing local config to trust the root; it uses no remote publisher acceptance. Add exact-incarnation publish-chain and cache removal hooks, or report stop-unconfirmed; permit one active managed publish chain per profile/channel and refuse overwrite. |
| Managed node lifecycle | The CLI can create short-lived attached clients and temporary supervisors, but has no generic command that owns a persistent production `MeshNode`, proves readiness, or stops that exact runtime. Profiles may contain plaintext `psk_hex`; there is no generated managed-secret owner, shared PSK-source abstraction or KMS resolver. | Add explicit `up`/`down`/`node status` over one owner-only lifecycle endpoint and exact incarnation. Pin foreground/detached ownership, readiness, drain and stale-state semantics. With no supplied source, generate and protect one stable profile PSK; otherwise accept bounded `file:`/`stdin`/configured-`kms:` sources. Do not turn Net into a credential vault or accept raw PSKs on `up` argv. |
| Ordinary consumer / leave | CLI `config.rs` and `context.rs` have no enrollment-intent or renewal lifecycle integration. A saved profile alone cannot fence a running independent SDK process. | Proposed: shared SDK durable intent/receipt owner and an enrollment reference in existing profile resolution; controlled CLI consumers register instance/incarnation with a protected local lifecycle owner. Leave first persists disabled intent, then requires scoped stop acknowledgements. Unmanaged consumers remain stop-unconfirmed. Pin IPC, liveness and fail-closed startup semantics before promising live stop. |
| Selective subnet removal | `src/adapter/net/subnet/auth.rs::SubnetGrant` has subject, rights and generation; `SubnetRevocationFloor` has scope/epoch/generation but no subject. `control.rs::SubnetFactKind` accepts four strict V1 tags. | No existing per-subject floor can simply be invoked. Proposed root-signed subject floor keyed by qualified scope, topology epoch, full subject and explicitly covered rights, with monotone floor/revision and explicit reissue. Ancestor credential coverage, ATTACH versus independent ROUTE/EXPORT, durable load, active-context invalidation and unsupported-peer refusal remain mandatory design decisions. Do not allocate a new wire tag yet. |

**Next bounded work:** validate the concrete transport/control proposal below,
including its feature boundary, before implementing command wrappers. Only
after those decisions and V2 acceptance evidence are recorded should V3-1 start
with RED witnesses for clean-device first contact, shared durable state and no
implicit execution grant. V3-0's other tasks below remain open; this table is
not their completion receipt.

**Validation of this record:** source inspection and CodeGraph navigation only;
no runtime, crash, inverse, cross-language, or CI acceptance evidence. Current
CI still owns separate default/`rtc-bootstrap` CLI runs and the existing subnet
integration pins; new root test binaries will need explicit pins. No production
files, existing wire formats, feature defaults or public command documentation
are changed by this preparatory slice.

#### Invitation policy decision and secure-redemption design — 2026-09-20

**Accepted product decision:** sending an invitation normally is the inviter's
authorization. The user explicitly selected preauthorized invitations with
secure redemption, not mandatory second approval and not PSK-bearing links.
The standard journey is **create → send → confirm issuer/scope → redeem → join**.
`--require-approval` is an explicit alternate creation policy, signed into the
invite and stored by its owner. It adds a pending decision; it is not the default.
Creation requires existing local operator authority in either mode.

**Adapter decision — PSK-free Noise enrollment listener (user-selected
2026-09-22, superseding the earlier HTTPS proposal):** redemption cannot ride the
existing mesh listener, because its Noise NKpsk0 handshake mixes the PSK into the
first message and a clean device has none. It also does not add a pre-PSK
handshake type to the mesh socket, which would widen the core admission path
every peer runs. Instead the `up --enroll` node owns one separate TCP enrollment
listener (TCP rather than UDP; see the session receipt below), distinct from the
mesh socket and local management IPC. The device
runs a PSK-free Noise handshake that authenticates the responder by the X25519
static key signed into the invite (`EnrollmentKey`), then performs challenge →
signed request → bundle inside that session. It reuses the workspace Noise stack:
no TLS certificates to provision, no HTTP/TLS/WebRTC dependency and no
`rtc-bootstrap` coupling. The exact Noise pattern, prologue/protocol-name domain
separation from mesh NKpsk0 (and whether the key is the node's mesh static key
or a dedicated enrollment key), reliable delivery of a bounded bundle over UDP,
retransmission, anti-amplification (response no larger than the unauthenticated
request until the handshake completes), per-source rate limits and challenge
capacity are the next slice's design items and require witnesses before the
listener is exposed. No plaintext fallback, PSK-bearing mode or public management
operation is added. Rotating the enrollment key invalidates unredeemed links
unless a later rotation contract is accepted.

Feature placement: because no new dependencies are expected, the listener is
proposed to build under the existing SDK `net` feature. If the implementation
does need a new dependency, stop and put it behind a separately reviewed feature.
Offline inspect and the typed receipt/store mechanisms must never need the
listener. Release-binary inclusion and a CI job exercising the live listener are
still required before claiming the shipped join journey.

**Invite/request/response contract (semantic, not allocated wire bytes):**

1. The versioned issuer-signed invite binds full issuer identity, named trust
   domain, TCP enrollment endpoint and its Noise static key, random invitation identifier,
   expiry, exact authorized relations, optional intended full device identity
   and approval policy. The link contains no standing PSK, root key or audience
   secret, but **is still sensitive bearer authorization** when subject-unbound.
   Signature verification preserves the supplied issuer binding; the recipient
   must confirm that this is the intended issuer, not trust any self-signed root.
2. Offline inspect verifies what it can and performs no network request or claim.
   Only an authenticated enrollment session can reserve, approve, issue or return
   a credential bundle; no unauthenticated probe or preview has effects. Redact
   invitation identifiers/proofs/bearer material from diagnostics; return only
   non-secret operation identifiers by default.
3. Before redeeming, the device persists its identity and canonical request intent.
   Inside a Noise session that proved the pinned enrollment key, it requests a
   fresh, bounded, short-lived server challenge.
   The device signs a domain-separated transcript binding that challenge, the
   signed invite digest, full subject and complete intent digest. The challenge
   is bound to this live session (its handshake hash) and consumed once; a captured signature
   cannot obtain the secret response on another connection. Capacity limits,
   expiry and refusal paths must be tested before exposing the listener.
4. The owner verifies the invite against its durable record, proof, subject,
   scope, expiry, current policy and revocation before claiming it. Under a short
   store transaction, exactly one verified identity/intent wins. Default mode
   proceeds to issuance; require-approval mode records pending intent and releases
   the transaction before waiting. Approval cannot change the winning intent or
   bypass a revoke/expiry that wins before issuance commit.
5. Persist the exact winning receipt/bundle before returning secrets on the same
   authenticated connection. The bundle contains transport configuration and
   only the explicitly requested membership/attachment artifacts; no legacy
   delegation chain or implicit execution/management rights. A receipt must bind
   its full subject and intent. A retry needs a fresh connection challenge and
   proof from that same device, then returns the same committed issuance rather
   than minting again. Current revocation blocks renewed secret delivery; expired
   unspent invites cannot issue. Already-committed receipt recovery after invite
   expiry is bounded by a separately recorded recovery deadline, does not extend
   credential lifetime, and must be distinguished from first redemption.
6. The device verifies and installs the receipt, then uses normal authenticated
   mesh attachment. Report credential installation and observed live admission
   separately. Once delivered, a standing PSK cannot be reclaimed by expiring or
   revoking the invitation; org/subnet admission continues to be independent.

The existing `JoinRequest` signs device/name/tags/nonce/root, not these additional
policy, intent and connection-challenge fields. Do not reuse its signature domain
or reinterpret `NMJ1`/`NMO1`; keep legacy APIs unchanged. New formats need bounded
decoding, explicit versions and compatibility witnesses before publication.

**Proposed local control owner:** Unix domain socket inside an owner-only service
directory; on Windows a local-only named pipe restricted to the owning user with
explicit DACL and remote-client rejection. Both require client identity/access
checks and an exclusive lifetime store lock, not trust in a pathname or PID alone.
The service publishes non-secret endpoint/instance metadata only after ownership
is acquired; clients never instantiate a replacement store on connection failure.
Local mint/approve/revoke/status use this owner, which is `up`'s node control
endpoint when the node runs with `--enroll` (one control endpoint, not two). Device-side lifecycle control is
a separate local instance, not an assumption that the remote issuer can stop
arbitrary consumers. Exact runtime registration/stop fencing remains open.

**Required first witnesses:** default redemption without any approver callback;
require-approval cannot issue until approved; tampered policy/scope/pin refusal;
default expiry is exactly creation plus 24 hours, explicit TTL is honored, and
the exact expiry boundary refuses unissued invitations without clock reset;
wrong-subject and invalid-proof attempts leave the invite unclaimed; two distinct
claimants have one durable winner; replay on a different connection yields no
PSK; fresh same-device recovery returns byte-identical issuance; revoked/expired
unspent invites fail; GET/HEAD/inspect have no effects; membership-only success
cannot invoke a protected handler until an explicit independent grant is added.
Store-crash and live transport witnesses must exercise production transitions,
not just a model or mocked successful response.

**Gate remaining:** this resolves the invitation UX choice and supplies a concrete
adapter/control proposal. It does not complete V3-0: feature/release inclusion,
receipt retention/persistence details, lifecycle fencing, selective subnet floor
semantics/compatibility and V2 exact-head acceptance still require closure.

#### First code slice — local invitation policy

The user's subsequent “let's start” authorizes a narrow implementation of the
settled policy while those wider gates remain open. This is not acceptance of
the proposed bootstrap protocol or permission to expose an enrollment listener.
Owner: `sdk/src/enrollment/policy.rs`; witnesses: `sdk/tests/enrollment_policy.rs`.
The policy carries creation/expiry times and preauthorized versus require-approval
mode, defaults to exactly 86,400 seconds, and accepts positive whole-second TTL
overrides with checked timestamp arithmetic. There is no selected product TTL
ceiling yet; overflow is an error, never saturation. Redemption-time inspection
refuses times before creation or at/after expiry and cannot mutate the policy.

This is an input policy primitive, **not an authorization verifier**: its result
does not verify a signature, consume an invitation, approve a request, issue a
credential or deliver a PSK. No serializer/wire version, CLI command, listener,
new Cargo feature or legacy enrollment behavior is changed. The eventual durable
owner must apply this policy together with authenticated invite/proof validation,
single-use state, current authority and explicit optional approval. Committed
receipt recovery is a separate path and must not reset or reuse first-issuance
expiry checks as an excuse to mint again.

**Implementation receipt (`43b4f1f5c`, 2026-09-20):** RED was the new integration
binary failing to compile because `enrollment::policy` did not exist; this was
API-absence evidence, not an observed pre-existing runtime defect. GREEN:
`cargo nextest run -p net-mesh-sdk --test enrollment_policy --no-tests=fail --retries 0`
passed all **7** tests. An in-place, temporary inverse changed the production
expiry comparison from `>=` to `>`: the exact-boundary witness failed with
`Ok(Preauthorized)` instead of `Expired`. Restoring the comparison restored all
7 passes; no inverse edit remains. This is a local policy-boundary inverse, not
a durable authority/transport enforcement witness.

Compatibility: the focused SDK lib filter `test(enrollment::) + test(operator::)`
passed **42** tests with zero retries (274 unrelated tests filtered out).
`cargo clippy -p net-mesh-sdk --lib -- -D warnings`, SDK formatting and
`git diff --check` passed on Windows/Rust 1.98.1 with default SDK features.
The ccc skill guided navigation/index maintenance; ccc/CodeGraph refreshed.
Existing Rust SDK CI auto-discovers this new integration binary with `net`
enabled; no new root integration pin is required. Full-feature rustdoc, broad
workspace/optional-feature gates and exact-head CI were not run locally.
Locally committed, not pushed. Next: close the durable receipt-store contract
and bootstrap/control feature boundary before wiring this policy into issuance;
do not mistake this initial primitive for completed V3-0 or V3-1.

#### Durable receipt-store contract — next V3-1 boundary

Source review after `c013da7d5` confirms that SDK `devices.rs::save_atomic`
and `revocation.rs::save_atomic` are not sufficient templates for this secret
store: their parent-directory flush is best-effort. Core
`behavior/org_revocation.rs::write_atomic_phased` distinguishes pre-publication
and post-rename uncertainty, uses a fresh create-new temporary file, requires
Unix parent-directory flush, and uses Windows write-through replacement.
Core `behavior/org_authority.rs` owns secure directory creation/validation,
including inheritable Windows DACL rules. Those helpers are internal, not a
ready-made public SDK receipt-store API. Do not copy the weaker inventory writer
or provision a fake organization merely to obtain a protected directory.

**Selected storage shape:** a bounded, versioned snapshot owned by one live
service, rather than a database service or a generic transaction framework.
Keep the semantic ledger in proposed `sdk/src/enrollment_store.rs`. The next
code-bearing prerequisite is a narrow reusable protected-file owner around the
existing core I/O/security mechanisms, with its own Unix/Windows tests and no
changes to organization revocation semantics. The precise extraction/API must
be reviewed before widening internal helpers; do not export arbitrary unchecked
paths or a write primitive that silently creates insecure parent directories.

**Owner and transaction rules:**

- Initialization is explicit and creates a protected directory, stable lock,
  issuer-bound store header and empty snapshot. Ordinary open requires them;
  missing/corrupt/unsupported state never becomes an empty ledger. A full issuer
  mismatch refuses open. Root keys are not serialized into this store.
- Hold an exclusive, nonblocking lifetime lock on the stable sidecar. A second
  owner refuses startup rather than waiting indefinitely. Keep the handle alive
  through all tasks and shutdown; never unlink the lock to recover a stale PID.
  The eventual local control endpoint belongs to this owner. Other CLI processes
  do not read credential-bearing snapshots or write their own copies.
- Within the owner, serialize state transitions under a short mutex. Build and
  validate the candidate from the current revision, persist it, then publish its
  in-memory view and answer. Do not hold this mutex while waiting for a human or
  network operation. Revision/counter exhaustion is an error, never wrapping.
- A pre-rename failure returns an error and leaves the previous committed view.
  A post-rename durability failure places the owner in an **uncertain** state:
  no issue/recovery response or further mutation until explicit reopen/recovery
  has established which complete snapshot is durable. Do not report rollback or
  retry issuance blindly. No secret response or external credential publication
  may occur before the corresponding successful durability boundary.
- On restart, acquire the lifetime lock, validate security/header/bounds and the
  complete snapshot, and establish durability before serving. In this single-file
  design, an interrupted unpublished mutation may recover as the previous or
  candidate snapshot; neither result may forget an earlier acknowledged commit.
  Do not automatically merge a backup or reconstruct spent records from inventory.
  This is crash consistency, not protection against an administrator restoring an
  old disk image. Org/subnet issuance with external effects needs explicit staged
  receipts in V3-2; this snapshot does not claim distributed atomicity.

**Minimal record and transitions:** store issuer/invitation ID, digest of the
complete signed invite, immutable `InvitationPolicy`, exact scope and optional
intended subject. Claim binds full device `EntityId` plus canonical intent digest.
Approval is for that exact claim; a later approval message cannot replace it.
Keep public operation IDs distinct from bearer invitation material.

| Current state | Allowed transition | Required condition / retry result |
|---|---|---|
| Offered, preauthorized | Claimed/ready | Verified current invite and device proof; subject/scope match; still within first-issuance window. Persist the winning identity/intent before any issuance. |
| Offered, require-approval | Claimed/pending | Same checks; no credentials. Approval changes only this claim to ready after current-policy and expiry recheck. |
| Claimed/pending or ready | Same state | Same identity **and** intent may resume. A different claimant or changed intent is refused; an invalid proof cannot reserve state. |
| Claimed/ready | Issued | Recheck expiry, policy and current authority; persist exact result bytes and issuance/recovery times before returning them. |
| Offered or claimed | Revoked/denied | Owner-authorized terminal transition. Revoke/deny winning before issuance prevents later approval or issuance. No automatic reset to offered. |
| Issued | Issued | Fresh same-device proof and identical intent may recover the original bytes within the recovery window, subject to current credential/transport validity. Never reissue on retry. |
| Issued | No invitation-revoke transition | Return already-issued with its non-secret receipt ID; revoke the actual membership/grants separately. Expiring/revoking an invite cannot undo a delivered PSK. |

The verified-proof and current-authority checks belong to the authenticated
redemption/issuer owner, not to a caller-supplied boolean in an exposed ledger
API. A store test may supply trusted fixture inputs but cannot be advertised as
proof of the signature gate. Connection challenges are short-lived and separate
from the durable claim: a restart invalidates old challenges, not the claim.

**Recovery/retention direction:** propose a fixed 24-hour receipt-recovery window
from successful issuance, independent of the invitation's 24-hour first-issuance
window. Persist its exclusive deadline with the receipt; retries never extend it.
This is an engineering proposal, not the user's invitation-TTL decision. A fresh
proof may recover after invitation expiry, but never after recovery expiry or
current membership/transport invalidation. Transport rotation must not cause a
retry to silently return newly issued material; return a typed stale-receipt result.
Once recovery closes, discard secret payload only through a durable compaction
that retains a spent/revoked tombstone until the signed invitation cannot be used
again. An issued receipt is never replaced by a new offer with the same ID.

Set separate record-count, per-receipt-byte and total-store-byte ceilings before
exposure; capacity refusal is before mutation and never evicts live replay state.
Bound reads before allocation and strictly reject unknown schema/state, duplicate
IDs, impossible transitions/timestamps and invalid lengths. Decoder errors and
Debug output must not echo receipt bytes, bearer material, PSKs or proofs. Secret
file removal is not secure erasure; no such claim is made.

**Required persistence witnesses (partial evidence below):** second process cannot own the
same store; an insecure/symlink/non-regular path refuses; repeated replacement
retains Unix modes/Windows DACL; missing/corrupt state refuses open; injected
pre-rename failure preserves old committed state; injected post-rename failure
returns uncertainty and blocks secret delivery; reopen resolves only a complete
durable snapshot. Then place barriers at claim/issue/revoke commit and exercise
two real processes, exact-byte lost-response recovery, expiry and approval races,
wrong subject/intent, crash points and capacity. These must reach production
transitions, followed by inverse mutations in a disposable review worktree.

**Protected storage implementation (`e1c687379`, 2026-09-20):**
`src/adapter/net/behavior/enrollment_storage.rs` now owns a fixed opaque snapshot
and stable lifetime lock, reusing the existing org directory/file validation and
phased atomic replacement helpers. Creation refuses existing directories; open
never initializes missing state. Reads/writes have a 64 MiB ceiling, and existing
insecure or hardlinked files are refused rather than repaired by replacement.
Post-rename uncertainty fences subsequent reads/writes until close/reopen; reopen
re-persists the checked snapshot under the lock before returning it. This is a
workspace-internal byte-storage primitive, not an SDK ledger or credential verifier.

Windows validation: `cargo tfl -E 'test(enrollment_storage) + test(org_authority) + test(org_revocation)' --retries 0`
passed **80 tests** (5,706 filtered), including six storage tests. Witnesses cover
same-process and real child-process lock contention, exact-byte reopen, missing
state, hardlinks, oversized reads, permissive Windows ACL refusal, and injected
pre-/post-rename outcomes. The child writes a marker so zero test discovery cannot
masquerade as lock evidence. Default-feature `cargo clippy --lib -- -D warnings`,
touched-file rustfmt and `git diff --check` passed. Tests were added with the code;
no pre-implementation RED is claimed. CI now explicitly includes the module in
Windows security tests; nextest sets its retries to zero.

**Inverse evidence:** in the Orca-managed disposable review checkout at
`e1c687379`, removing the production `self.uncertain = true` assignment made
`phase_failures_preserve_or_fence_reads_and_writes` fail at its read-fencing
assertion (one test discovered, zero retries). The mutation was restored from the
committed source and verified by an empty Git diff. During restoration verification,
a NUL-filled review source and a corrupt generated PDB separately prevented
compilation; neither is test evidence. The primary source/commit remained intact;
the review source was replaced and only the named rebuildable PDB was removed.
After recovery, `cargo tfl enrollment_storage --retries 0` passed **6/6** in the
restored review checkout (5,780 filtered). No inverse changes remain. The candidate
and this receipt are locally committed, not pushed; exact-head CI is unverified.

Limits: Unix-only assertions have not run locally; exact-head CI, actual power-loss
testing and schema/corruption/issuer validation remain outstanding. The latter
belong to the forthcoming SDK ledger: opaque storage cannot reject semantically
invalid bytes. No SDK wiring, receipt issuance, bootstrap listener or CLI command
is implemented by this slice. Same-account/privileged filesystem mutation and
restoring old disk images remain outside this storage boundary.

**Handoff:** next bounded implementation is the versioned, issuer-bound SDK ledger
and its claim/approval/receipt transitions above. Bootstrap feature/release
inclusion, final recovery/retention bounds, lifecycle fencing, selective subnet
semantics and V2 exact-head acceptance remain open; V3-0 is not complete.

**SDK ledger implementation (2026-09-22, uncommitted working tree):** final path
`sdk/src/enrollment/store.rs` (child of the existing `enrollment` owner, beside
`policy.rs`, reusing its private bounded `Reader`) rather than the proposed
top-level `enrollment_store.rs`; witnesses `sdk/tests/enrollment_store.rs`.
`EnrollmentLedger` wraps core `EnrollmentStorage` with a versioned (`NMEL` v1),
issuer-bound, blake3-checksummed snapshot and implements the transition table
above: offer → claim (preauthorized ready / pending) → approve exact claim →
issue once → same-claimant byte-identical recovery; revoke/deny terminal before
issue; issued offers report `AlreadyIssued`, never reissue. Operator-facing
calls take a non-secret `OfferId`; device-facing calls take the redacted
`InvitationId`. Refusals never mutate; mutations persist before publishing in
memory; storage `Uncertain` fences every call until reopen. The decoder rejects
bad magic/checksum/trailing bytes, unknown tags/bools, duplicate offer/invitation/
digest/receipt IDs, out-of-window claim/issue times, pending preauthorized claims,
denied preauthorized offers, intended-subject mismatches and any recovery deadline
other than exactly issuance + 24 h. `InvitationPolicy::from_stored` (crate-private)
rebuilds persisted policy. Limits (default 4,096 records / 64 KiB payload / 16 MiB
snapshot; hard 65,536 / 1 MiB / 64 MiB) gate new mutations only. `compact` drops
payloads at the recovery deadline and removes records only once the invitation
has expired and nothing is recoverable. Recovery window is the proposed fixed
24 h; it remains an engineering choice, not a user decision.

Still **not a verifier**: signatures, device proofs, intent, current authority
and revocation are the future redemption owner's checks; inputs in tests are
trusted fixtures. No wire invite format, listener, CLI command, control IPC or
feature change. Ledger-level post-rename fencing and crash barriers are not
witnessed (the storage layer's are, above; the ledger has no injection seam yet),
nor are real two-process races beyond same-process lock refusal.

Windows/Rust 1.98 validation: `cargo nextest run -p net-mesh-sdk --test
enrollment_store --test enrollment_policy --no-tests=fail --retries 0` passed
**19** (12 new); SDK lib `test(enrollment::) + test(operator::)` passed **42**;
`cargo clippy -p net-mesh-sdk --lib -- -D warnings` and `--all-targets` (CI `-A`
flags) clean; `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk --no-deps
--features full` clean; touched-file rustfmt clean (`cargo fmt --all` hits the
Windows command-length limit). Tests were written with the code; no pre-existing
RED is claimed. **Inverses** (in place, each restored byte-identically from a
backup): disabling the intended-subject check, the issue-time expiry recheck, the
snapshot checksum, the claim-conflict check, allowing reissue of an issued offer,
and making the recovery deadline inclusive each failed exactly its named witness.
Exact-head CI and Unix execution are unverified. SDK CI auto-discovers the new
binary; no new root pin is needed.

**Signed membership invite and intent (2026-09-22, uncommitted working tree):**
`sdk/src/enrollment/invite.rs`, witnesses `sdk/tests/enrollment_invite.rs`.
`MembershipInvite` (`NMM1`, signature domain `net-mesh membership invite v1`,
join token `netmesh-join_<base64url>` — see the join-token decision below, ≤1 KiB) signs over the full issuer `EntityId`, a
trust-domain label (`[A-Za-z0-9._-]`, 1..=64) plus the existing public
`TrustDomainId`, a strict UDP `host:port` enrollment endpoint (DNS name, IPv4 or
bracketed IPv6; port required; no scheme/path/userinfo/whitespace), the
enrollment responder's X25519 Noise static key (`EnrollmentKey`),
a CSPRNG `InvitationId`, the `InvitationPolicy`, an optional intended subject and
a canonical relation set. v1 defines only `Relation::Mesh` (membership-only);
unknown tags, empty/duplicate/unordered sets refuse, and organization/channel/
subnet relations get their own tags and verifiers in V3-2/2A. Decoding bounds
input before base64, rejects trailing bytes and verifies the signature against
the **embedded** issuer — integrity only; the human still confirms the issuer
fingerprint. No PSK/root key/secret is carried, but a bearer link is still
secret; `Debug` redacts the identifier and link. `digest()` covers the complete
signed bytes, `scope_digest()` covers issuer + trust domain + relations, and
`offer_spec()` produces the ledger record. `check_trust_domain` lets a device
refuse a delivered PSK from another domain. `RedemptionIntent` (`NMN1`) binds the
invite digest, full subject and exact relations; `check_against` is the owner's
recheck and `claimant()` yields the ledger `Claimant`. Added public
`TrustDomainId::from_bytes` (decode only; equality with `of_psk` remains the
check). Legacy `NMI1`/`NMJ1`/`NMO1`, `net-invite:` and delegation enrollment are
unchanged; a legacy link is refused by the new decoder.

Not included: the connection-bound challenge/proof of key possession, the
receipt/bundle format, enrollment listener, CLI and any cross-language codec. This
format is SDK-internal until those exist; it is not a published wire contract.

Validation (Windows): new binary **10/10**; with ledger and policy binaries
**29/29**; SDK lib `enrollment:: + operator:: + bootstrap_credential::` **55/55**;
SDK clippy lib/all-targets, full-feature rustdoc, touched-file rustfmt and
`git diff --check` clean. **Inverses** (restored byte-identically): ignoring the
signature result, allowing duplicate relations, accepting `http://`, skipping
the intent subject check, skipping the intent digest check, accepting any trust
domain, and printing the invitation id in `Debug` each failed their named
witnesses (every-byte tamper sweep, foreign-issuer splice, etc.).

**PSK-free Noise enrollment session (2026-09-22):** `sdk/src/enrollment/redeem.rs`
(protocol, responder key, client `redeem`) and `sdk/src/enrollment/service.rs`
(`EnrollmentService` over a `SharedLedger`); witnesses
`sdk/tests/enrollment_redeem.rs` over real loopback TCP.

Resolved design items from the adapter decision above:

- **TCP, not UDP**, on its own port. The TCP handshake removes the
  retransmission, fragmentation and amplification design a UDP responder would
  need; a spoofed source cannot obtain a response. The invite's signed
  `host:port` string is unchanged; its docs now say TCP.
- **`Noise_NK_25519_ChaChaPoly_BLAKE2s`**, prologue `net-mesh enrollment session
  v1`: the device authenticates the responder by the invite's `EnrollmentKey`
  and stays anonymous at the Noise layer. Protocol name and prologue separate it
  from mesh `NKpsk0`.
- **Dedicated responder key**, not the mesh static key: X25519 secret =
  `blake3::derive_key("net-mesh enrollment responder x25519 v1", issuer seed)`.
  Stable across restarts with no extra storage; rotation (and its invalidation
  of unredeemed links) is future work.
- **Challenge = Noise handshake hash.** It is fresh on both sides and unique to
  the session, so the device signs `domain ‖ handshake hash ‖ invite digest ‖
  subject ‖ intent digest` and a captured proof fails on any other session. One
  request per session; frames are `u16` big-endian length-prefixed.
- **Service verification order**, all before any ledger mutation: bounded
  request decode, invite signature, intent against invite, subject signature
  over this session's transcript, invite issuer = ledger issuer, and presented
  invite digest = recorded digest (new `EnrollmentLedger::invite_digest`). Then
  claim → pending / issue via the caller's `BundleIssuer` / recover (gated by
  `BundleIssuer::may_recover` for current authority). Refusals are coarse
  (`Invalid` covers unknown invitation, bad invite, mismatched intent and bad
  proof). The ledger lock is held across `BundleIssuer::issue`, so the issuer
  must be local and bounded.
- **Bounds:** 64 concurrent sessions (excess closed, never queued), 10 s session
  deadline, 128-byte handshake frames, 4 KiB request, 32 KiB bundle. No
  management operation is exposed remotely; the owner uses `service.ledger()`.
- **Dependencies:** SDK `net` now enables `dep:snow` (0.10.0, already in the
  workspace via `net-wire`; the lockfile gains only that edge) and
  `tokio/net`/`io-util`. No HTTP/TLS stack; `bootstrap_dep_boundary` passes.

Validation (Windows): `enrollment_redeem` **11/11** (default redemption without
approval, byte-identical recovery with the issuer called once, second-device
conflict, require-approval pending then approved, wrong pinned key fails the
handshake with nothing claimed, unrecorded/foreign invites invalid, revocation
before issue and `may_recover` refusal after, local intent refusal before
connecting, cross-session proof replay refused with nothing claimed via a raw
hand-written client, garbage/oversized/malformed input closes only that session,
capacity + deadline, shutdown). All enrollment binaries plus
`bootstrap_dep_boundary` **43/43**; SDK lib filter **55/55**; SDK clippy
lib/all-targets, full-feature rustdoc, rustfmt, `git diff --check` and
`cargo check -p net-cli` clean. **Inverses** (restored byte-identically): ignoring
the transcript signature, removing the session semaphore, reissuing instead of
recovering, skipping `may_recover`, removing the client's local intent check, and
pinning a different key in the client each failed their named witness. The
issuer-equality and recorded-digest checks are defense in depth that the public
API cannot reach (an invite with the same random ID needs the issuer's key), so
no inverse witnesses them.

Not included: the membership-only bundle format (the bundle is opaque here);
device-side durable identity/intent persistence and install; `up --enroll`
/ `invite` / `join` CLI; release-binary inclusion and CI job for the live path;
Unix execution. SDK CI auto-discovers the new binary.

**Membership bundle and durable device join (2026-09-23):**
`sdk/src/enrollment/bundle.rs` and `sdk/src/enrollment/device.rs`, witnesses
`sdk/tests/enrollment_join.rs`.

- `MembershipReceipt` (`NMP1`, signature domain `net-mesh membership receipt
  v1`) binds issuer, subject, invite digest, intent digest, trust domain,
  relations and issue time. It is a record, not authority: no delegation chain,
  permission token, invocation, management, organization, channel or subnet
  right, and no gate consumes it.
- `MembershipBundle` (`NMB1`) = receipt + trust-domain PSK + `MeshContact`
  (socket address, mesh Noise static key, node id for `Mesh::connect_via`).
  Delivered only inside the authenticated enrollment session. `verify_for`
  checks the receipt signature and binds issuer, subject, both digests, trust
  domain and relations to the device's invite and intent, and refuses a PSK
  whose `TrustDomainId` is not the one signed into the invite. The contact is
  not signed; it is authenticated by the session (and by the node's Noise key
  when attaching). `Debug` redacts the PSK.
- `MembershipIssuer` is the standard `BundleIssuer`: it refuses to deliver a
  PSK outside the invite's trust domain (`Unavailable`) and refuses recovery
  after a transport rotation (`RecoveryClosed`) instead of re-delivering a stale
  or silently new secret. Membership revocation hooks belong to V3-4.
- `DeviceJoin` persists the identity seed, signed invite and intent in a new
  protected directory (core `EnrollmentStorage`, `NMDJ` checksummed snapshot)
  **before any network use**; `redeem` recovers the same issuance on retry,
  verifies the bundle, and persists it before reporting `Installed`. `open`
  refuses corrupt or inconsistent state (checksum, invite signature, intent
  binding, bundle re-verification) and never resets it. Installed is credential
  state, not live admission.

Validation (Windows): `enrollment_join` **8/8**, including the end-to-end
journey: operator mesh started, clean device decodes the link, persists,
redeems, restarts (service stopped — no network needed), then attaches to the
running mesh with the delivered PSK via `connect_via` (operator peer count
rises); the same contact with a different PSK is refused. All enrollment
binaries plus `bootstrap_dep_boundary` **51/51**; SDK lib filter **55/55**; SDK
clippy lib/all-targets, full-feature rustdoc, rustfmt and `git diff --check`
clean. **Inverses** (restored byte-identically): dropping the bundle's
trust-domain check, the receipt subject check, the receipt signature check, the
issuer's trust-domain check, the issuer's recovery check, the device snapshot
checksum (witnessed by altering the unsigned contact node id of an installed
snapshot), and the device's own install-time verification (witnessed by a
service that signs a valid receipt but delivers another domain's PSK) each
failed their named witness. The last one initially passed against the
honest-issuer journey alone; the dedicated wrong-domain witness was added to
close that gap.

Not included: CLI `up --enroll` / `invite` / `join`, the local control endpoint,
profile integration, standing-PSK rotation and membership revocation, Unix
execution and CI. Loopback single-process evidence only.

**Join token decision (user, 2026-09-23):** `invite create` returns
`netmesh-join_<base64url of the signed NMM1 invite>`. A briefly adopted
`net-mesh://<host:port>/join/…` URL form was replaced before release because a
URL-shaped string invites browsers, chat apps and people to treat it as a web
link (failed opens, search-engine leaks of a bearer credential, linkified
"click me" text). The token is deliberately not URL-shaped, carries no visible
address, and has a fixed prefix so secret scanners can recognize a leaked
invite. Humans verify issuer fingerprint, enrollment address, trust domain,
expiry and bearer status through `invite inspect` and `join`'s confirmation
prompt, never by reading the token. The earlier `net-join:` form is dropped
(never published). Witnesses: the round-trip test asserts the prefix and that
no `://` or address appears; the malformed table refuses the bare body,
`net-join:`, `net-invite:`, the `net-mesh://` URL form, a case-changed prefix,
suffix characters and oversize input. Because the token carries the
operator's address, `up --enroll` must advertise a reachable public address
(`--public-addr`) and a fixed port.

**Control endpoint decision (user, 2026-09-23):** loopback TCP plus an
owner-only per-run secret file with mutual keyed-BLAKE3 authentication, instead
of the Unix-socket / Windows-named-pipe proposal above. One portable
implementation, testable on every platform; its security rests on the same
owner-only state-directory protection the ledger already relies on.

**`up` / `down` / `node status` (2026-09-23, `86c9e6207`):**
`cli/src/commands/lifecycle.rs`, witnesses `cli/tests/node_lifecycle.rs`.

- **State directory:** `--state-dir`, default `<data dir>/net-mesh/nodes/<profile>`
  (profile names outside `[A-Za-z0-9._-]` must pass `--state-dir`). Its `node/`
  subdirectory is a core `EnrollmentStorage` (owner-only directory, atomic
  durable replacement, exclusive store lock) holding the generated identity
  seed and generated PSK (`NMUP` v1, checksummed); plus `up.lock` and
  `control.json`.
- **Ownership and liveness:** the running `up` holds the store lock and a
  lifetime lock (`up.lock`). Liveness is that lock, never a PID or file
  presence. A second `up` refuses, naming the live incarnation and pid.
- **PSK:** omitted → a CSPRNG PSK committed in the first snapshot before bind
  and reused on restart (witnessed: same trust-domain id across restart);
  `file:<path>` → 32 raw bytes or 64 hex through the core secret-file gate;
  `stdin` → piped only (a terminal is refused: echo cannot be disabled);
  `kms:` and argv literals refused; all-zero refused. Supplied sources are read
  and validated before any state is created. Output shows only the public
  `TrustDomainId`. Identity: `--identity` / profile identity, else the generated
  seed.
- **Control endpoint:** loopback `127.0.0.1:<random>`; `control.json` (written
  atomically into the protected directory) holds port, incarnation, pid and a
  fresh 32-byte secret. Handshake: node nonce → client nonce + keyed tag →
  node tag; session key from both nonces; every message carries a keyed MAC
  with direction and sequence. Messages are not encrypted (no secret crosses
  it yet); bounded to 8 concurrent sessions of 5 s. Operations: `status`,
  `shutdown`.
- **Readiness:** the `ready` row (stream output: ndjson when piped) is emitted
  only after the mesh is built and started, the control endpoint is bound and
  the control file is published. `stopped` follows a drain.
- **`down`:** no-op success when not running (reporting stale metadata);
  otherwise authenticate, require the acknowledged shutdown to name the
  recorded incarnation, then succeed only once the lifetime lock is released
  (`--wait`, default 15 s; timeout exits 7 "running or unknown"). An endpoint
  that fails authentication is exit 6 and nothing is claimed.
- **`node status`:** `stopped` / `stale_metadata` (control file, lock free) /
  `starting` (lock held, no endpoint yet) / `ready` / `draining` / `unknown`
  (endpoint unreachable or unauthenticated).

Validation (Windows): `node_lifecycle` **6/6** subprocess witnesses — start,
status, duplicate refusal, down, restart with same identity and trust domain,
idempotent down; kill → stale metadata → clean restart; file and stdin sources
give the exact trust domain, and a peer with that PSK attaches to the reported
address/key/node id while a wrong PSK is refused; invalid sources (kms:, argv
hex, all-zero, short) exit 2 with no stdout and no state directory; a client
without the secret gets no answer; a port squatter that also holds the lifetime
lock is reported `unknown` and `down` refuses to claim a stop. Full `net-cli`
suite **326/326** (1 pre-existing skip); CLI clippy (bin strict, all-targets
with CI allows), rustfmt and `git diff --check` clean. **Inverses** (restored
byte-identically): node skips the client proof; status relabels stale
metadata as ready; `down` returns before the lock is released; readiness
without `mesh.start()`; `up` without the lifetime lock — each failed its named
witness. Client-side node authentication is two layers (node proof, message
MACs): with a forged well-formed reply, removing either alone still refuses the
squatter, removing both fails the witness.

Not included: `--detach`, `kms:` adapters, `--enroll`, invite/join commands,
public docs (`cli/README.md`, web reference) and Unix execution. The default
bind is `0.0.0.0:0`, so the mesh port changes per restart until an operator
pins `--bind`; enrollment will need a stable one.

**`up --enroll`, `enrollment init`, `invite …` (2026-09-23, `03c47f75b`):**
`cli/src/commands/enrollment.rs` (+ `lifecycle.rs`), witnesses
`cli/tests/enrollment_lifecycle.rs` and a control-channel unit test.

- **Scope assumption:** the operator node is publicly reachable (public IP,
  port forward or VPS); joiners may be behind NAT because every joiner
  connection is outbound. Both-sides-behind-NAT (relay / hole punching) is not
  in this slice and needs its own scope decision.
- **`enrollment init --issuer-identity <PATH> [--ledger DIR]`** creates an empty
  ledger (default `<state-dir>/ledger`) bound to that issuer; never replaces one.
- **`up --enroll`** requires `--public-addr <host:port>`, `--issuer-identity`,
  a fixed `--bind` port and an existing ledger; optional `--domain-name`
  (default: profile name). All inputs are validated before any node state is
  created; the issuer is loaded and the ledger's exclusive lock taken before
  the mesh binds (another issuer's ledger → exit 2). After the mesh starts it
  binds the PSK-free Noise enrollment listener on TCP at the **same port
  number** as the mesh's UDP socket (one number to forward), resolves
  `--public-addr` for the bundle's `MeshContact`, and serves
  `MembershipIssuer` bundles carrying this node's PSK. Readiness and
  `node status` include a non-secret enrollment block (public endpoint, listen
  address, enrollment key, issuer and fingerprint, domain). `down` stops the
  listener with the node.
- **`invite create [--require-approval] [--ttl D] [--for ENTITY] [--out PATH]`**
  is a control operation: the node signs, records in its ledger, and returns the
  `netmesh-join_` token, offer id, expiry, approval mode and bearer flag; a
  bearer warning goes to stderr. `--out` writes the token to a new owner-only
  file (existing path refused) and omits it from stdout.
- **`invite status [OFFER]`, `invite revoke OFFER`, `invite approve|deny OFFER
  --subject ENTITY`** are control operations; approve/deny act only on the
  pending claim whose full subject the operator names. With no running
  enrolling node they exit 6; a node without `--enroll` refuses them.
- **`invite inspect <TOKEN|->`** is offline: signature, issuer and
  fingerprint, endpoint, enrollment key, domain, trust-domain id, relations,
  approval, created/expiry, bearer or intended subject. Never prints the token.
- **Control channel** now encrypts messages (keyed-BLAKE3 keystream, MAC over
  ciphertext, direction and sequence bound) because tokens cross it.

Validation (Windows): `enrollment_lifecycle` **5/5** — each refusal case lacks
exactly one input and asserts its reason (missing public address, ephemeral
port, missing issuer, missing ledger, unusable address) with no node state;
foreign-issuer ledger refused; second init refused; invite commands exit 6
without a node. Journey: init → `up --enroll` → `invite create` → stdin
`invite inspect` (endpoint, fingerprint, 24 h, bearer) → a clean SDK device
redeems the token and **attaches to the running node's mesh with the delivered
PSK** → `invite status` shows issued to that subject → a second device is
refused (conflict) → a revoked token issues nothing. Require-approval: pending,
wrong-subject approve refused, correct approve → install → attach. `--out`
file, no overwrite, ledger offers persist across down/restart. A node without
`--enroll` refuses invite operations. Unit: control frames never contain the
plaintext; wrong sequence fails. Full `net-cli` **332/332**; SDK enrollment
binaries **48/48**; clippy/rustfmt clean. **Inverses** (restored
byte-identically): allowing an ephemeral port, approving any subject, `--out`
overwriting, answering invite operations without `--enroll`, removing the
keystream — each failed its witness. The ephemeral-port inverse first passed
because its case also lacked a ledger; the refusal test was rebuilt so each case
lacks exactly one input, and one-shot commands are bounded at 20 s so a refusal
that regresses into a live node fails fast.

Not witnessed on one host: that the bundle contact uses the public address
rather than the bind address (identical on loopback); multi-host/NAT behavior;
Unix execution. `invite create` opening the ledger directly is structurally
excluded (the node holds the ledger lock) but has no dedicated inverse.

#### Primary use case and reachability requirement (user, 2026-09-23)

**Use case.** Anyone, with minimal configuration, connects a *local* device
(sensor, robotic arm, camera, home appliance) to an AI agent in the cloud:
the device side runs `invite`, the token is pasted into a chat, and the agent
runs `join` through a tool. The **inviter is the device behind a home NAT**;
the **joiner is the cloud agent**. The slices above assumed the reverse (a
publicly reachable inviter), so a device behind NAT without a manual port
forward cannot currently be joined.

**Requirement (user decision):**

1. **Direct first, relay fallback.** The token carries a direct path when one is
   viable and a relay path as fallback; `join` tries direct first and falls
   back automatically.
2. **No manual router configuration** is required for the supported journey.
3. **Relay availability is never a prerequisite** for an otherwise viable
   direct connection: with no relay configured or reachable, a viable direct
   path still enrolls and attaches.
4. **Evidence required:** (a) prove direct operation *without application
   forwarding* — the joiner's enrollment session and mesh session reach the
   device's own sockets through the NAT, with no relay or forwarding node in
   the path (and ideally none in the topology); then (b) force direct-path
   failure and prove automatic relay fallback — the same journey completes
   through the relay without operator action, and the evidence identifies the
   relayed path.

**Direct path (no manual router configuration).** The SDK already provides
opportunistic UPnP-IGD / NAT-PMP / PCP port mapping (`port-mapping` feature:
install, 30-minute renewal, revoke on shutdown) that pins the mesh's reflex
override to the mapped external address. V3 work:

- `up --enroll` requests mappings for the mesh UDP port **and** the enrollment
  TCP port (same number when the router allows) and signs the mapped external
  `host:port` into tokens. `--public-addr` becomes an optional override; a
  concrete routable bind address is also used directly. With no mapping, no
  override and no routable bind, the token carries no direct path.
- The bundle's contact reuses the address that reached enrollment (same port
  number for TCP and UDP), so the node need not know its address for bundles.
- Auto-provisioning for minimal config: accepted (decision 3 below).

**Relay fallback.** Design constraints discovered so far (to verify before
freezing): a Net relay today is an ordinary mesh node that forwards routed
handshakes/data to peers it holds sessions with, i.e. it sits inside the trust
domain and holds its PSK; sessions through it stay end-to-end Noise-encrypted.
Enrollment is PSK-free TCP and cannot ride that mesh path, so the fallback also
needs a relayable enrollment path (the relay splicing the device's outbound
registration to the joiner's inbound enrollment stream, with the NK session
end-to-end), plus the device keeping an outbound registration with the relay.
The token would carry direct and relay locators under the same issuer
signature.

**Decisions (user, 2026-09-23):**

1. **Relay hosting: both, with a configurable default.** A project-run default
   relay makes the fallback zero-config; operators can point at their own relay
   instead (e.g. a VPS or the agent's cloud side). This is an explicit scope
   change from the plan's no-hosted-service non-goal, limited to a blind relay:
   no registry, roster, control plane or account system comes with it.
2. **Relay trust: blind forwarder.** The relay never holds the trust-domain PSK,
   the issuer key or any mesh credential; it only forwards encrypted traffic it
   cannot read, and it cannot join the mesh. Today's in-mesh relaying (a
   PSK-holding node) therefore does not satisfy the fallback; relaying must sit
   below the mesh session layer for both the PSK-free enrollment session and
   the subsequent mesh session. The relay must also not become an oracle or
   amplifier: registrations are authenticated by the device, forwarding is
   bounded, and relay state is not authority.
3. **Minimal config: auto-create on first `up --enroll`.** Missing issuer
   identity, ledger and fixed port are created durably on first run and reported;
   explicit flags still override. This replaces the earlier rule that `--enroll`
   refuses without an issuer identity and ledger store: they now exist because
   they were created, never because a refusal was skipped. Corrupt or foreign
   existing state still refuses rather than being replaced.

**Evidence plan.** Loopback cannot prove either path. Extend `natsim` (Linux
network namespaces with real nftables NAT, CI-only) with a port-mapping gateway
(e.g. `miniupnpd` with NAT-PMP/PCP/UPnP in the device's gateway namespace):
- direct row: device behind that gateway, agent on the WAN, **no relay node in
  the topology**; `up --enroll` maps ports without manual configuration, the
  agent redeems and attaches, and the evidence shows the peer address is the
  gateway's mapped address and the route is direct;
- fallback row: the same topology plus a relay, with mapping made to fail
  (daemon off, or mapping refused); the journey completes through the relay and
  the evidence shows the relayed path; and a row with the relay down but mapping
  working, proving the relay is not a prerequisite.
These cannot run on the Windows development machine; they gate in CI.

**R1a — TCP port mapping in the core (`47f497b46`):** each mapper is bound to
one `MapTransport` (UDP default, so the mesh's own mapping task is unchanged).
NAT-PMP gains `MapTcp` on opcode 2 and accepts only a map answer for its own
transport; UPnP maps with the matching IGD protocol; `SequentialMapper::new_for`
and `sequential_mapper_from_os_for` build either transport. Witnesses: TCP
opcode/codec test and a mock-gateway TCP install that also refuses a
UDP-opcode answer. 64/64 port-mapping unit tests; all-features clippy
(all targets) and rustdoc clean. Inverses: forcing UDP in the TCP mapper,
accepting any transport's answer — both caught.

**R1b — minimal-config `up --enroll` with router mapping (`765d11299`):**

- `up --enroll` needs no other flags. First run creates and persists a fixed
  port (allocated TCP-first, then checked free for UDP — Windows reserves large
  TCP-only ranges that sequential UDP allocation kept hitting), an issuer seed
  (node state format v2 adds issuer + port; v1 still reads) and the default
  ledger, and reports them in `enrollment.created`. `--issuer-identity`,
  `--ledger` (must exist), `--bind` and `--public-addr` override. A foreign or
  corrupt ledger/state still refuses.
- Port mapping (default on, `--no-port-mapping` to disable): the mesh maps its
  UDP port (`try_port_mapping`) and the new SDK `enrollment::portmap::TcpMapping`
  maps the enrollment TCP port (install, 30-minute renewal, abandon + remove
  after three failures, remove on shutdown). `up` waits up to 4 s for both and
  reports `port_mapping` (`active` / `partial` / `unavailable` / `disabled`)
  with the mapped addresses.
- Token address, in order: `invite create --addr`, `--public-addr`, the
  router mapping (only when **both** TCP and UDP were mapped), a concrete bind
  address. With none, `invite create` refuses and names the fix — no guessing.
  Bundles carry the mesh contact matching each token's address (the UDP
  mapping for the mapped TCP address, otherwise the token's host and port).
- **Dependency trade-off (for review):** the CLI now enables SDK
  `port-mapping`; UPnP-IGD is SOAP over HTTP, so the default CLI binary gains
  `igd-next` and a `hyper` HTTP *client* (no server, no TLS). NAT-PMP/PCP alone
  would avoid it but misses routers that only speak UPnP.

Validation (Windows): `enrollment_lifecycle` 6/6 and `node_lifecycle` 6/6
(stable over repeated runs), including the minimal-config journey (auto-created
port/issuer/ledger, device joins and attaches, restart reuses all three,
per-invite `--addr` signed into the token) and the reduced refusal set
(missing explicit ledger, unusable public address, foreign-issuer ledger).
Unit: endpoint selection order, node-state v2 round trip + v1 read + tamper
refusal, SDK TCP keeper (no gateway, install/renew/remove, abandon after
failures). Full `net-cli` 335/335; SDK enrollment units 28/28; CLI and SDK
clippy, SDK rustdoc (`full port-mapping`), rustfmt clean. Inverses: TCP-only
mapping treated as direct, port not persisted, issuer regenerated, `--addr`
ignored, wildcard bind claimed as an address — all caught.

**Not yet proven:** that a real router mapping makes a NATed device joinable
(tests always pass `--no-port-mapping` so a developer's own router is never
touched). That is R1c: a natsim row with a port-mapping gateway (e.g.
`miniupnpd`) and no relay in the topology, CI-only.

**`net-mesh join` and joined `up` (`e876785c1`):** `cli/src/commands/enrollment.rs`
(+ `lifecycle.rs`), witnesses `cli/tests/join_lifecycle.rs`. The joiner is the
cloud agent in the primary use case.

- `join <TOKEN|->` decodes the token and prints the issuer fingerprint,
  enrollment address, domain, trust-domain id, expiry and bearer/bound status to
  stderr, then requires confirmation: `--yes` for scripts and agent tool use;
  non-interactive without `--yes` exits 8 before any effect; a stdin token (`-`)
  always needs `--yes`. It persists identity and intent (`<state>/join`,
  `DeviceJoin`) before redeeming, installs the verified bundle, then proves live
  admission with a real attach. Installed-but-unattached exits 6 and keeps the
  credentials; pending approval is a reported state (exit 0). Re-running the same
  join is idempotent (no second issuance); a state dir holding one join refuses a
  different token.
- `up` in a state dir with an installed join runs as that device: enrolled
  identity and delivered PSK (`psk_source: "joined"`), attaches to the issuer's
  node and reports `joined.attached` (and `detail` on failure). It refuses a
  pending join, `--enroll` (would hand another operator's PSK to others),
  `--psk-from` and `--identity`.

Validation (Windows): `join_lifecycle` 4/4 — CLI-only journey (refusals without
`--yes`, summary names the issuer, joined + attached, idempotent re-join, second
agent refused, joined `up` with the operator's trust domain and attached, joined
node refuses `--enroll`); require-approval via CLI; different-token refusal;
installed vs attached (operator down → re-join exits 6 "credentials are
installed", joined `up` reports `attached: false`). Full `net-cli` 339/339;
clippy/rustfmt clean. Inverses: confirmation skipped, different token reusing a
state dir, attach failure ignored, joined node allowed to `--enroll`, pending join
allowed to start — each failed its witness (the attach inverse was first written
as a panic, which proves nothing, and redone as a genuine ignore).

**Measured behavior to track:** a second attach with the same node identity
while the operator still holds the earlier session (the re-join, or `up` right
after `join`) takes about 5.2 s instead of ~0.1 s. The operator's routed
re-handshake rule defers rotation of a live, busy session (`DeferBusy`,
`mesh.rs` `routed_rotation_outcome`) and the joiner succeeds on its handshake
retry. This is deliberate core safety behavior; a clean close of `join`'s
attach probe would avoid it and is a candidate follow-up.

#### R1c — natsim direct-path row (`14e524808`..`cc750bb85`, CI PASS)

`tests/natsim/setup.sh --nat-a upnp` adds a gateway that only masquerades
outbound and forwards nothing by itself. `tests/natsim/enroll/run_enroll_direct.sh`
starts `miniupnpd` (NAT-PMP / PCP / UPnP, nftables backend) in that gateway,
runs `net-mesh up --enroll` with defaults on the device, and has a public agent
run `join` and `up`, with **no relay or helper process in the topology**. It
requires the mapping to be active and signed into the token, join and joined
`up` to attach, the bundle contact to be the mapped UDP address, conntrack to
show the agent's TCP and UDP flows DNAT'd straight to the device's private
address, a negative control (`--no-port-mapping`, public address forced into the
token) to fail to join, and the mappings to be removed after shutdown. The new
`natsim-enroll` workflow runs only this row.

**Receipt: PASS, `natsim-enroll` run 35800318087** (`[enroll] PASS`) after three
lab-only iterations, none touching product code: miniupnpd 2.3.4 does not know
`ext_allow_private_ipv4` (removed); it refuses an RFC1918 `ext_ip`, so the upnp
gateway gets a public alias `11.99.0.2/32` (`29706542a`); and `join` does not pin
a source address, so the conntrack evidence accepts any `10.99.0.x` WAN source
while still requiring the reply source to be the device's private address
(`cc750bb85`).

#### R2 — blind relay fallback: design (user-approved 2026-09-23)

**Choice: a native blind UDP relay in the core transport**, not a TCP
splice/tunnel. Rationale for the primary use case (devices such as robot arms,
sensors and cameras talking to a cloud agent):

1. The traffic is latency-sensitive and loss-tolerant. Net's lossy streams and
   its own reliable-stream recovery assume UDP; tunnelling over TCP turns lossy
   into "reliable but late", adds head-of-line blocking, and stacks TCP
   retransmission under Net's own — worst exactly on the lossy CGNAT/mobile
   paths where the relay is used.
2. A relayed session that is a real mesh session over UDP can later move to a
   direct path through the existing hole-punch / direct-path upgrade machinery;
   a TCP tunnel is invisible to it and stays relayed.
3. A shared default relay must be cheap per packet: forwarding opaque datagrams
   by a small channel header scales better than per-pair TCP streams.
4. It stays blind: the relay forwards NKpsk0 ciphertext by channel number, never
   holds the PSK, issuer key or any mesh credential, and cannot join the mesh.

**Shape.**

- **Device registration:** the device keeps an authenticated UDP registration
  with the relay (registration id derived from the device's relay key, so only
  that key can claim it); its keepalives also hold the device's NAT mapping
  open, so every joiner reaches the device through that one relay port and
  mapping without punching.
- **Relayed peer transport (core):** a new relayed peer-address kind; datagrams
  to and from a relayed peer carry a small channel header, and the relay maps
  channels to endpoints. Mesh handshakes and data are unchanged end-to-end.
- **Enrollment through the relay:** enrollment stays PSK-free Noise NK over a
  byte stream; the relay offers a small blind TCP splice between the joiner and
  a fresh outbound stream from the device. No reliability layer is built for
  enrollment over UDP.
- **Tokens and bundles** carry the direct address (if any) plus a relay locator
  (relay address and registration id). `join` and joined `up` try direct first
  and fall back automatically; relay availability never blocks a viable direct
  path.
- **Relay hosting:** `net-mesh relay serve` for user-run relays; a project-run
  default endpoint is configuration (the address is not invented before a
  relay is actually deployed). Relays bound registrations, channels, splices and
  per-registration rates, time out idle state, and never amplify.
- **Not covered:** networks that block UDP entirely. A TCP/443 tunnel mode can be
  added later as a last-resort path alongside this design.

**Phases, each proven before the next:**

1. Relay server (`net-mesh relay serve`) and the core relayed-peer transport,
   with unit and loopback witnesses.
2. Enrollment splice through the relay.
3. Token and bundle relay locators; direct-first, relay-fallback in `join` and
   joined `up`.
4. natsim rows: direct forced to fail (mapping off / CGNAT-style gateway) with a
   relay present → joins relayed, evidence identifying the relayed path; relay
   down with mapping working → joins directly.
5. Later: relayed-to-direct upgrade using the existing hole-punch machinery.

The core transport is hardened code with a large test surface; each phase runs
its focused witnesses plus the full relevant CI families, with inverse
mutations for every authority or path claim.

#### R2 phase 1 — relay server and relayed-peer transport (receipt)

- **Relay core** (`traversal/blind_relay.rs`, `7846ab57c`): `RelayCore` and the
  UDP `BlindRelay`. Registration is HELLO → stateless CHALLENGE (keyed MAC over
  the observed endpoint and time, so the relay holds nothing for unanswered
  HELLOs) → REGISTER signed by the device's mesh identity key over the nonce and
  the observed endpoint; the registration id is derived from the EntityId, so
  only that key can claim it. Joiners BIND → BOUND a channel; DATA is
  `[0x10][channel u32 BE][payload]`, forwarded opaque. Every reply is no larger
  than its request (a compile-time assertion), and registrations, channels per
  registration, bind rate and idle state are bounded.
- **Wire:** `PeerAddr::Relayed { relay, channel }`, the DATA header helpers and
  the `relay:{addr}#{channel}` display.
- **Core transport:** `PeerSink` frames datagrams to a relayed peer in
  `send`/`try_send`/`send_bounded`. The UDP receive loop's `relay_ingress`
  unwraps DATA from a relay the node uses and attributes it to the relayed
  endpoint, and hands anything else to that relay's client. `MeshNode` gains
  `relay_register` (refreshes every ttl/3, min 5 s, which also keeps the
  device's NAT mapping open, and stops when the handle drops), `relay_bind` and
  `connect_via_endpoint`; `connect_via` now delegates to it. All of these sit
  behind `nat-traversal`.
- **Explicit limit:** the router's own socket cannot send to a relayed endpoint.
  `send_to` returns `Unsupported` and the scheduler counts it as dropped (the
  `dropped` counter is no longer webrtc-gated), so no silent fallback arm
  swallows it. Scheduled router streams over a relayed path are not supported in
  phase 1; direct session traffic is.
- **CLI:** `net-mesh relay serve --bind ADDR [--max-registrations N]
  [--max-channels-per-registration N]` runs in the foreground, emitting `ready`
  and `stopped` rows (the second with counters).

Witnesses:
- `blind_relay` units 10/10, including
  `a_mesh_session_runs_end_to_end_through_the_blind_relay` (a real NKpsk0
  handshake plus a channel subscribe/ack through the relay on loopback) and
  `binding_an_unregistered_device_is_refused`.
- `cli/tests/relay_serve.rs` 2/2: a mesh session through a real `relay serve`
  subprocess, and refusal of a non-literal bind.

Inverse mutations, each caught by its witness and then restored:
- skipping the REGISTER signature check;
- skipping the challenge-freshness check;
- forwarding from any endpoint;
- the channel cap;
- the bind rate limit;
- not unwrapping relayed ingress;
- not framing relayed egress.

Regressions:
- `cargo tl` 5804/5804.
- Transport integration families 135/135.
- `net-cli` suite 341/341.
- Clippy (all-features, all-targets, lib/bins default and no-default) and
  rustdoc (root and `net-mesh-wire`) are clean.

#### R2 phase 2 — enrollment splice through the relay (receipt)

**Protocol** (`blind_relay.rs`). The relay's TCP listener shares the UDP
socket's port number:
1. A joiner sends `[0x20 JOIN][registration id]`.
2. The relay allocates a random 128-bit splice id and sends UDP
   `OFFER { splice id }` (17 bytes) to the device's **registered** endpoint. It
   resends every 1 s, re-reading the endpoint so a NAT rebinding is followed,
   until `splice_accept_wait` (5 s) passes.
3. The device dials back with `[0x21 ACCEPT][splice id]`.
4. The relay writes one status byte to each stream and copies bytes blindly.

**Bounds:**
- per-registration splice rate: `splices_per_window` = 4 per `bind_window`;
- pending splices: 1 024; live splices: 4 096; TCP connections: 8 192;
- preamble deadline: 5 s;
- per-direction byte cap: 1 MiB;
- splice lifetime: 60 s;
- stale pending splices are swept.

An offer is only ever sent to a registered device, after a completed TCP
handshake. An unanswered splice is refused `Unreachable`, and an `ACCEPT` for
no pending id is refused.

**Relay socket order.** `BlindRelay::bind` with port 0 picks the TCP port
first, then binds UDP on it. Windows excludes TCP-only port ranges that sit
inside the UDP ephemeral range, and allocates UDP ports roughly in sequence, so
a UDP-first choice failed every retry with WSAEACCES. This is the same lesson as
the enrollment port.

**Device side.** `RelayRegistration::accept_splices(capacity)` routes relay
`OFFER`s (which `RelayClient::deliver` now separates from control replies) to a
dial-back task. That task ignores resends it has seen, runs at most 8
concurrent dial-backs, and yields spliced `TcpStream`s. Offers are dropped while
the node is not accepting.

**SDK.**
- `EnrollmentService::serve_stream` runs a stream obtained elsewhere under the
  same session permit and deadline as accepted connections.
- `redeem::redeem_over(stream, …)` runs the unchanged Noise NK redemption over
  any byte stream. The responder must still prove the invite-pinned enrollment
  key, so neither the relay nor anyone who claims a splice can answer for the
  device.

Witnesses:
- `blind_relay` units 15/15:
  - `splices_are_bounded_rate_limited_and_claimed_once`
  - `a_byte_stream_is_spliced_to_the_registered_device`
  - `a_splice_the_device_never_accepts_is_refused_as_unreachable`
  - `an_accept_for_no_pending_splice_is_refused` (a forged `ACCEPT` while a real
    joiner waits is refused, and the joiner is never spliced to it)
  - `a_splice_is_cut_at_its_byte_cap`
  - the `OFFER`/`Unreachable` codec cases
- `sdk/tests/enrollment_relay.rs` 2/2:
  - `an_invite_is_redeemed_over_a_blind_relay_splice`: the invite's direct
    endpoint is unroutable, and the bundle arrives through the splice;
  - `a_splice_answered_by_another_responder_fails_the_handshake`: the genuine
    invite stays `Offered`.

Inverse mutations, each caught by its witness and then restored
byte-identically:
- an accept that takes any waiting joiner;
- no per-registration splice rate;
- no byte cap;
- the device dropping offers;
- an unanswered splice left pending.

Regressions:
- `cargo tl` 5809/5809.
- SDK enrollment binaries 50/50.
- `net-cli` 341/341.
- Clippy (core all-features all-targets, lib/bins all and no-default; SDK
  `full` all-targets; `net-cli`) and rustdoc (root, SDK `full`) are clean.

#### R2 phase 3 — relay locators and direct-first, relay-fallback (receipt)

**Formats** (unreleased, changed in place). The signed invite's direct
endpoint is now optional, and the invite gains an optional signed
`RelayLocator { endpoint host:port, registration id }`. A token must name at
least one of the two. The bundle's `MeshContact` likewise has an optional
`addr` and an optional `relay`, and is refused when it has neither.
`invite inspect`, `invite create` and `join` show both.

**Redemption** (`redeem_with_path`, `DeviceJoin::last_path`):
- The direct endpoint is always tried first.
- Only a failure to *connect* falls back to the relay splice: refused,
  unreachable, or no accept within `DIRECT_CONNECT_WAIT` (4 s) when a relay is
  named. Without a relay, the direct path gets the whole budget.
- A service answer, including a refusal, is never rerouted.
- A dead relay never delays a reachable direct endpoint.

**Mesh attach** (`attach_contact`, used by `join` and joined `up`). Direct is
tried first, bounded by `DIRECT_ATTACH_WAIT` (5 s) only when a relay is named.
After that the relay is used: `relay_bind`, then `connect_via_endpoint`. The
path taken is reported: `join` reports `enroll_path` and `attach_path`, and
joined `up` reports `joined.path`. The joined-`up` attach wait rose to 20 s,
leaving the relay room for the operator's ~5 s `DeferBusy` on a re-attaching
identity (measured in R1b).

**Device side.**
- `up --enroll --relay HOST:PORT` (or the profile `relay`; `--no-relay`
  overrides both; `DEFAULT_RELAY` is `None` until a relay is deployed) starts a
  `RelayLink`.
- The link registers from the mesh socket; the first attempt is bounded at 4 s
  before readiness, then retries every 15 s in the background. The core refresh
  re-registers after a relay restart.
- Spliced streams go through `SessionSink` into the same enrollment service.
- Readiness reports `relay`, `relay_state` (`registered`/`unavailable`) and
  `relay_error`.
- Tokens carry the locator whenever a relay is configured. The registration id
  is derived from the node's entity id, so it is known even before the first
  registration succeeds.

**Core fix found by the witness.** A routed handshake cancelled by its caller
(the direct attempt's timeout) left its `pending_handshakes` entry behind, so
the relay attempt to the same peer failed "handshake already in flight". The
new `PendingInitiator` guard closes the attempt's receiver on drop and removes
the entry only if it is still that attempt's; a newer attempt's entry is never
touched.

**Port selection on Windows.** Hyper-V/WSL reserve both TCP-only and UDP-only
blocks inside the ephemeral range (here 63787–65141 for UDP), and Windows
allocates ephemeral ports roughly sequentially, so neither TCP-first nor
UDP-first retries escape a block. `BlindRelay::bind` and the CLI's enrollment
`free_port` now try the OS choice once, then random ports from 49152–65535
(up to 64 attempts) until both protocols bind.

Witnesses:
- SDK `enrollment_relay` 6/6:
  - `a_reachable_direct_endpoint_is_used_before_the_relay` (0 splices);
  - `an_unreachable_direct_endpoint_falls_back_to_the_relay` (plus a
    relay-only token);
  - `a_dead_relay_does_not_block_a_reachable_direct_endpoint` (< 2 s);
  - `failures_are_not_rerouted_without_cause` (no relay → `Io`; refusal →
    `Conflict`, 0 splices);
  - the two phase-2 witnesses.
- `enrollment_invite`: `a_relay_locator_is_signed_and_the_direct_endpoint_is_optional`
  (redirecting the registration id fails `BadSignature`; neither path is
  refused).
- `enrollment_join`: `a_bundle_contact_carries_its_relay_and_may_omit_the_direct_address`.
- Core: `a_cancelled_routed_handshake_does_not_block_the_next_attempt`.
- CLI `relay_join` 4/4, all real subprocesses including `relay serve`:
  - `a_dead_direct_path_falls_back_to_the_relay` (`enroll_path` = `relay`,
    `attach_path` = `relay`, joined `up` `path` = `relay`);
  - `a_live_direct_path_is_used_while_a_relay_is_present` (both `direct`);
  - `a_dead_relay_does_not_block_the_direct_path` (`relay_state` =
    `unavailable`, token still carries it, both paths `direct`);
  - `relay_flags_are_validated_before_any_effect`.

Inverse mutations, each caught by its witness and then restored
byte-identically:
- the guard not removing the entry;
- redeem going relay-first;
- redeem never falling back;
- CLI attach going relay-first.

Regressions:
- `cargo tl` 5810/5810.
- The `connect_via` integration binaries (channel_identity_readiness,
  connect_direct, coordinator_selection, direct_upgrade, route_withdraw,
  routed_transport_availability, sensing_failure_plane,
  three_node_integration) 116/116.
- SDK enrollment 56/56.
- `net-cli` 345/345.
- Clippy (core all-features all-targets, lib/bins all, default and
  no-default; SDK `full`; `net-cli` all-targets) and rustdoc (root, SDK `full`,
  `net-cli`) are clean.

#### R2 phase 4 — natsim fallback rows (receipt)

**Receipt: PASS, `natsim-enroll` run 35808945349.** Both jobs pass. The `direct`
job (R1c) is unchanged. The new `relay` job runs
`tests/natsim/enroll/run_enroll_relay.sh`.

**Topology.** The upnp home-router lab, where the gateway forwards nothing by
itself; `relay serve` on the WAN at 10.99.0.10:3478; the agent at 10.99.0.12.
Each row uses fresh device and agent state.

| Row | Setup | Result and evidence |
|---|---|---|
| B | Relay down, router mapping working | The device starts with `relay_state` = `unavailable` and its token still names the relay. The agent joins with `enroll_path` = `direct` and `attach_path` = `direct`. Relay availability is not a prerequisite. |
| C | Relay up, mapping working | Still `direct` for both, and the relay's `stopped` row shows `splices` = 0. Direct first. |
| A | Relay up, direct forced dead (`--no-port-mapping`; the token names the router's public address, which forwards nothing) | `join` shows `enroll_path` = `relay` and `attach_path` = `relay`, and joined `up` shows `joined.path` = `relay`, with no flag on the agent's side. |

Row A's evidence:
- The relay's counters show `splices ≥ 1`, forwarded packets and an accepted
  registration.
- The gateway's conntrack (flushed first, since rows B and C leave DNAT'd direct
  flows behind) shows:
  - the device's outbound UDP from its mesh socket (7001 → relay 3478), which
    is the registration;
  - the device's outbound TCP to the relay, which is the splice dial-back;
  - the agent's direct UDP attempt to 11.99.0.2:7001, `[UNREPLIED]`, which
    proves direct was tried first;
  - no flow reaching the device directly.

Three iterations, none touching product code:
1. SIGTERM went to the forked subshell of a backgrounded shell function rather
   than the relay (now launched through `ip netns exec` directly).
2. Stale conntrack entries from rows B and C tripped row A's "no direct flow"
   check.
3. The unreplied-direct-attempt assertion was added.

`relay serve` now also stops cleanly on SIGTERM and reports
`registrations_accepted`, `splices` and `splice_bytes` in its `stopped` row.

This completes the user's reachability requirement in the lab:
- direct operation without application forwarding (R1c);
- forced direct failure with automatic relay fallback (row A);
- no dependency on the relay for a viable direct path (rows B and C).

**Next:** R2 phase 5 (later), a relayed-to-direct upgrade using the existing
hole-punch machinery (done in V3 S7). Also still open: a deployed project relay to fill
`DEFAULT_RELAY`; a TCP/443 last-resort tunnel for UDP-blocked networks; and
the earlier open items (lifecycle fencing, selective subnet semantics, V2
exact-head acceptance).

#### R2 status summary (2026-09-23)

**Outcome.** Relay fallback works end to end. A device the agent cannot reach
directly enrolls and attaches through a blind relay automatically, with no flag
on the agent's side. When the direct path is viable, it is used first; the relay
is never a prerequisite. The natsim lab confirms this (`natsim-enroll` run
35808945349, both jobs pass). This meets the reachability requirement recorded
under "Primary use case and reachability requirement".

**What each phase added.**

| Phase | Commit | Added |
|---|---|---|
| 1 | `898094987` | The core carries mesh sessions through a relay (`PeerAddr::Relayed`), and `net-mesh relay serve` runs one. The relay forwards ciphertext only: it never holds the PSK or any mesh credential and cannot join the mesh. |
| 2 | `08c151d88` | Enrollment works through the relay (TCP splice). The joiner's stream is handed to the device, and the Noise NK handshake still proves the invite-pinned enrollment key, so neither the relay nor anyone who claims a splice can answer for the device. |
| 3 | `e7cb22203` | See below. |
| 4 | `5b515fc0e`..`b1d1c52fa` | The natsim rows below. |

Phase 3 added:
- Tokens and bundles carry an optional signed relay locator alongside an
  optional direct address; at least one is required.
- `join` and joined `up` try direct first and use the relay only if direct
  cannot be reached. A refusal from the device is never rerouted.
- `up --enroll --relay HOST:PORT` (or the profile `relay` key; `--no-relay`
  turns it off) registers in the background, so a relay that is down never
  blocks start-up.
- `join` reports `enroll_path` and `attach_path`; joined `up` reports
  `joined.path`.

**natsim rows** (run 35808945349):

| Setup | Result |
|---|---|
| Relay down, router mapping working | Joins directly |
| Relay and mapping both up | Still joins directly; `splices` = 0 |
| Direct path forced dead | `join`, the attach and joined `up` all go through the relay |

In the forced-dead row, the gateway's conntrack shows the agent's direct attempt
unanswered, only the device's outbound flows to the relay, and nothing reaching
the device directly. The relay's own counters confirm the relayed session.

**Defects the witnesses found (fixed).**
- **Core handshake:** a routed handshake cut off by its caller's timeout left
  its pending entry behind, so the relay attempt to the same peer failed
  "handshake already in flight". The `PendingInitiator` guard now removes only
  that attempt's own entry.
- **Windows port selection:** Windows reserves TCP-only and UDP-only port
  blocks and hands out ephemeral ports roughly in sequence, so retrying the
  OS's pick kept failing. The relay bind and the enrollment `free_port` now fall
  back to random dynamic-range ports until one binds for both protocols.
- **Joined-`up` attach wait:** raised from 10 s to 20 s. After a dead direct
  attempt (5 s), the device still defers a re-attaching identity for about 5 s.
- **`relay serve`:** now stops cleanly on SIGTERM and reports its splice
  counters.

The three natsim reruns fixed scenario-script problems only (a signal sent to a
forked subshell, and stale conntrack entries from earlier rows). None touched
product code.

**Verification.** Every change has witnesses, and each protection was checked
by breaking it in place, confirming its witness fails, and restoring it
byte-identically.

Last local results:
- lib 5810/5810;
- SDK enrollment 56/56;
- `net-cli` 345/345;
- the `connect_via` integration binaries 116/116;
- clippy and rustdoc clean.

The full CI suite passed on the pushed R2 heads: run 35807029554 at
`5b515fc0e` (all R2 product code) and run 35808941788 at `c0cb21b6a` (the
natsim-fixed head). Both are success.

**Still open.**
- R2 phase 5, upgrading a relayed session to direct (planned for later).
- `DEFAULT_RELAY` stays empty until a relay is actually deployed.
- A TCP/443 last-resort tunnel for networks that block UDP entirely.
- The earlier V3 open items: lifecycle fencing, selective subnet semantics,
  and V2 exact-head acceptance.
Lifecycle fencing, selective subnet semantics and V2 exact-head acceptance
remain open.

Tasks:
1. Pin accepted V2 HEAD and verify its real completion evidence; map the final CLI contract into V3 commands.
2. Select one real operator-service ownership/control path and prove no per-command fresh-store split. Document root-key custody and supported local/remote management boundary.
3. Pin a clean-device bootstrap path with no manual PSK exchange, including listener owner, transport features, trust establishment and secret delivery. Produce a minimal source-backed flow, not an unattended permissive demo.
4. Specify membership-only enrollment outcome and its consumer path, preserving the existing agent delegation APIs unchanged. Pin only the new typed fields/version boundaries actually necessary.
5. Specify the new channel-issuance owner, subject-bound chain issuance, deterministic canonical name/hash, controlled-publisher root inspection for subscribe, intended publisher metadata versus observed routing-ID ACK, local publish-gate semantics, SDK full-chain subscription extension, protected credential storage and exact-incarnation publish removal/cache fencing. Preserve/disclose current cross-publisher token portability; do not imply reciprocal publisher authentication or a remote publish acceptor.
6. Specify the minimal selective subnet-removal mechanism and verification set; identify exact SDK/core/wire owners and cross-language compatibility work if wire changes are required.
7. Map each required outcome to an existing mechanism, missing hook and witness. Record final CLI syntax/defaults and state transitions in this document.
8. Pin the local lifecycle owner for voluntary mesh/org/channel/subnet leave, its durable intent/fencing and dependent-service stop policy. Identify which consumers acknowledge live stop and which can only honor next-start disablement; do not promise control over arbitrary SDK processes.
9. Pin the generic managed-node owner used by `up`/`down`: identity persistence, profile/incarnation lock, protected control transport, foreground/detached process ownership, readiness handshake, shutdown/drain behavior, first-start PSK generation/persistence and supplied-source resolver boundary. Select the initially shipped KMS adapters/features or explicitly mark them feature-conditional; a placeholder `kms:` parser is not a working source.

**Exit:** No unresolved authority/bootstrap/state-owner decision may be passed to a command wrapper. If a mechanism needs a wider protocol redesign than this bounded lifecycle, stop that slice for an explicit scope decision; do not declare V3 complete or reopen serverless/remote Deck. This is a bounded design gate for named blockers, not a new platform-foundation project.

### V3-1 — managed node, protected PSK sources, opt-in enrollment and mesh join links

Merged from the former V3-1 (operator service and join links) and V3-1A (managed
node up/down). One process, `up`, owns the node, its local control endpoint and,
with `--enroll`, the ledger and enrollment listener; there is no second daemon.

**Modify:** CLI `main.rs`, `context.rs`, `target.rs`, `config.rs`, `secret.rs`;
`sdk/src/enrollment.rs` and its `enrollment/*` modules; the selected SDK/runtime
lifecycle owner and platform-local control implementation.
**Proposed new files:** `cli/src/commands/lifecycle.rs`,
`cli/src/commands/enrollment.rs`, `cli/tests/node_lifecycle.rs`,
`cli/tests/enrollment_lifecycle.rs`. SDK owners already landed or in progress:
`sdk/src/enrollment/{policy,store,invite,redeem,service,bundle,device}.rs`.

Tasks:
1. Write RED subprocess witnesses showing the current CLI cannot start a persistent node, report its verified readiness, or stop that exact instance. Cover duplicate `up`, stale metadata/PID reuse, wrong profile/incarnation, startup failure after spawn, shutdown timeout and a second unrelated node. Add RED for clean-device bootstrap, preview-without-redemption, no implicit INVOKE/DELEGATE, and invite create/redeem operating on the same ledger the running node owns.
2. Implement `net-mesh up [--psk-from <SOURCE>]` with foreground default and explicit `--detach`. Persist or load the selected node identity under existing protected-file rules; do not silently create a new identity on every restart. With no source and no existing managed PSK, generate one CSPRNG PSK and commit it before bind; concurrent first starts must converge on one durable value. Return success only after the production node is live and the protected control owner acknowledges the exact incarnation.
3. Implement bounded `file:` and `stdin` sources first, with permission/type/length checks, secret-free diagnostics and in-memory scrubbing. Implement only explicitly selected `kms:` provider adapters behind named features; use workload identity and reject unsupported/malformed references before binding. Prove generated-secret restart reuse and missing/corrupt/insecure generated-store refusal rather than silent replacement. No shell-command resolver, argv literal, environment-value shortcut or secret-bearing temporary file.
4. Implement `net-mesh down` and `net-mesh node status` through authenticated owner-only local control. Prove graceful drain/stop, advertisement withdrawal where owned, exact-instance termination and honest partial results. Preserve identity, stores and source configuration; do not claim revocation or PSK rotation.
5. Add `up --enroll`: refuse before binding without a valid issuer identity and ledger store (missing, corrupt, foreign issuer, already locked). On success the node holds the ledger lock and runs `EnrollmentService` with `MembershipIssuer` delivering its own trust-domain PSK and mesh contact; readiness includes the enrollment listener. Expose `invite create` (sign, record, then return the link; `--require-approval`, `--ttl`, optional intended subject), `invite revoke`, `invite status` and `enrollment status` only through the node's local control endpoint, and optional `enrollment stop`/`start` as control operations. With no enrolling node running these commands refuse rather than touching the ledger. Show bearer-versus-intended-subject policy without printing the link except to its requested protected destination.
6. Add `net-mesh join <link>` (protected file/stdin input to avoid shell history) over `DeviceJoin`: persist identity and intent before redeeming, verify and install the bundle, then attach with the delivered PSK and contact and report credential install and observed live admission separately. Persist enrollment/profile references and consume them through one existing V2 hosted/client path after CLI exit and restart.
7. Prove default redemption succeeds without a second approval and optional approval cannot be bypassed. Cover concurrent same/different identity redemption, lost response after commit, operator/device crash points, revoke during claim/approval, corruption and saturation. A second redemption is not an implicit grant reissue.
8. Cross foreground/detached mode with Unix and Windows process/control semantics, parent crash, child crash, terminal closure, Ctrl-C/service-manager stop, restart and concurrent status/down/invite. Ensure a failed detached readiness handshake cannot leave an unreported live child, and that `down` closes the enrollment listener with the node.
9. In a disposable review worktree, mutate readiness to answer before `MeshNode` runtime start, mutate `down` to trust PID metadata, and let `invite create` open the ledger directly; the named witnesses must fail. Restore the candidate and run existing remote-attach, temporary-supervisor, wrap and MCP-service compatibility controls.

**Exit:** A fresh profile can start one production node with no supplied PSK
(generating and protecting a stable value), from protected file, from stdin and
from every advertised feature-enabled KMS source; prove live readiness; be used
by an existing capability/provider path; stop through a second CLI process; and
prove that exact runtime is no longer reachable. Restart without a source reuses
the generated trust domain. With `--enroll`, two clean participants enroll
through a link without manual PSK handling or a second approval in default
mode, and attach to the running node with the delivered PSK; restart and retry
recover the same identity/result. `up --enroll` without issuer identity or
ledger store refuses before binding; invite commands without a running enrolling
node refuse. Invalid/mismatched requesters cannot redeem; optional approval
cannot be bypassed. Even a valid enrolled device receives no implicit
application authority. Secrets never appear in argv, environment, output,
metadata or test logs. Duplicate/stale/wrong-instance operations fail without
affecting another node. Sources not shipped are not advertised. No
organization/channel/subnet success is claimed yet.

### V3-2 — organization and subnet-scoped enrollment

**Modify:** `sdk/src/org.rs`, relevant SDK mesh/subnet interfaces selected at V3-0, `cli/src/commands/org.rs`, `node.rs`, `subnet.rs`, shared enrollment module.
**Proposed new tests:** `cli/tests/org_join.rs`, `cli/tests/subnet_join.rs`, SDK/core witnesses at the owning modules.

Tasks:
1. Write RED tests for wrong org/subnet authority, wrong subject, altered bundle scope, missing issuer permission, conflicting current owner, and unauthorized automatic dispatch/routing/export.
2. Compose org membership and separately issued endpoint attachment through the shared redemption flow. Preserve full `SubnetRef`, epoch, grant lifetime and issuer lineage; one link may request multiple independent relations, not merge their authorities.
3. Support standalone subnet enrollment with the existing device identity and connection. A connectivity-only invitation does not implicitly join an org.
4. Install and prove the real protected-session admission; add authenticated status from its actual owner if the public SDK cannot currently observe it.
5. Exercise partial issuance/install, restart, expiry/renewal and same-identity resume. Verify a standard consumer after restart can use the enrolled profile and requires separately supplied invocation grants.

**Exit:** All requested enrollment shapes complete through CLI processes, with joined status only for stages actually verified. Wrong authority and denied policy fail before handler effects; direct and delegated subnet issuers are tested where offered.

#### V3-2 subnet half: decisions (user, 2026-09-23)

- **Order.** Subnet join, with `up` as a subnet verifier, comes first. It closes
  the V3-4 CLI removal journey end to end.
- **Admission on the wire.** A session **subprotocol**, reusing
  `SubnetAuthPresentation`. Today nothing drives admission over the wire:
  `issue_subnet_challenge` / `admit_subnet_session` are local APIs only.
  - The device asks; the verifier answers with a session-bound challenge.
  - The device replies with its presentation and credential set; the verifier
    replies with a verdict.
  - Admission therefore rides the session itself, with no application call.
- **Key custody.** A **delegated issuer**. The subnet root stays offline; `up`
  holds only an issuer key under a root-signed `SubnetIssuerGrant` bounded by
  scope, rights and lifetime.
  - Consequence, per V3-4: delegated leaves never re-admit a removed subject,
    so re-admission after a removal requires a root-direct grant.

**Slices:**
1. **S1:** the admission subprotocol in core, with witnesses.
2. **S2:** the subnet relation in invites and bundles, and delegated leaf
   issuance.
3. **S3:** CLI wiring: `up` as verifier and issuer, `invite create --subnet`,
   `join`, and joined `up` presenting its credentials. The e2e journey runs
   join → admitted → `subnet remove` → refused on reconnect.

#### V3-2 org half: decisions (user, 2026-09-24)

**Survey facts.**
- An `OrgMembershipCert` (156 bytes: org, member, window, generation,
  nonce) is signed only by the offline org root. Org v1 is deliberately
  one-hop and root-signed, with no delegated issuance.
- A cert proves belonging only. Protected calls also need a separate
  dispatcher grant, which join must never emit (E4).
- Membership is adopted into a `NodeAuthority` directory (one owner org per
  node), but `net-mesh up` loads no authority directory today.
- Revocation is per-(org, member) generation floors, root-signed and merged
  monotonically. They are applied only at adoption (`node adopt --floors`);
  no path reaches a running node.
- `Relation` has only `Mesh` and `Subnet`.

**Decisions.**
- **Signing at approval.** Org links are always approval-gated.
  - The device redeems and waits.
  - `org approve <offer> --root-key <org key>` signs a membership cert for
    exactly that claimant in the CLI process, then hands the cert to the
    node over the control channel. The root never touches a node.
  - The node delivers the cert on the device's next ask; the device-side
    supervisor re-asks by itself.
  - The admission engine, the cert wire format and the bindings are
    unchanged.
  - Cost: one operator action per device. A delegated org issuer stays
    possible later as its own core-and-bindings slice.
- **Order.**
  - O1 is join: an `Org` relation composed with mesh at first join, plus a
    standalone org link over the session; per-node adoption
    (`<state>/authority`); `up` loading it; proven by an org-protected
    call.
  - O2 is `org remove`: a root-signed floor applied to running enforcement
    points with per-node reporting, the same shape as `subnet remove`.

#### V3-2 org O1 — organization join (receipt, 2026-09-24)

A device joins an organization through enrollment. Only the offline org root
signs membership, at approval, for exactly the claiming device.

**SDK.**
- **O1a (`06d287b01`).**
  - `Relation::Org` (tag 3) with a signed `OrgOffer { org }`; relation and
    offer must agree.
  - An org invite must be `RequireApproval` (refused otherwise, at sign and
    at decode).
  - The bundle carries the `OrgMembershipCert`.
  - `MembershipIssuer::with_org_certs` delivers only a certificate that is a
    valid membership of the offered org, for the claimant, taken from an
    `OrgCertStash` keyed by the exact claim.
  - The device's `verify_for` accepts only that, and refuses a membership on
    an invite that offered none.
- **O1b (`341e48e9d`).**
  - Standalone redemption is generalized to org-only links:
    `answer_standalone_redeem` / `serve_standalone_redeem` on
    `net.enroll.standalone.redeem`.
  - An org-only link is pending until approved; then the device proven on
    the session gets the approved certificate (again on a repeat, never
    re-signed).
- **O1c fix.** The device-side redeem now proves identity to the issuer
  first. A mesh-only session had not proven its entity yet: identity proof
  runs on demand, and subnet admission had been triggering it. The server
  now answers `Unavailable` when no identity is proven, and `Conflict` only
  when a different one is.

**CLI (O1c).**
- **Operator.**
  - `invite create --org <ORG>` composes the org relation with the mesh
    (always approval-gated); `org invite <ORG>` creates a standalone link.
  - Plain `invite approve` refuses an org offer.
  - `org approve <offer> --subject <device> --org-key <root>`:
    - fetches the pending claim and its offered org from the node
      (`invite_org_pending`);
    - checks the named subject, and refuses a key for another org;
    - signs the cert **on the operator's machine**, and hands it to the node
      (`invite_org_approve`), which re-checks org and member, keeps it
      durably per claim (`<ledger>.org/`), then approves.
- **Device.**
  - `join` adopts a delivered membership into `<state>/authority` (the
    `NodeAuthority` ceremony: validated, one owner org).
  - `up` installs it before serving, and reports `org` in ready and in
    `node status`.
  - `org join <link>` redeems over the session. When pending, the link is
    kept (`<state>/orgs-pending/`, surviving restart) and the link
    supervisor asks again every 30 s. Once issued, the membership is adopted
    and installed live.

**Witnesses.**
- SDK `enrollment_org` (5 tests):
  - the invite rules;
  - delivery of only the approved certificate (foreign org and another
    device refused);
  - the device-side check;
  - standalone org redemption over the session.
  - **The decisive one:**
    `an_enrollment_delivered_membership_is_admitted_for_an_org_protected_call`.
    A certificate delivered by enrollment, adopted and installed from its
    directory, is admitted by a same-org provider for a real `serve_org`
    call. The handler sees the device's entity and org.
- CLI `org_join` (2 tests):
  - composed join: pending; plain approve refused; wrong org key and wrong
    subject refused; approve; join adopts; `up` and `node status` report the
    org;
  - standalone: a mesh invite refused; pending; the pending link survives a
    device restart; approve; the running node installs it by itself; a
    restart re-installs it.

**Inverse mutations** (all RED):
- SDK (10): org without approval; delivery unfiltered; stash accepting
  another device; the device skipping the org check; match ignoring the
  member; match ignoring the org; org links not standalone; pending
  delivered anyway; no redelivery; delivery unfiltered on the standalone
  path.
- CLI (7): join not adopting; `up` not installing; plain approve allowed;
  both org-key checks skipped; pending never re-asked; the pending link not
  kept across restart; no identity proof before redeem.

**Regressions.**
- The SDK suite as CI runs it: 818/818.
- `net-cli` 357/357.
- Clippy (`net-cli` all targets, SDK lib and touched targets, SDK `full`) and
  SDK rustdoc are clean.

**Found, not fixed (pre-existing).** Private org discovery keys on an
org-wide owner audience. `NodeAuthority::adopt` mints a fresh one per node,
and nothing distributes it, so two separately adopted members cannot
discover each other privately. The protected-call witness pre-stages it, as
the existing live facade test does. Admission itself is unaffected.
Distribution belongs with the org half's next slices.

**O2 `org remove`:** see the next receipt.

#### V3-2 org O2 — `org remove` (receipt, 2026-09-24)

Decisions (user, 2026-09-24):
- floors are delivered **over the mesh, attested**, the same shape as
  `subnet remove`;
- the generation is **explicit and required** (`--minimum-generation`).

**Mechanism: reused, not replaced.** A floor is the existing root-signed
`OrgRevocationBundle`. `NodeAuthority.revocation.apply_bundle` merges it
monotonically, persists it before publishing the live view, and reloads it at
restart. Admission checks `floor_for(org, member)` against that live view.
What was missing, and is now added:
- a way to put a floor onto a *running* node;
- proof that it landed.

**SDK `net_sdk::org::floors`.**
- `OrgFloorRequest`: the bundle, the named node and a nonce, with a ±300 s
  freshness window.
- `answer_org_floor`:
  - applies the bundle to the node's installed authority;
  - signs an `OrgFloorAttestation` with the node's entity key over the exact
    request digest;
  - the attestation carries the outcome (`applied` / `not_member` /
    `uncertain` / `refused`) and the node's effective floor per named member;
  - `DurabilityUncertain` is never reported as success;
  - a request naming another node is refused.
- `serve_org_floor` (`net.org.floor.apply`, applied off the async runtime)
  and `request_org_floor`.
- A floor authenticates itself (root-signed, can only raise), so any peer
  may carry one.

**CLI.**
- `net-mesh org remove <member> --org-key K --minimum-generation N
  --verifier self|ENTITY@HOST:PORT#PUBKEY … [--dry-run] [--wait]`:
  - signs the floor **on the operator's machine**;
  - carries it through the operator's node (control op `org_floor_forward`:
    answers locally, or connects and forwards);
  - verifies every attestation against its own request;
  - reports per node; `complete` only when every named node attested
    applied at ≥ N;
  - states the scope: transport and other grants are untouched, and nodes
    not named were not asked.
- Every `up` serves floor application.
- A node whose adopted membership has ended keeps running without the org:
  - revoked by a floor → `org_state: revoked`;
  - expired or invalid → `invalid`.
  - Removal from an org is not removal from the mesh. A *corrupt* authority
    directory still fails closed.

**Witnesses.**
- SDK `a_floor_revokes_one_member_at_the_provider_and_survives_its_restart`
  is the decisive one:
  - members B and C both make real org-protected calls to provider P;
  - a floor for B, delivered through C's node, is attested `applied` with
    floor 2;
  - B's calls are refused and C's are admitted;
  - P restarts (a new node reopening its persisted authority): the floor
    holds, B is still refused, C still admitted.
- SDK `a_node_without_org_authority_attests_not_member` covers the
  `not_member` outcome, a tampered attestation, and a request for another
  node.
- CLI `org_remove_applies_a_root_signed_floor_at_each_named_node`:
  - the operator (itself a member via `node adopt`) removes an enrolled
    device;
  - self and the device attest `applied`, a mesh-only bystander
    `not_member`, so `complete` is false;
  - `--dry-run` has no effect;
  - the device restarts running, with its org `revoked`;
  - a corrupt authority directory refuses to start;
  - after the operator restarts, its persisted floor is re-attested.

**Inverse mutations** (all RED):
- SDK: floor not applied but reported applied; attestation not bound to its
  request; signature unchecked; a request for another node answered; a
  non-member claiming applied.
- CLI: any outcome counted as applied; a revoked membership failing
  startup; a corrupt authority tolerated; the node not serving floor
  application.

**Regressions.**
- The SDK suite as CI runs it: 820/820.
- `net-cli` 358/358.
- Clippy (`net-cli` all targets; SDK lib default, CI features and `full`;
  the touched test target) and SDK rustdoc are clean.

**Still open (org).**
- ~~Distributing the org owner audience.~~ Closed by the next receipt.
- Floors reach only the nodes named. There is no inventory of enforcement
  points; `org members` is a later slice.
- ~~`org leave`.~~ See the O4 receipt.

#### V3-2 org O3 — the org's shared audience (receipt, 2026-09-24)

Decision (user, 2026-09-24): an **explicit audience file**.

**The gap.** Private discovery of owner-scoped services keys on one org-wide
owner audience, but every adopting node minted its own. Enrolled members
could be admitted, yet could not find each other privately.

**Core.** `NodeAuthority::adopt_with_audience` is the same adoption ceremony
(the same lock and checks), installing a supplied org audience through the
ceremony's own owner-only atomic writer. It replaces a node-local audience,
since rotation is config management. A new variant, `AudienceForeignOrg`,
refuses an audience naming another org.

**SDK.**
- `OrgCertSource::audience_for` (default `None`), and
  `OrgCertStash::put_audience` (owner-only; refused for another org).
- The bundle carries the audience as secret material, redacted in `Debug`.
  The device accepts it only beside a membership of the same org
  (`Mismatch("org audience")` otherwise).
- The standalone reply is now `OrgIssued { cert, audience }`.

**CLI.**
- `org audience-keygen --org-key K --out F` mints it (owner-only, bound to
  the org id).
- `org approve --audience F` sends it with the certificate; the node keeps
  it per claim.
- The device adopts with it at `join` and `org join`; the adoption report
  names `audience: org`.
- `node adopt --audience F` covers existing members.
- The audience file is read through the secret-file gate: owner, type and
  mode are checked on the opened descriptor. The repo's
  `no_seed_loader_reimplements_the_permission_check` guard caught a
  hand-rolled mode check first.

**Witnesses.**
- Core `adopt_with_audience_installs_the_org_audience`.
- SDK: the protected-call witness now runs with **no pre-staging**. The
  device adopts the audience delivered by enrollment, the provider adopts
  the same file, and the device discovers the provider privately and is
  admitted.
- SDK: bundle audience refusal, wire round-trip and `Debug` redaction; the
  standalone reply carrying the audience; the stash refusing a foreign
  audience.
- CLI `org_join`: the device's `owner-audience.key` equals the operator's
  file byte for byte; `node adopt --audience` does the same for the
  operator.

**Inverse mutations** (all RED):
- core: the supplied audience not written (the real protected call then
  fails); a foreign-org audience accepted;
- SDK: the issuer dropping the audience; the device skipping the org check;
  the standalone reply dropping it;
- CLI: approve not sending it; `node adopt` ignoring it; `join` adopting
  without it.

**Regressions.**
- `cargo tl` 5818/5818.
- Every integration binary: 7039/7039.
- The SDK suite as CI runs it: 820/820.
- `net-cli` 358/358.
- Clippy (core strict and all-targets, CLI, SDK) and rustdoc (root, SDK) are
  clean.


#### V3-2 org O4 — `org leave` (receipt, 2026-09-24)

Decision (user, 2026-09-24): **record, then restart**. A live core uninstall
of the org authority was weighed and deferred: it touches the fence-heavy
install path, and a device can afford a brief restart.

**Behavior.** `net-mesh org leave [--state-dir] [--wait]`:
- **Running node** (control op `org_leave`):
  - durably records `<state>/authority.left` (the org and the time; written,
    synced, then renamed);
  - drops pending standalone links to that org;
  - drains and stops the node, and the CLI waits for the lifetime lock
    (`runtime: stopped`).
  - A node already running without the org, after an earlier leave, is left
    alone (`runtime: unchanged`).
- **Offline:** the departure is recorded directly (`runtime: not running`).
- **Next start:** `up` skips the org authority and reports `org_state: left`.
  The mesh relation is unaffected.
- **Kept:** the authority files, including the revocation floors, for a
  later rejoin.
- **Stated in the output:** the org is not notified (it accepts the
  device's certificate until `org remove`), and how to rejoin.
- **Rejoin needs fresh authorization:**
  - re-running the original join token does **not** re-adopt; `join`
    reports `org: left`;
  - only an approved adoption clears the record: `org join` with a new
    link, or `node adopt`.

**Witness.** CLI
`org_join::org_leave_holds_until_an_approved_rejoin`:
- join the org → `org leave` stops the node;
- the next `up` stays on the mesh without the org;
- the original token does not restore it;
- a new approved `org join` reinstalls it live and holds across restart;
- an offline `org leave` holds at the next start.

**Inverse mutations** (all RED): `up` ignoring the record; a running leave
not recorded; the node kept running as a member; the original token
re-adopting; rejoin not ending the leave; an offline leave not recorded.

**Regressions:** `net-cli` 359/359; `net-cli` clippy (all targets, bins) is
clean.

**Still open (org).** ~~`org members`~~, closed by the V3-3 first-slice receipt.
A live authority uninstall (no restart) remains possible as its own core
slice.

#### V3-2 task 3: standalone subnet join, decisions (user, 2026-09-24)

A device already on the mesh redeems a subnet-only link (`relations =
[Subnet]`; the invite format already allows it).

- **Transport: nRPC on the existing session.** A new service
  `net.enroll.subnet.redeem` runs on the issuing node.
  - The request is signed by the device and binds the destination node and a
    freshness window.
  - The issuer additionally requires that the session which delivered it has
    proven that same entity (`peer_identity_established` and
    `peer_entity_id`). This is E5: bind the same proven identity.
  - It reuses the ledger's claim, approval (`invite approve`), issue and
    recovery steps.
  - It returns only subnet credentials: no PSK is re-delivered and no TCP or
    relay path is involved.
  - Renewal already runs this way and works unchanged for such invites.
- **Device side: through the running node.** `net-mesh subnet join <token>`
  asks the running `up` over its authenticated control endpoint. The node:
  - redeems on its session;
  - persists the credentials in a per-invite store
    (`<state>/subnets/<invitation-id>`);
  - presents them at once, and its link supervisor re-presents and renews
    each membership on every session.
  - A device may hold several memberships, and `leave` covers them.
- **v1 limit.** A standalone link is redeemable only from the node this
  device enrolled with: its issuer must be the join's issuer, which supplies
  the node to call. A link from another operator is refused with a clear
  error.

#### V3-2 S1 — subnet admission on the wire (receipt, 2026-09-23)

**Protocol.** Subprotocol `0x0A02`, `SUBPROTOCOL_SUBNET_ADMISSION`, in the auth
family next to identity proof `0x0A01`. It is now registered in
`docs/SUBPROTOCOLS.md`, together with the previously unlisted `0x0A01`.
Codec: `subnet/admission_wire.rs`.

1. The device sends `ChallengeRequest{nonce}`.
2. The verifier answers `Challenge{nonce, verifier, session_id, challenge}`,
   where the challenge comes from the existing one-use challenge store.
3. The device sends `Present{nonce, presentation, credential set}`.
4. The verifier answers `Verdict{nonce, refusal}`.

**Dispatch and node.**
- The dispatch arm uses the same §12 provisional gate as identity proof.
- Replies complete a pending leg only from the node the leg went to.
- Verifier legs run on the node through `self_weak`, now shared with
  `DispatchCtx`: at most 64 concurrent, and auth-throttled peers are dropped.
- The verifier runs the **unchanged** `admit_subnet_session`: credential
  chain, floors including subject floors, and the routing-id pin.
- A node without subnet authorities answers `unknown_authority` and mints no
  challenge.
- Prover API: `MeshNode::present_subnet_credentials(verifier, set, target,
  rights, timeout)`. Each leg retransmits the same correlation nonce, and the
  pending entry is removed on every exit, including cancellation.

Witnesses, `tests/subnet_wire_admission.rs` 3/3 (pinned in ci.yml):
- `a_device_is_admitted_over_the_wire_and_refused_after_removal`: B and C are
  admitted over the wire; after a subject floor B is `Revoked`, its context is
  gone, and C's context is identical.
- `delegated_credentials_are_admitted_over_the_wire`.
- `refusals_are_verdicts_and_nothing_is_installed`: unanchored →
  `unknown_authority`; another entity's leaf → `wrong_subject`, nothing
  installed; no session → `NoSession`.

Inverse mutations, both caught and restored:
- the verifier admitting without checking;
- the prover treating any verdict as admission.

Not witnessed:
- The "reply only from the addressed node" guard. A forger would have to guess
  a 64-bit correlation nonce; the guard mirrors identity proof's.
- The unanchored node minting no challenge state. Admission would still fail
  at the authority lookup either way.

Regressions:
- `cargo tl` 5813/5813.
- 205/205 across the 15 subnet binaries, `channel_identity_readiness`,
  `connect_direct`, `routed_transport_availability`,
  `three_node_integration` and `rtc_admission`.
- Clippy (all-features all-targets, lib/bins, default; `net-cli`) and rustdoc
  are clean. `large_enum_variant` was fixed by boxing the `Present` payload.

#### V3-2 S2 — the subnet relation and delegated issuance (receipt, 2026-09-23)

**Invite.**
- New `Relation::Subnet` (tag 2) and a signed `SubnetOffer`: the
  authority-qualified scope, the topology epoch, and strict rights.
- The offer exists **exactly** when the relation does; this is checked at
  signing and at decoding.
- The format is unreleased and was changed in place.

**Bundle.**
- Optionally carries the device's encoded `SubnetCredentialSet`.
- `verify_for` now also requires the delivered leaf to match the offer
  exactly: authority, scope, epoch, rights, and subject equal to the intent's
  subject.
- It refuses missing credentials for a subnet invite, and any credentials for
  a mesh-only invite.
- The chain's signatures remain the verifier's to check at admission.

**Issuance.** `SubnetLeafIssuer` holds a root-signed `SubnetIssuerGrant`, the
issuer key (which must be the issuer the grant names), a generation, and a
leaf lifetime.
- `covers(offer)` checks authority, epoch, the subtree and the rights ceiling.
- `MembershipIssuer::with_subnet_issuer` issues a **one-hop** set for exactly
  the redeeming device, never valid beyond the grant.
- A subnet invite without a configured issuer, or outside its envelope, is
  refused `Unavailable` rather than issued as a partial bundle.

Witnesses, `sdk/tests/enrollment_subnet.rs` 4/4 (the SDK job auto-discovers
it):
- The offer is signed; relation and offer must agree; widening the rights
  breaks the signature.
- Issuance for exactly this device, whose chain passes the core
  `verify_credential_set` against the root.
- The device refuses another scope, another subject, wider rights, missing
  credentials, and credentials smuggled into a mesh-only bundle.
- The issuer cannot exceed its grant (subtree, ceiling, key identity).

Inverse mutations, 4 of 4 caught and restored:
- the device accepting any credentials;
- the device ignoring the leaf subject;
- the issuer ignoring its rights ceiling;
- relation and offer consistency unchecked at signing.

Regressions:
- The full SDK suite as CI runs it: 810/810.
- `net-cli` 350/350.
- Clippy (SDK `full` all-targets, SDK `net` lib, `net-cli`) and SDK rustdoc
  are clean.

#### V3-2 S3 — CLI subnet join, `up` as verifier, and the removal journey (receipt, 2026-09-23)

**CLI.**
- `up --enroll --subnet-issuer-grant G --subnet-issuer-key K
  [--subnet-leaf-ttl 24h] [--subnet-generation 1]` loads the root-signed issuer
  grant and the issuer key; the root stays offline.
- With that, the node:
  - trusts the grant's authority;
  - persists floors at `<state>/node/subnet-floors`;
  - serves floor readback;
  - issues one-hop leaves through `NodeBundles`.
- Readiness reports `enrollment.subnet {authority, issuer_scope, max_rights,
  topology_epoch, verifier: true}`.
- `invite create --subnet PATH [--subnet-rights R]` builds the signed offer
  and refuses anything outside the issuer grant **at creation**. `invite
  inspect` shows the offer.
- `join` reports `subnet.credentials: "installed"`.
- Joined `up` presents the credentials over `0x0A02` after attaching and
  reports `joined.subnet {scope, rights, admitted, detail}` from the
  verifier's verdict, never from holding the credentials.
- SDK: `MeshBuilder::subnet_floor_store`.

**Core defect found and fixed** (user-approved direction: ignore control
streams).
- The routed-handshake and direct-upgrade busy gate
  (`has_open_streams() || has_unacked()`) counted **every** stream. That
  included the fire-and-forget control-plane subprotocol streams, whose id is
  the subprotocol id, and those entries never go away.
- So after any capability announcement, identity proof or subnet admission, a
  **restarted** peer's re-handshake was deferred (`DeferBusy`) until the old
  session timed out (30 s). That is longer than joined `up`'s 20 s attach.
- An operator serving nRPC always triggered it, because it announces
  capabilities to every peer.
- The gate is now `session_is_busy`: unacked reliable data on any stream, or
  an open stream whose id is not in `CONTROL_SUBPROTOCOL_STREAM_IDS` (every
  registered subprotocol id). New wire method: `has_open_streams_where`.
- This matches the gate's documented intent, and `has_open_streams`' own doc
  ("control-plane traffic … is not counted").
- In-flight reliable data still defers. The existing witnesses (application
  ids `1` and `0xABCD`) keep their meaning.

Witnesses:
- Unit: `routed_rotation_ignores_control_plane_streams` (control-only →
  `AcceptRotation`; plus an application stream → `DeferBusy`) and
  `control_stream_list_covers_the_core_subprotocols`.
- CLI `cli/tests/subnet_join.rs`, the full journey as real subprocesses in
  about 11 s, stable across repeated runs:
  1. The offline ceremony (root key, issuer key, issuer grant over `3`).
  2. `up --enroll` with a PSK file and the subnet issuer.
  3. An out-of-grant invite (`4.1`) is refused at creation.
  4. `invite create --subnet 3.7`; `join` installs the credentials.
  5. Joined `up` shows `admitted: true`.
  6. `subnet remove` against the operator is attested `applied`, with
     `complete: true`.
  7. The device's next start shows `admitted: false`, detail `revoked`.

Inverse mutations, 5 of 5 caught and restored:
- the busy gate counting control streams again;
- `0x0A02` left out of the control list;
- joined `up` reporting admission without presenting;
- `invite create` skipping the envelope check;
- the `up` verifier keeping floors only in memory.

Regressions:
- `cargo tl` 5815/5815.
- `net-mesh-wire` 276/276.
- **Every** integration binary (`cargo t`): 7035/7035.
- The full SDK suite as CI runs it: 810/810.
- `net-cli` 351/351.
- Clippy (core all-features all-targets, lib/bins all, default and
  no-default; SDK `full`; `net-cli`) and rustdoc (root, `net-mesh-wire`, SDK
  `full`) are clean.

**V3-4 open items closed by this slice.** `up` is now a subnet verifier, and
the CLI removal journey runs end to end.

**Still open.**
- ~~`subnet remove` needs the mesh PSK.~~ Closed by the next receipt.
- Standalone subnet join by an already-connected device (V3-2 task 3).
- ~~Leaf renewal before expiry.~~ Closed by the V3-2 task 5 receipt.
- Organization enrollment.

#### `subnet remove --state-dir`: removal through the operator's own node (receipt, 2026-09-23)

`subnet remove --state-dir DIR` (which conflicts with `--psk-hex`) reaches
the verifiers through the operator's running `up` node, over its
authenticated local control endpoint. No PSK is needed: a PSK that `up`
generated never leaves the node.

**The CLI still does all the trust work.**
- It signs the floor and every readback request with the root key, which never
  reaches the node.
- It decodes and verifies every returned attestation against its own request
  (`verify_for`), so the node carries bytes and cannot vouch for anything.
- `--verifier self` resolves to the node's own entity, taken from its
  authenticated status.

**The node.** Control op `subnet_floor_query`:
- The node answers locally when it is the named verifier.
- Otherwise it connects to the verifier's contact if it has no session, then
  relays over its own mesh, bounded by at most 20 s.
- It replies `attestation`, `refused` or `no_answer`.
- The server-side control session bound rose from 5 s to 30 s so a forward can
  include a connect. Clients keep the 5 s bound for ordinary ops;
  `control_call_within` sets a longer one.

Witness: `cli/tests/subnet_join.rs`.
- The operator runs with a PSK file, used only so a second **in-process**
  durable verifier can share the mesh. The CLI never receives it.
- `subnet remove --state-dir … --verifier self --verifier <other>` reports
  both as `applied`.
- The other verifier's row carries its own entity, reached by the operator's
  node connecting first.

Inverse mutation: a forward that never connects is caught.

Not witnessed: the CLI refusing an attestation a *lying* node forged. It
would take a hostile node, and `verify_for`'s binding and signature checks are
witnessed at unit level (slice 2).

Regressions: `net-cli` 351/351, and `net-cli` clippy is clean.

#### V3-2 task 5 — subnet leaf renewal (receipt, 2026-09-23)

A subnet-joined device holds a delegated leaf that expires (`up
--subnet-leaf-ttl`, default 24h). It now renews that leaf from the node that
issued it, over nRPC service `net.enroll.subnet.renew`.

**The request.** `SubnetRenewRequest` (`NMSR`) is signed by the device's own
identity key over:
- the signed invite;
- the device's subject;
- the issue time (the issuer honours it for ±300 s);
- a nonce.

**The issuer re-issues only when** (`answer_renewal`):
- the request is fresh and the signature verifies;
- the invite carries a subnet offer;
- the invite is this node's own, and its digest matches the ledger's record;
- the ledger shows that invite issued to **this same device**. Any other
  device gets `Conflict`, and a revoked issuance gets `Revoked`.

The serving handler (`serve_subnet_renewal`) also refuses a subject that a
subject floor at this node removes from the offered scope. A delegated leaf
never satisfies a floor, so renewal never re-admits a removed device. The
fresh leaf covers exactly the original offer. The device re-checks it against
the signed offer (`replace_subnet_credentials`) before persisting it.

**CLI.**
- The operator's `up --enroll` with a subnet issuer serves renewal.
- A joined `up` renews at start when a third or less of the leaf's life is
  left, or when it has already expired, before presenting. It persists the
  fresh leaf.
- A background task then renews at the same point, persists the leaf and
  re-presents it (5 s floor between renewals, 30 s retry after a failure). The
  task stops at shutdown.
- The start report gains `expires_at`, `renewed` and `renew_error`.
  `node status` gains `subnet_expires_at`.

**Leave fence.** `replace_subnet_credentials` refuses a left join
(`DeviceJoinError::Left`), so a renewal completing after `leave` installs
nothing. The owner mutex orders it against `leave`. This closes the
"no renewal exists yet" note in the V3-2B receipt.

**Issuance fix found on the way.** `SubnetLeafIssuer::issue` counted the
leaf's lifetime from its 60 s back-dated `not_before`. A leaf with a TTL under
a minute was therefore born expired. The lifetime now runs from `now`. The
back-dating still covers clock skew, and the renewal point ignores that
back-dated minute.

**Witnesses.**
- SDK `only_the_enrolled_device_renews_its_subnet_leaf` covers:
  - not issued yet;
  - issued, then renewed for exactly the offer;
  - another device (`Conflict`);
  - stale requests, forged signatures, mesh-only invites and foreign invites.
- SDK `leaving_erases_the_credentials_…` covers renewal after leave → `Left`.
- CLI `a_joined_node_renews_its_subnet_leaf_and_removal_stops_renewal` runs
  with a 15 s leaf TTL:
  - a fresh leaf is admitted with no renewal;
  - the running node's `subnet_expires_at` moves forward (background renewal);
  - stopped past expiry, the next start renews first and is admitted;
  - after `subnet remove --verifier self`, the next start's renewal is refused
    as revoked and admission is refused.

**Inverse mutations** (all RED):
- no renewal at start;
- no background renewal;
- the handler ignoring subject floors;
- the leaf lifetime counted from the back-dated start;
- `answer_renewal` accepting any subject;
- the request signature not checked;
- no `Left` check on replace.

**Regressions.**
- The full SDK suite as CI runs it: 811/811.
- `net-cli` 352/352.
- `net-cli` clippy (all targets) is clean.
- SDK rustdoc (`full`) is clean.
- The SDK builds with default features and with `--no-default-features
  --features net`.
- Two pre-existing SDK `--all-features` lints outside this change
  (`rtc_bootstrap.rs` `iter_kv_map`, `tests/sensing_consumer.rs`
  `explicit_auto_deref`) are unchanged.

**Found, not fixed (core policy): a restart after nRPC use is deferred by up
to `session_timeout`.** nRPC request and reply channels are application
streams, and they stay open after a call. By the C3 busy gate
(`routed_rotation_outcome`), the operator therefore defers a crashed-and-
restarted peer's re-handshake until the old session times out (30 s). The
joined `up`'s attach wait is 20 s, so a device killed within ~30 s of its last
renewal (or of any nRPC call) comes back `attached: false`.

This predates renewal; renewal makes every subnet device an nRPC user. The
CLI test waits past the lapse and says so. Options, for a decision:
- close or ignore idle nRPC channels in the busy gate;
- stretch the joined attach wait past `session_timeout`;
- add background re-attach for a joined `up`.

**Still open.**
- ~~Leaf renewal before expiry.~~
- Standalone subnet join by an already-connected device (V3-2 task 3).
- Organization enrollment.
- ~~The restart deferral above.~~ Closed by the next receipt.

#### Self-healing joined link, stable Noise key, C3 liveness (receipt, 2026-09-23)

This closes the restart deferral found in the renewal receipt, in two
commits. Writing the re-attach witness also surfaced a larger defect.

**Defect found: the operator's Noise key changed on every restart.**
`MeshNode::new` always generated a fresh Noise static key. The node id and
entity key survived a restart; the Noise key did not. Invites and bundles pin
that key, so after **any** operator restart no enrolled device could ever
reattach: every handshake silently timed out. The shipped join flow was
affected, not just subnet joins.

Fix (decision: persisted key file):
- **Core:** opt-in `MeshNodeConfig::with_static_key(NoiseStaticKey)`. The
  public half is derived from the private key, `Debug` is redacted and the
  key is wiped on drop. The default (a fresh key) is unchanged.
- **SDK:** `MeshBuilder::noise_static_key`.
- **CLI:** node secrets v3 persist the key. v1 and v2 still read. A missing
  key is generated and committed before bind. `up` always uses it.
- **Downgrade note:** an older binary refuses v3 node state, the same as the
  v1 → v2 step.

**Self-healing link (commit `a115fd8b7`).** A joined `up` supervises its link
to the node it enrolled with:
- It re-attaches (direct, then relay, backoff 1–30 s) when the session is
  gone or silent.
- It re-presents subnet credentials on every new session, since admission is
  per session.
- It renews the leaf, and reports the live link in `node status`
  (`attached`, `path`, `reattaches`, `subnet_admitted`, `readmissions`).
- It stops once the join has left or is gone.

"Silent" is the new core `peer_session_is_silent`: no authenticated inbound
for `session_timeout`. The peer table keeps a dead peer for 30 ×
`session_timeout`, and the failure detector only says `Failed` after 3 ×, so
neither was a usable trigger.

**C3 liveness (commit `8baeed7f4`; decision: option 2).**
- **The problem.** The responder gate deferred a same-static re-handshake
  while the old session was busy and "live", and it keyed liveness on
  `is_timed_out(session_timeout)`. Two things made that wrong. Our own sends
  refresh that timestamp. And a finished nRPC call leaves its channel stream
  open. So a peer that restarted after any RPC waited out the full 30 s.
- **The check.** The C3 spec (NAT_TRAVERSAL_V2_PLAN) says "pending unary
  nRPC calls do not block the swap". Simply ignoring idle nRPC streams was
  rejected: large and streaming responses are session-bound, and between
  credit-paced chunks they have no unacked data. Such a stream can look idle
  mid-transfer.
- **Fix.** Liveness now means an authenticated inbound packet within 3
  heartbeat intervals, capped at `session_timeout`. This is what the plan
  specified: "keys deferral on recent authenticated inbound".
  - `NetSession::last_inbound` is stamped only after AEAD verification and
    counter admission, at the mesh receive path, the legacy adapter path and
    the heartbeat verify.
  - A live peer mid-transfer keeps heartbeating, so its transfer stays
    protected.
  - A restarted peer falls silent at once, so its re-handshake rotates
    within about 15 s at default settings.
- `peer_session_is_silent` uses the same inbound-only stamp.

**Witnesses.**
- CLI `a_joined_node_reattaches_and_is_readmitted_without_being_touched`
  (operator on a fixed port):
  - A: the operator restarts under a running device. Its `public_key` is
    unchanged, and the device reattaches and is re-admitted untouched.
  - B: a device started while the operator is down comes up unattached, then
    attaches and is admitted once the operator returns.
- Core unit tests:
  - `a_stored_noise_key_is_kept_across_builds`;
  - `busy_rotation_defers_only_while_the_peer_is_speaking` (our sends do not
    count; the peer speaking again re-defers).
- Integration `a_live_peers_rehandshake_does_not_cut_a_streaming_response`
  (in the pinned `integration_nrpc_streaming`): the C3 responder gate,
  mid-transfer. The plan listed this witness, but it did not exist.
- CLI unit test: `node_state_v3_round_trips_and_v1_v2_state_still_reads`.
- The renewal test restarts right after nRPC use with no session-lapse wait
  (86 s → 53 s).

**Inverse mutations** (all RED):
- no re-attach;
- silence not detected;
- `up` not using the stored key;
- no re-present on a new session;
- v3 decode dropping the key;
- core ignoring the stored key;
- liveness back to the whole `session_timeout` (unit test and CLI renewal
  test);
- never deferring (unit test and streaming witness);
- sends counted as the peer speaking;
- heartbeats not stamped as inbound (streaming witness: heartbeats are what
  keep a live peer "speaking").

**Regressions.**
- `cargo tl` 5817/5817.
- `net-mesh-wire` 276/276.
- Every integration binary (`cargo t`): 7038/7038.
- The SDK suite as CI runs it: 811/811.
- `net-cli` 353/353.
- Clippy is clean: core (all-features, default and no-default lib/bins; all
  targets), wire, and `net-cli`.
- Rustdoc is clean: root, wire (`json`), and SDK (`full`).

**CI along the way.** `f3eeaf82b` fixed two CI-only failures:
- `subnet_join` wrote its `--psk-from` file with mode 0644, which the CLI
  refuses on Unix;
- `nrpc_service_equality`'s positive controls trusted a single 150 ms sleep.
  They now poll, settle, and assert exactly one hit. With the gate removed,
  both tests still fail.

**Still open.**
- ~~The restart deferral after nRPC use.~~
- Standalone subnet join by an already-connected device (V3-2 task 3).
- Organization enrollment.
- ~~Unverified: re-attach through the relay after an operator restart.~~
  Closed by the next receipt.

#### Relay re-attach after an operator restart (receipt, 2026-09-24)

A device attached only through the blind relay (the direct contact is dead)
reattaches by itself after the operator restarts **on a new port**, so only
the fresh relay registration can lead back to it. Three properties make this
work:
- the registration id is derived from the operator's entity, so it is stable;
- the relay's re-registration re-points the id at the new endpoint, and
  forwarding resolves the endpoint on every packet;
- the persisted Noise key still matches the bundle.

No production change was needed: the supervisor's re-attach already tries
direct, then relay.

Witness: `relay_join::a_device_reattaches_through_the_relay_after_the_operator_restarts`
(`reattaches >= 1`, `path == "relay"`).

Inverse mutations:
- The relay keeps the old endpoint on re-registration: RED at the reattach
  assertion.
- The supervisor's re-attach drops the relay: RED at the reattach assertion.
- Not counted: salting the registration id per process, and dropping the
  relay from `attach_contact` altogether. Both went RED, but at the first
  `join`, so they do not isolate the restart path. The id's stability is
  structural: the relay computes it from the entity.

Noted, not changed: the relay allows 64 channels per registration by default
(`max_channels_per_registration`), and idle channels expire after 120 s. With
many devices behind one operator, a mass reattach after a restart could hit
that cap. This is relay sizing, and it applies to first joins too.

Regressions: `relay_join` 5/5; `net-cli` clippy is clean.

#### V3-2 task 3 — standalone subnet join (receipt, 2026-09-24)

A device already on the mesh joins another subnet with a subnet-only link,
over its own session.

**Operator.**
- `net-mesh subnet invite <scope> [--rights] [--require-approval] [--for]
  [--ttl] [--out]` creates a link with `relations = [Subnet]`, through the
  node's `invite_create` (the same envelope check as `invite create
  --subnet`).
- An `up` with a subnet issuer also serves `net.enroll.subnet.redeem`.

**Device.** `net-mesh subnet join <link|-> [--yes]` shows the scope, rights,
authority and issuer, then asks for the same YES confirmation as `join`. It
hands the link to the running `up` over its authenticated control endpoint
(`subnet_join`). The node:
- refuses a mesh invite, and a link whose issuer is not the one this device
  enrolled with (the v1 limit, stated in the error);
- records the membership (`<state>/subnets/<digest key>`) before redeeming;
- redeems over its session (SDK `request_subnet_redeem`);
- installs only credentials that are exactly the signed offer;
- presents them, and reports the verifier's verdict.

The link supervisor then keeps every membership admitted, through one shared
helper for the join's own subnet and for standalone memberships:
- it re-presents on every new session;
- it renews near expiry;
- it asks again for an approval-gated membership until it is issued;
- it reports each membership in `node status` → `link.standalone`.

`leave` records every membership as left, credentials erased, before
recording the join's own departure.

**SDK (commit `1ea7e1083`).**
- `SubnetRedeemRequest` is device-signed and binds the destination node and
  a ±300 s freshness window.
- `answer_subnet_redeem`:
  - requires the delivering session to have proven the same entity;
  - accepts subnet-only invites only, and only this ledger's own;
  - runs claim → approval → issue.
  - `AlreadyIssued` re-issues fresh credentials. The ledger returns that
    only to the identical claimant; an extra subject check there was an
    equivalent mutant, so it was removed.
- `serve_subnet_redeem` adds the subject-floor refusal.
- `SubnetMembership` is the device's per-link store.

**Found on the way.** `attach_contact` now retries the direct handshake
within its budget. A peer that just restarted can be deferred for a few
heartbeats (C3), which outlasts one `connect_via`'s own retries. A start
report can still be the first, unattached attempt right after nRPC traffic;
the supervisor completes it (measured: attached 0.6 s after the start
budget).

**Witnesses.**
- SDK:
  - `a_standalone_subnet_link_issues_only_to_the_proven_session_entity`
    covers the session binding, the destination, freshness, the signature,
    exact-offer issue, re-issue to the same device, a second device refused,
    mesh+subnet refused, and approval gating.
  - `a_standalone_membership_installs_only_its_offer_and_leave_fences_it`.
- CLI `a_joined_device_joins_another_subnet_with_a_standalone_link`:
  - join with 3.7; `subnet invite 3.8`; a mesh invite refused;
    `subnet join` → installed, admitted, and the same device entity;
  - restart → both memberships re-presented by the node;
  - a second joined device refused ("bound to another claim");
  - a link from another operator refused;
  - an approval-gated 3.9 link → `pending_approval`, then `invite approve`,
    after which the node completes it by itself;
  - `leave` → both membership stores left, with no credentials.

**Inverse mutations** (all RED):
- SDK: no session binding; no destination binding; mesh+subnet accepted;
  pending issued anyway; install without the offer check; no leave fence.
- CLI: the operator not serving redemption; a standalone link carrying the
  mesh relation; the foreign-issuer check skipped; the supervisor ignoring
  memberships; pending memberships never asked again; `leave` skipping
  memberships.

**Regressions.**
- The SDK suite as CI runs it: 813/813.
- `net-cli` 355/355.
- `net-cli` clippy is clean; SDK clippy (CI features, `full`) is clean apart
  from the pre-existing `sensing_consumer` lint; SDK rustdoc `full` is clean.

**Not done (v1 limits).**
- A standalone link from another operator on the mesh. Its node would need
  to be named in the link, or discovered.
- Org and channel standalone links.

### V3-2A — channel-scoped invitation, join and credential lifecycle

**Modify:** `src/adapter/net/mesh.rs` for exact publish-chain/cache lifecycle hooks; `sdk/src/identity.rs` to expose the canonical `TokenChain`; `sdk/src/mesh.rs` for a full-chain subscribe path; shared enrollment/persistence modules; `cli/src/commands/channel.rs`, `main.rs`, `context.rs`, `config.rs`; and the selected durable authority/runtime control owner. Modify `identity/token.rs` or `channel/config.rs` only for a separately source-proven gap.
**Proposed new test:** `cli/tests/channel_join.rs`; reuse `tests/channel_auth.rs`, `channel_auth_hardening.rs`, `channel_auth_origin_binding.rs` and `channel_identity_readiness.rs`, and add owning SDK/core witnesses for delegated multi-link subscribe, reconnect, local publish gating and exact-incarnation removal/cache fallback. A new root integration binary requires a CI pin.

Tasks:
1. Write RED subprocess witnesses for `channel invite`, effect-free generic inspection, `channel join` and both leave modes. Cover exact subject, trusted/untrusted root, canonical name/`u64`, same-`u16` collisions, subscribe routing target versus intended publisher metadata, local publish gate, rights, expiry, generation, revocation, delegated multi-link attenuation, same-identity retry, reconnect and restart.
2. Extend the shared invitation intent/result schema with canonical `ChannelName`, canonical `u64 ChannelHash`, token-root identity, requested rights, TTL and delegation depth. Include intended publisher `EntityId`/locator only when `SUBSCRIBE` is requested and derive its routing `NodeId`; do not claim the ACK authenticates the full identity. `PUBLISH` carries no remote publisher target. Compute name/hash with `ChannelName::new/hash`; reject an encoded name/`u64` mismatch, never accept a `u16` policy key, and prove two names sharing a hint remain separate.
3. Add the channel-issuance facet to the selected durable authority owner; do not refer to a pre-existing remote token-root service. It holds the issuing identity and durable invitation/receipt state. For `SUBSCRIBE`, v1 supports only a publisher runtime controlled by that owner: inspect its actual `ChannelConfig` and refuse invitation creation unless the issuer root is currently configured. For `PUBLISH`, issuance is allowed but redemption never installs a trust root; the subject's managed runtime must already trust it or that stage remains unavailable. On redemption, mint/recover one subject-bound `TokenChain` after durable winner commit. Require subscribe/publish, make delegate bounded and non-standalone, and refuse `ADMIN`, wildcard, widening and unsupported root custody.
4. Atomically store the canonical chain and relation metadata in protected local state before profile publication. Never print the link or chain in normal output. Same-identity lost-response recovery returns the committed chain; a second identity cannot redeem the invitation. A composed mesh/org/channel/subnet link records each stage independently and resumes only the same full intent.
5. Export the canonical `TokenChain` through `sdk/src/identity.rs` and add a chain-capable `sdk/src/mesh.rs` subscription API/options path that delegates to core `subscribe_channel_with_chain` without flattening to one `PermissionToken`. Preserve the legacy single-token API. Require a delegated multi-link positive/negative witness. On first join and every reconnect/restart, present the full chain and regain ACK; a later bare subscribe cannot satisfy live state.
6. Split live evidence. `SUBSCRIBE` is live only after the derived routing target ACKs the full-chain request; report the intended full publisher identity as invitation metadata, not reciprocal authentication. `PUBLISH` installs on the subject's local managed runtime and is credential-ready only when that runtime's config trusts the root. It becomes live-active only when a real caller-requested `publish`/`publish_many` clears the local production gate; optional subscriber receipt is end-to-end evidence, not “publisher acceptance.” Join emits no synthetic payload.
7. Add exact publish lifecycle hooks. V1 permits one active managed publish chain per `(profile, canonical channel)` and refuses conflicting overwrite. Add conditional removal keyed by exact installed chain fingerprint/incarnation, plus targeted `TokenCache` eviction/fencing so fallback cannot bypass leave. A stale leave cannot remove a successor; if another uncontrolled cache/SDK source remains, report stop-unconfirmed. Subscribe leave remains acknowledged unsubscribe of the exact routing target/channel relation.
8. Preserve authority limits in status: intended publisher metadata, derived routing ID/ACK, chain scope, local publish config/gate and same-root cross-publisher portability are separate fields. In inverse worktrees, use the `u16` hint as policy key, accept a self-issued root, flatten a delegated chain, auto-install a root, treat routing ACK as full identity proof, bypass exact removal through cache fallback, add `ADMIN`/wildcard, or relabel stored credentials as live. Each named witness must fail.

**Exit:** The new owner can create a one-time channel link and a clean subject can recover a protected, subject-bound, root-anchored full chain. Subscribe and publish use their distinct real paths: full-chain remote ACK on every (re)connect versus local publish-gate acceptance. Same-`u16` names never swap authority; canonical mismatches fail. Publish leave conditionally removes only its exact managed chain/cache incarnation and cannot remove a successor or fall back silently. No reciprocal publisher-identity proof, remote publish acceptor, implicit root installation, organization/subnet/invocation authority, `ADMIN` or wildcard is invented. Residual portability and stop uncertainty are explicit.

#### V3-2A decisions and slices (user, 2026-09-24)

**Survey.**
- Core already has the pieces: `TokenChain` (root → leaf, depth ≤ 8,
  attenuating delegation); `ChannelConfig.token_roots` with fail-closed
  gates; `subscribe_channel_with_chain`, which retains the chain for
  per-publish re-checks; and `set_publish_chain`.
- Gaps:
  - the SDK subscribes with a single token only;
  - nothing re-subscribes after a reconnect;
  - `published_chains` has no removal API, and `TokenCache` evicts only
    expired entries;
  - the subscribe ACK is an unsigned node-id check (no publisher entity
    proof);
  - the CLI can only read the channel registry.

**Decisions.**
- **Root custody: a delegated issuer.** An offline channel root signs one
  DELEGATE token (PUBLISH/SUBSCRIBE, bounded depth and lifetime) to the
  operator node's issuer identity. The node mints device leaves from it, so
  chains are root → node → device, and a leaf never outlives the grant.
- **Channel configuration: `net-mesh channel serve <name> --token-root …`.**
  It registers the gated config on the running node and persists it for
  restart. `invite create --channel` refuses unless the root is configured.
  The device side uses the same command, so publish readiness is never
  implied by a trust root being installed.

**Slices.**
- **C1 (core + SDK primitives):** chain subscribe in the SDK; publish-chain
  install, and exact conditional removal by fingerprint; targeted
  `TokenCache` eviction; the delegated channel issuer; witnesses for
  delegated multi-link subscribe and publish, and for exact removal.
- **C2 (issuance):** `Relation::Channel` in invites and bundles; the node
  mints the leaf at redemption; `channel serve` with persistence; the offline
  `channel issue-grant`.
- **C3 (device):** join installs the chain; `up` subscribes with the full
  chain on every (re)connect; the publish stage is ready only when local
  config trusts the root; both leave modes; channel status (credential state
  vs ACK / publish-gate observation, never a roster).

**C1 receipt (2026-09-24).**

What shipped:
- **Core `identity/token.rs`:**
  - `TokenChain::fingerprint` (blake3 over the canonical chain bytes);
  - `TokenCache::evict_exact`, which removes exactly one token's bytes from
    its `(subject, channel | wildcard)` slot.
- **Core `mesh.rs`:**
  - `install_publish_chain` allows one managed chain per channel. The
    identical chain is a no-op; a different chain gets
    `PublishChainConflict { installed }` and nothing is overwritten.
  - `publish_chain_fingerprint`.
  - `remove_publish_chain_if(channel, fingerprint)` is a DashMap
    `remove_if` on the exact incarnation. It also evicts that chain's tokens
    from the `TokenCache`, so the cache fallback cannot bypass a leave.
- **SDK:**
  - `SubscribeOptions.chain` delegates to `subscribe_channel_with_chain`
    unflattened; passing a token and a chain together is `SdkError::Config`.
  - `Mesh` wrappers for install, fingerprint and removal.
  - `net_sdk::channel_issuer::ChannelLeafIssuer`. It checks that the grant
    names the issuer's key, carries no ADMIN or WILDCARD, is DELEGATE with
    depth > 0, and verifies. `issue` mints `[grant, leaf]`: PUBLISH and/or
    SUBSCRIBE only, never empty, never beyond the grant, and never ADMIN,
    WILDCARD or DELEGATE.

Witness: `sdk/tests/channel_delegated.rs` (3 tests; auto-discovered by the SDK
job).
- The publisher trusts only the root:
  - the full chain subscribes;
  - the leaf alone is refused, and so is another device's chain.
- Publish:
  - passes the local gate only while the managed chain is installed;
  - a conflicting install is refused;
  - wrong-fingerprint removal is a no-op;
  - exact removal re-closes the gate and evicts the leaf from the cache;
  - a stale removal spares the successor.

Inverse mutations: each was applied, run against the witness, and reverted. All 7 went RED:

| # | Mutation |
|---|---|
| 1 | Removal ignores the fingerprint |
| 2 | Install overwrites a different chain |
| 3 | Removal skips cache eviction |
| 4 | SDK flattens the chain to its leaf |
| 5 | Leaves may carry DELEGATE |
| 6 | Rights beyond the grant |
| 7 | ADMIN grant accepted |

Mutation 5 was first GREEN, because the rights check also refused DELEGATE.
The witness now pins the `Forbidden` variant, and the mutation goes RED.

Gates:
- fmt; core clippy, strict and all-targets;
- SDK clippy: default, `full` and the CI feature set;
- rustdoc: root `--all-features` and SDK `full`;
- `cargo tl` 5818/5818; `cargo t` 7039/7039; SDK suite 825/825; CLI 360/360.

**Honest limit:** a leaf's expiry is the grant's (`delegate` copies
`not_after`). Per-device lifetimes shorter than the grant would need a new
core delegation variant, which is not in scope. Revoking one device is by
revocation, not by expiry.

**C2 receipt (2026-09-24).**

SDK:
- **`Relation::Channel` (tag 4)** carries a signed `ChannelOffer { channel,
  root, rights }`. The wire form holds the canonical name, its `u64` hash,
  the root and a rights byte.
  - Decode refuses a hash that is not the name's.
  - The rights are a non-empty subset of publish/subscribe.
  - The relation and the offer come together or not at all.
  - In v1 it rides with `Relation::Mesh`; a standalone channel link is not
    offered yet.
- **`MembershipBundle` carries the chain.** `verify_for` goes through
  `channel_chain_matches`. The chain must be anchored at the offered root,
  verify link by link for every offered right on the offered channel, and
  have a leaf with exactly the offered rights for the intent's subject.
  Otherwise the bundle is refused: a missing chain, a stray chain, the leaf
  alone, a wider leaf, or another device's chain.
- **Minting.** `MembershipIssuer::with_channel_issuers` mints only from an
  issuer that `covers` the offer (same root, same `u64` channel, rights
  within the grant); otherwise the result is `Refusal::Unavailable`. The
  committed bundle is what lost-response recovery returns, so a retry gets
  the same chain.

CLI:
- `channel issue-grant --root-identity <operator identity> --issuer <enrollment
  issuer> --channel <name> [--rights] [--ttl] --out` runs offline. It writes
  `{kind: channel-grant, channel, grant_hex}`. There is no secret in it, and
  it refuses to overwrite without `--force`.
- `up --enroll --channel-grant <file>` (repeatable) loads the grant for the
  **enrollment issuer**. That key is distinct from the node's mesh
  identity, so the chain is `root → enrollment issuer → device`.
  - Start is refused when the grant names another identity, or when the file's
    name does not match its signed hash.
  - `up` reports `enrollment.channels`.
- `channel serve <name> --token-root <ENTITY>…` is a control op
  (`channel_serve`). It persists `<state>/channels.json` and registers the
  gated config live. Every `up` re-registers it. A corrupt record fails the
  start closed.
- `invite create --channel <name> --channel-rights publish|subscribe|both`:
  - requires a grant for exactly that canonical channel covering the rights;
  - for subscribe, also requires the channel to be served here with that
    exact name and the grant's root among its token roots, because this
    node is the publisher the device is sent to;
  - publish installs nothing here.
- `invite inspect` and `join` report the channel. `join` reports
  `credential: stored`, because live use is C3.

Witnesses:
- `sdk/tests/enrollment_channel.rs` (4 tests):
  - the offer is signed and canonical (hash swap, rights widening,
    relation/mesh rules);
  - redemption mints exactly the offered chain;
  - the device accepts only the offered chain for itself;
  - two names sharing a `u16` wire hint never stand in for each other.
- `cli/tests/channel_join.rs` (2 tests; auto-discovered by the CLI job):
  - the full ceremony: learn the issuer, grant offline, grant loaded;
  - subscribe is refused until served under the right root;
  - no grant for another channel; rights are explicit;
  - the device joins with a verified chain; publish needs no serving;
  - serving survives a restart; a corrupt record fails closed;
  - a grant for another identity, or with an edited channel name, is
    refused at start.

Inverse mutations, all RED (11):

| # | Mutation |
|---|---|
| 1 | Decode skips the hash check |
| 2 | Channel without mesh |
| 3 | Device accepts any chain |
| 4 | Leaf rights not exact |
| 5 | Issuer ignores `covers` |
| 6 | `covers` ignores the channel |
| 7 | Subscribe needs no serve |
| 8 | Serve check ignores the root |
| 9 | Corrupt served record ignored |
| 10 | Served channels not re-registered at `up` |
| 11 | Grant name not checked |

Gates:
- CLI clippy `--all-targets`;
- SDK clippy: default, `full` and the CI features;
- rustdoc: SDK `full` and net-cli;
- SDK suite 829/829; CLI 362/362.
- Core is unchanged in C2.

**C3 receipt (2026-09-24).**

`cli/src/commands/channel_link.rs` owns a joined device's channel
credential at runtime. Status keeps the two real paths apart:

- **Subscribe.**
  - At `up`, and again by the link supervisor on every new session, the
    device presents the full chain with `subscribe_channel_with_chain` to the
    node it enrolled with.
  - `subscribed: true` is that publisher's ACK on the current session;
    `resubscribes` counts later sessions. With no live session nothing is
    claimed.
- **Publish.**
  - The chain is installed as the node's managed publish chain
    (`install_publish_chain`), shown as `publish_installed`.
  - `publish_ready` is recomputed on every supervisor pass. It is true only
    while this node's own config for exactly that channel trusts the chain's
    root (`channel serve` on the device). No root is installed implicitly.
- **Expiry.** An expired credential is reported `expired` and never used.

Leave:
- `channel leave` (control op `channel_leave`) works in this order:
  1. It records `<state>/channel.left` durably.
  2. It sets the runtime's left flag, so a racing supervisor subscribe
     cannot win.
  3. It sends an unsubscribe, bounded wait, and reports `unsubscribed`
     true/false.
  4. It removes exactly the installed chain incarnation by fingerprint,
     evicting its tokens (`publish_removed`).
  5. It reports `publish_stop: confirmed` only when no publish chain and no
     cached PUBLISH token for the device remain. Otherwise it reports
     `unconfirmed`.
- The departure is idempotent and survives restart: `up` reports `left`
  and uses nothing. Mesh membership is untouched.
- A whole-mesh `leave` of a subscribed device withdraws the subscription
  first and reports `channel_unsubscribed`.

Status: `channel status` returns the served channels and the joined
credential, never a roster. `node status` carries `channel`.

Witnesses in `cli/tests/channel_join.rs` (4 tests):
- **Subscribe test:**
  - ACKed at start; subscribe only;
  - resubscribes by itself after the operator restarts (served channel
    re-applied from state);
  - channel leave is acknowledged and idempotent, survives the device's
    restart, and leaves the device attached;
  - a whole `leave` of a second subscribed device reports
    `channel_unsubscribed: true`.
- **Publish test:**
  - installed but not ready without local trust, and not ready when a
    different root is trusted;
  - ready under the right root;
  - channel leave removes exactly the chain with the stop confirmed;
  - serving stays independent of holding the credential.

Inverse mutations, all RED (9):

| # | Mutation |
|---|---|
| 1 | Subscribe flattens the chain to its leaf |
| 2 | Supervisor never resubscribes |
| 3 | Readiness ignores the root |
| 4 | Ready = installed |
| 5 | Channel leave not recorded |
| 6 | Leave keeps the publish chain |
| 7 | Leave skips the unsubscribe |
| 8 | Whole leave keeps subscribing |
| 9 | Start ignores the left marker |

Gates: fmt, CLI clippy `--all-targets`, net-cli rustdoc, CLI 364/364.

**V3-2A closure, and what stays open (honest limits):**
- A subscribe link's publisher is the issuing node, reached at the
  bundle's contact. Its ACK is a routing fact, not a proof of its full
  identity.
- Same-root portability to other publishers is possible, since the chain
  anchors at the root, but the CLI does not automate it.
- Leaf expiry equals the grant's (C1). Per-device revocation is by
  revocation floor.
- A channel link rides with mesh membership. A standalone channel link for
  an already-joined device, and rejoining a channel after a channel leave,
  need a fresh link and are not offered yet.
- `publish_ready` is credential readiness. Whether an application's publish
  cleared the gate is that application's observation, and the CLI has no
  publish verb.
- The `publish_stop: unconfirmed` branch is reachable only when another
  credential source exists. No witness exercises it.
- CI on the C1 head (f9aff9287) failed on two unrelated issues:
  - an `enrollment_lifecycle` free-port bind race
    (`Address already in use`);
  - the known browser-matrix signalling flake.

### V3-2B — voluntary leave through the same lifecycle

**Modify:** shared SDK enrollment/persistence/lifecycle modules selected in V3-1/2A; CLI `enrollment.rs`, `org.rs`, `channel.rs`, `subnet.rs`, `config.rs` and relevant runtime adapters.
**Proposed new test:** `cli/tests/enrollment_leave.rs`. Keep this inside the existing lifecycle work, not a new control-plane project.

Tasks:
1. Write RED for mesh/org/channel/subnet leave, offline authority, repeated leave and an in-flight join/renew/subscription completion arriving after departure. Assert preserved device identity and unaffected relations/data.
2. Implement durable scoped disabled intent, callback fencing, controlled runtime stop and profile/credential deactivation. Preserve the existing owner guard and revocation floors.
3. Add explicit-target leave commands, bounded stop/notification behavior and partial receipts. Subscribe leave persists disabled intent before acknowledged unsubscribe. Publish leave persists intent before exact-incarnation conditional chain/cache removal; stale removal, another active source or cache fallback cannot become complete-left success. Distinguish copied token validity from local deactivation. Authenticate any remote self-notification and keep advisory notification separate from revocation.
4. Prove restart does not renew/rejoin; genuine explicit rejoin uses current authorization; delayed old leave/renew callbacks cannot affect the successor. An unmanaged live consumer must produce stop-unconfirmed, not success.
5. In a disposable review worktree, bypass disabled-intent checks and allow a late renewal to install: the relevant witnesses must fail. Restore the candidate and run compatibility controls.

**Exit:** Local voluntary departure for mesh, organization, channel and subnet relations is usable online/offline and remains in effect across restart, with honest runtime and remote-state reporting. Subscribe unsubscribe, local publish-chain/cache deactivation and copied-token/issuer revocation are distinct; no leave depends on remote approval, removes a successor, or claims stop while an uncontrolled fallback remains.

#### V3-2B mesh relation — `net-mesh leave` (receipt, 2026-09-23)

Scope: the mesh relation, the only relation V3 enrolls so far. Organization,
channel and subnet leave follow their enrollment slices (V3-2/2A).

**Owner and fencing.** The join store (`DeviceJoin`, protected storage with an
exclusive owner lock) is the durable owner of the departure.
- `DeviceJoin::leave(now)` records `left_at` and erases the delivered bundle
  (PSK and contact) in **one** durable write. It keeps the device identity,
  invite and intent.
- The snapshot is now NMDJ v2 (v1 still reads as "not left"). A left snapshot
  that still carries a bundle is refused as corrupt.
- A left join refuses `redeem` (`DeviceJoinError::Left`). Only an explicit
  `rejoin()` clears the state, and the next redeem must go back to the issuer
  and pass its current authority (recovery). Nothing is reinstalled locally.
- The owner lock totally orders leave against any in-flight `join` on the same
  state. No renewal exists yet, so there is no late renewal to fence.

**Runtime.**
- A running joined `up` owns the join store, so `net-mesh leave` goes through
  that node's authenticated control endpoint (op `leave`). The node records the
  departure through the store it owns, replies with its incarnation, and then
  drains. The CLI verifies the incarnation and waits for the lifetime lock to be
  released (`runtime: "stopped"`).
- If the lock is not released in time, leave exits with a timeout stating the
  departure **is** recorded and the stop is unconfirmed.
- Without a running node, leave opens the store directly
  (`runtime: "not_running"`). `Busy` reports a concurrent `join` and changes
  nothing.
- At shutdown the node releases the join store (`take()` under its mutex)
  before its lifetime lock, even if a control session still holds the shared
  state.

**Receipt fields.** `state`, `newly_left` (repeated leave is idempotent and
keeps the original time), `left_at`, `credentials: "erased"`, `runtime` and
`incarnation`, plus two explicit limits:
- `unmanaged_consumers: "unknown…"`: copies of the PSK held by other processes
  are not tracked;
- `authority: "not notified…"`: leaving is local; issuer-side revocation is
  separate.

**Restart and rejoin.**
- `up` on a left state refuses before any bind (exit 2, "left the mesh").
- `join <token>` on a left state refuses unless `--rejoin` is given.
- `join --rejoin` redeems from the issuer again with the same device identity.

Witnesses:
- SDK `leaving_erases_the_credentials_and_only_an_explicit_rejoin_recovers`:
  idempotent leave, a restart that stays left, redeem `Left`, the PSK absent
  from every file in the join state, and rejoin recovering the same committed
  PSK.
- CLI `cli/tests/enrollment_leave.rs` 3/3, all real subprocesses:
  - `leaving_stops_the_running_joined_node_and_restart_stays_left`: leave
    through the running node, `stopped` with its incarnation, `up` refused, an
    idempotent second leave, and the operator's node and ledger untouched;
  - `an_offline_leave_is_undone_only_by_an_explicit_rejoin`;
  - `leave_refuses_state_that_never_joined`: a fresh state and an operator node
    both refuse without effect.

Inverse mutations, each caught by its witness and then restored
byte-identically:
- redeem ignoring the departure;
- leave keeping the credentials;
- leave not persisted;
- `up` running a left join;
- `join` silently undoing the departure;
- the node recording leave but not stopping.

Regressions:
- `net-cli` 348/348.
- SDK enrollment 57/57.
- `enrollment_storage` units 7/7. The reopen witness was deflaked, see §1.
- Clippy (core all-features all-targets, SDK `full`, `net-cli`) and SDK rustdoc
  are clean.

**Not covered (stated, not implied).**
- Live stop of *unmanaged* processes that copied the PSK.
- Authority notification.
- PSK rotation to exclude a departed device. That is issuer-side removal
  (§6 / V3-4), not leave.

#### V3-2B relation coverage (2026-09-24)

| Relation | Where it landed |
|---|---|
| Mesh | Above |
| Org | O4 (`org leave`) |
| Channel | V3-2A C3 (`channel leave`, plus the whole `leave` withdrawing a subscription) |
| Subnet | This slice (user decision: withdrawal over the session) |

#### V3-2B subnet relation — `net-mesh subnet leave <scope>` (receipt, 2026-09-24)

**Core: self-withdrawal on the admission subprotocol (`0x0A02`).**
- New legs `Withdraw { nonce, authority, attachment }` and
  `Withdrawn { nonce, dropped }`.
- The verifier calls `SubnetContextStore::forget_if_at`. It drops the
  sender's context only when that context is exactly the named attachment
  under the named authority. It acknowledges either way; `dropped: false`
  means nothing was held there.
- The sender is the AEAD-resolved session peer, so a node can withdraw only
  itself. The withdrawal is advisory and revokes nothing.
- The prover side is `MeshNode::withdraw_own_subnet_admission` (retrying
  leg). With no session it returns `NoSession`. A verifier that predates the
  message gives `Timeout`.

**CLI.** `subnet leave <scope>` runs through the device's running `up`
(control op `subnet_leave`). It resolves the join's own subnet relation or a
standalone membership at that scope.

1. **Records durably first.**
   - Join relation: `<state>/subnet.left` is written under the join lock and
     the runtime's `subnet_left` flag is set.
   - Standalone membership: `SubnetMembership::leave`, which erases the
     credentials and already fences `install`.
2. **Fences the supervisor.**
   - A left relation is filtered out of every pass, so it is never presented
     or renewed again.
   - A renewal in flight is refused at persist, under the same lock.
   - A presentation that raced the leave is withdrawn again.
3. **Withdraws at the verifier.** `withdrawal` is `confirmed` only on the
   acknowledgement, otherwise `unconfirmed` with a detail.

The receipt also states:
- `credentials`: `disabled` (join relation) or `erased` (standalone);
- `credential_validity: unchanged`, because leave is not `subnet remove`.

`up` on a left relation reports `state: left` and uses nothing. The mesh
membership stays attached. Repeating is idempotent, and an unknown scope is
refused.

Witnesses:
- **Core:** `tests/subnet_wire_admission.rs`
  `a_device_withdraws_only_its_own_named_admission`, already pinned in CI:
  - another scope or authority drops nothing;
  - the named admission drops, and a sibling's is untouched;
  - a repeat is acknowledged as `false`;
  - with no session the result is `NoSession`.
- **CLI:** `cli/tests/subnet_join.rs`
  `a_device_leaves_its_subnet_relation_and_is_withdrawn_at_the_verifier`:
  - the leave is confirmed with `dropped`;
  - the operator's own observation (`subnet members …
    observed.admitted_here`) shows the device gone;
  - it is idempotent, and an unknown scope is refused;
  - a 15 s leaf passes its renewal point and `subnet_expires_at` is
    unchanged, with no re-admission;
  - after a restart it is still `left`, still attached, and still not
    admitted.

Inverse mutations, all RED (7):

| # | Mutation |
|---|---|
| 1 | Withdrawal ignores the named scope |
| 2 | Verifier ignores withdrawal (RED in both the core and CLI witnesses) |
| 3 | Disabled intent bypassed |
| 4 | Intent bypassed **and** a late renewal installs (task 5) |
| 5 | Leave not recorded |
| 6 | Start ignores the marker |
| 7 | Withdrawal skipped but reported confirmed |

Gates:
- fmt; core clippy, strict and all-targets; root rustdoc; CLI clippy
  `--all-targets`;
- `cargo tl` 5818/5818; `cargo t` 7040/7040; SDK 829/829; CLI 365/365.

**Limits:**
- The verifier keeps one admission context per peer. A device with the
  join's own relation and a standalone membership at the same verifier is
  admitted at only one of them at a time. This is pre-existing and is not
  changed here.
- The post-present re-withdraw covers the join relation. A standalone
  membership relies on `leave` fencing `install` and the supervisor
  filtering left memberships.
- Other verifiers the device never presented to hold nothing, so no
  withdrawal is needed there.

### V3-3 — live inspection without false completeness

**Modify:** shared enrollment SDK/service and CLI `enrollment.rs`, `org.rs`, `channel.rs`, `subnet.rs`; narrow runtime read interfaces where necessary.
**Proposed new test:** `cli/tests/enrollment_status.rs`.

Tasks:
1. Write RED controls distinguishing issuer inventory, credential validity, live admission/subscription/publish observation, expired observation and an unreachable node. Channel status must not infer a durable member roster from the live subscriber set.
2. Add read-only SDK queries and CLI views with exact responder/instance, subject, scope, observation time, known revision, completeness boundary and pending stages. Authorize protected inventory reads.
3. Prove `--inspect-target` has no service startup/network/storage side effects and normal execution consumes the same resolved target.
4. Exercise empty-but-reachable, unreachable, unauthorized, truncated/bounded inventory, stale receiver and a second independent admitted device. Redact all secret fields.

**Exit:** The user can answer “what was issued, what is currently admitted here, and what is unknown?” without source inspection. Never relabel old `subnet ls --local` output as live state.

#### V3-3 (first slice) — `org members` / `subnet members` (receipt, 2026-09-24)

Decision (user, 2026-09-24): **the issuer inventory plus this node's
observations**. Remote enforcement points come in a later slice.

These commands answer "what was issued, what is admitted here, and what is
unknown?" for one org or one subnet scope (with its subtree), from the node
of `--state-dir`:
- **`issued`** (when that node enrolls). Every offer it created whose
  recorded relation matches, with its ledger state (`offered` /
  `pending_approval` / `approved` / `issued` / `revoked_offer` / `denied`),
  subject, `issued_at`, relation and scope, and, for org offers, the
  generation `org approve` signed.
  - Relations are now recorded per offer at `invite create`
    (`<ledger>.org/relations/`), because the ledger keeps digests only.
  - Older offers are counted in `unrecorded_offers`, never guessed.
- **`observed`**, this node only:
  - **Subnet:** the peers admitted to the scope at this node right now,
    through a new core read `MeshNode::admitted_subnet_peers`. Each context
    is checked as `subnet_context_for` checks it, **and** the session must be
    live.
    - Found while writing the witness: a dead peer keeps its table entry,
      and its context, for 30 × `session_timeout`. Without the liveness
      filter a killed device would still be listed as admitted.
  - **Org:** each issued member's standing against this node's floors
    (`admissible_here` / `revoked_here`, with `floor_here`), only when this
    node enforces that org. Org admission is per call, so activity is
    reported as unknown, never implied.
- **`completeness`** states the boundary in words:
  - a node that does not enroll reports its issuer inventory as `unknown`;
  - other verifiers were not asked;
  - a member not connected here is absent, not removed.

**Witnesses.**
- CLI `subnet_join::subnet_members_separates_issued_from_admitted_here`:
  - a connected 3.7 device is issued and admitted;
  - a 3.8 device is issued there, and admitted nowhere;
  - the subtree view covers both;
  - stopping the device drops it from `admitted_here` (still `issued`);
  - a non-enrolling node reports `issued: unknown`.
- CLI `org_join::org_remove_…`, extended: before removal the device is
  `issued` with generation 0 and `admissible_here`; after `org remove` it is
  `revoked_here` with `floor_here` 1.

**Inverse mutations** (all RED):
- dead sessions still reported admitted;
- the admitted view ignoring the scope;
- the issued view ignoring the scope;
- standing ignoring floors;
- relations not recorded;
- the approved generation not recorded.

**Regressions.**
- `cargo tl` 5818/5818.
- `net-cli` 360/360.
- Clippy (core strict, CLI all targets) and root rustdoc are clean.

**Next in V3-3.** ~~Signed observations from named remote enforcement
points~~ (see the next receipt), and channel status.

#### V3-3 slice 2 — signed observations from named nodes (receipt, 2026-09-24)

Decision (user, 2026-09-24): **root-signed requests**. Only a holder of the
authority reads a node's inventory; the root stays on the operator's
machine.

**SDK `net_sdk::members`.**
- `MembersRequest` targets either a subnet scope (with its subtree) under an
  authority, or the standing of named members in an org. It names exactly
  one node, and carries a nonce and a ±300 s freshness window.
  - It is signed by a subnet root the node trusts for that authority, or by
    the org root itself (the org key as an `EntityKeypair` over the same
    seed, so it verifies under the org id).
- `answer_members` refuses a stale request, one naming another node, a bad
  signature, and a signer without that authority. It answers with a
  `MembersObservation` signed by the node's entity key over the exact
  request digest:
  - `observed`: for a subnet, the admitted peers in scope (live sessions
    only, through `admitted_subnet_peers`); for an org, each named member's
    floor there;
  - `not_verifier` / `not_member`: the node enforces nothing for that
    authority, which reveals nothing.
- Service `net.members.observe`, served by every `up`.

**CLI.**
- `subnet members <scope> --verifier self|CONTACT … --root-key K --authority
  A` and `org members <org> --verifier … --org-key K` sign one request per
  node on the operator's machine, and carry each through the operator's node
  (`members_forward`).
- The CLI verifies every observation against its own request and adds
  `remote` rows:
  - `observed` / `not_verifier` / `not_member` / `refused` / `no_answer` /
    `bad_observation`;
  - for subnets, the admitted peers; for orgs, each member's floor and
    standing (`revoked_there` / `admissible_there`, judged against the
    generation the operator approved).
- The completeness note: only the named nodes were asked, each at its own
  `observed_at`, and an unanswered node is unknown, not empty.

**Witnesses.**
- SDK `members_observe` (2 tests):
  - the subnet root reads, and a stranger, another node, a stale request or
    a tampered signature is refused;
  - another authority gets `not_verifier`;
  - the observation binds its request and its signature;
  - only the org root reads org standing; a non-member gets `not_member`, a
    member reports floors.
- CLI subnet: the operator's node observes the connected device in 3.7; the
  device's node is `not_verifier`; 3.8 is observed empty; the issuer key
  (not the root) is `refused`.
- CLI org, after `org remove`: the operator's node and the device's node are
  both `revoked_there` at floor 1; the bystander is `not_member`.

**Inverse mutations** (all RED):
- any signer reading a subnet (SDK and CLI);
- any signer reading an org;
- a request for another node answered;
- an observation not bound to its request;
- the remote view ignoring the scope;
- remote standing ignoring floors;
- the request signature unchecked.

**Regressions.**
- The SDK suite as CI runs it: 822/822.
- `net-cli` 360/360.
- SDK clippy (default, `full`, CI features on the new target), `net-cli`
  clippy and SDK rustdoc are clean.

**Next in V3-3.** Channel status: installed credential state vs live ACK /
publish-gate observation, never a roster. It builds on V3-2A (the channel
lifecycle), which has not started.

### V3-4 — selective removal with durable enforcement

**Modify:** subnet auth/admission/control owners, actual org floor persistence/application owners, SDK wrappers, CLI `org.rs` / `subnet.rs` / shared enrollment service.
**Proposed new test:** `cli/tests/enrollment_removal.rs`; core integration binary `tests/subnet_subject_revocation.rs` only if a distinct binary is warranted (then pin it in CI).

Tasks:
1. Write RED for B removed while C remains admitted in the same subnet, with B retaining all old credentials. Include active-session traffic and a genuine reconnect.
2. Implement the accepted subject-scoped mechanism, durable monotone application and cache/session invalidation. Cover issuer authority, stale revisions, alternate qualifying credentials and deliberate reissue semantics.
3. Add exact-target dry-run/confirmation and idempotent mutation receipts. Inventory update failures after durable revocation report that enforcement committed; never un-revoke for cosmetic consistency.
4. Add bounded propagation/readback through real protected management/status paths, not forged ACKs or a new anti-entropy framework. Exercise unavailable verifiers and offline recovery.
5. Prove independent organization removal, unaffected siblings/scopes, restart durability and old/new incarnation race handling. State actual consequences for active RPCs; do not introduce org streaming here or imply revocation retracts completed effects.
6. Run inverse mutations against production verifiers/persistence, including bypass of subject revocation and rollback on restart. Restore the candidate and verify no inverse edits remain.

**Exit:** A command can remove one subject from the named boundary and support its precise claim with real enforcement evidence. Partial propagation remains a first-class result, not false global success.

#### V3-4 slice 1 — subject floor mechanism (receipt, 2026-09-23)

This implements the §6.1a pinned design in core, SDK, the cross-language
fixtures and the CLI issuance verb.

**Core.**
- `SubnetSubjectFloor` artifact (126-byte signed payload plus signature,
  domain `net.subnet.subject-floor.v1`), travelling as control-fact kind 5
  `subject_floor`.
- `SubnetFloorRegistry::apply_subject` enforces: root signer only, a strictly
  higher revision per `(scope, epoch, subject)`, and per-right generations that
  only rise.
- Subject floors are checked at admission against the **target** in
  `verify_admission`; a `OneHop` leaf never satisfies a floor.
- `SubnetContextStore::invalidate_subject` drops only the removed subject's
  covered contexts; the authority epoch does not move.
- New `SubnetAuthError::StateNotPersisted`.

**Durability.**
- `MeshNodeConfig::with_subnet_floor_store(dir)` enables
  `subnet/floor_store.rs`: an append-only log, in acceptance order, of every
  floor fact (subtree and subject) that changed state, kept in
  `EnrollmentStorage`.
- Each fact is logged before its apply returns success.
- `MeshNode::new` replays the log through the root-anchored verifiers before
  any admission. A corrupt log, or a logged floor that no longer verifies,
  refuses construction.

**Cross-language.** Updated together in one commit:
- `stable_kinds.json`, regenerated (`fact_kinds` + `subject_floor`,
  `auth_kinds` + `state_not_persisted`);
- the SDK `render_stable_kind_fixture` and its kind test;
- the Node, Python and Go kind tests;
- the Go and Node doc comments.

**CLI.** `subnet issue-control-fact subject-floor --subject <64-hex>
[--rights attach] --minimum-generation N`:
- The default right is ATTACH; others apply only when named.
- `--minimum-generation 0` is refused.
- The receipt carries `enforcement: "pending: signed only; …"`. Issuing signs
  the artifact and removes nothing by itself.

Witnesses:
- `tests/subnet_subject_floor.rs` 4/4, pinned in ci.yml beside
  `subnet_session_auth`:
  - `b_is_removed_c_is_untouched_across_reconnect_and_restart` (the decisive
    witness):
    - B's live context drops, while C keeps the **identical** context and the
      authority epoch does not move;
    - B's old credentials are refused on its live session and on a real
      reconnect (a fresh node, same identity, new session);
    - after a verifier restart on the same store B is still refused and C is
      admitted;
    - a delegated leaf at generation 99 is refused;
    - a root-direct leaf at the floor re-admits B.
  - `an_ancestor_scoped_grant_cannot_defeat_the_removal`: refused inside and
    below S; works outside S.
  - `rights_are_exact_and_generations_never_lower`: an ATTACH floor leaves
    ROUTE; a stale revision is a no-op; a lower generation never lowers.
  - `only_a_root_signs_and_the_wire_kind_is_strict`: a non-root is
    `IssuerNotAuthorized`; the wire tag is 5; an unknown tag 6 is
    `InvalidFormat`.
- `floor_store` unit (round trip, tamper, oversize).
- CLI `subject_floor_issuance_is_exact_and_reports_enforcement_as_pending`.

Inverse mutations, 9 of 9 caught by their witnesses and then restored
byte-identically:
- admission ignoring subject floors;
- checking the grant scope instead of the target;
- delegated leaves satisfying the floor by generation;
- a subject floor moving the authority-wide epoch (sibling churn);
- floors not persisted;
- a later fact lowering a generation;
- stale revisions applying;
- the floor broadening to every right;
- a non-root signer accepted.

Regressions:
- `cargo tl` 5811/5811.
- All 14 CI-pinned subnet binaries 116/116, and `subnet_auth_e2e` 23/23.
- SDK subnet and fixture tests 8/8.
- Python `test_subnet_kinds.py` 5/5.
- `net-cli` 349/349.
- Clippy (core all-features all-targets, lib/bins all, default and
  no-default; SDK `full`; `net-cli`) and rustdoc (root, SDK `full`) are clean.
- Not run locally: the Node kind test (no vitest installed) and the Go kind
  test (this box cannot build cgo). CI runs both.

**Still open for V3-4.** Stated, not implied:
1. **Propagation readback.** No management path lets the CLI learn which
   verifiers applied a floor. A `subnet remove` verb with dry-run, applied and
   pending enforcement points, and an `unsupported` report for pre-kind-5
   verifiers needs that path. Until it exists the CLI reports issuance only.
2. **`up` is not a subnet verifier.** No CLI-run node configures a subnet
   authority or a floor store yet. The mechanism is proven in core; the CLI
   journey follows the subnet enrollment slice (V3-2).
3. **Organization removal** composes its existing floor separately and is not
   touched here.

#### V3-4 slice 2 — authenticated floor readback: design (2026-09-23)

The survey found no existing path that lets a verifier prove what floor it
applied:
- nothing reads the floor registry over the wire;
- control facts are applied with no acknowledgement;
- org-protected nRPC authenticates the caller, but its replies are unsigned and
  it cannot target one verifier;
- `session_peer` is the last hop, not the origin.

Readback is therefore two signed artifacts over ordinary nRPC (service
`net.subnet.floor`). This is no anti-entropy framework and no unsigned ACK.

- **`FloorStatusRequest`**, signed by an **authority root**:
  - fields: scope, topology epoch, subject, the **named verifier** entity, a
    fresh 16-byte nonce, `issued_at`, and optionally the subject-floor fact to
    apply first (it must name the same scope, epoch and subject);
  - the verifier answers only its own name, only a configured root, and only
    within ±300 s.
- **`FloorStatusAttestation`**, signed by the **verifier's own entity key**
  over the request digest. It carries:
  - the apply outcome (`not_requested`, `applied`, `unchanged`, or `refused`
    with a stable kind);
  - the subject's per-right generations and revision as that verifier now
    holds them;
  - `persisted`: whether its floor store is durable.

  Neither a relay nor a last hop can forge or replay one into a different
  request.
- **CLI result per named verifier:**
  - `applied`: attested, covers the floor, persisted;
  - `applied_not_persisted`: attested, covers the floor, but the verifier has
    no durable store;
  - `refused`: an attested refusal, e.g. `invalid_format` from a verifier that
    lacks kind 5;
  - `no_attestation`: unreachable, no service (pre-readback), timeout, or a bad
    attestation.

  `complete` is true only when every named verifier is `applied`. Verifiers
  that were not named are reported as not checked, never assumed.
- **Addressing.** Noise static keys are generated per start, so a verifier is
  named by a full contact, `ENTITY@HOST:PORT#NOISE_PUBKEY`, with the mesh PSK
  as in existing remote attach. `--dry-run` queries status without applying.

#### V3-4 slice 2 — receipt (2026-09-23)

**Core.**
- `subnet/floor_status.rs` defines `FloorStatusRequest` (root-signed, bound to
  one verifier, a nonce and `issued_at`, optionally carrying a subject floor
  for the same scope, epoch and subject) and `FloorStatusAttestation`
  (verifier-signed over the request digest).
- `MeshNode::answer_subnet_floor_status` refuses a request that is:
  - addressed to another verifier (`wrong_verifier`);
  - signed by a non-root (`issuer_not_authorized`);
  - stale or future-dated beyond ±300 s (`expired` / `not_yet_valid`).

  It applies a carried floor through the normal (persisting) path and signs
  what it now holds, including `persisted`.
- `serve_subnet_floor_status` (opt-in) serves service `net.subnet.floor`.
- `query_subnet_floor_status` accepts an answer only when `verify_for` passes.
  A missing service (`NotFound`) or no route is `NoAnswer`, never `Refused`.

**CLI.** `net-mesh subnet remove --root-key … --authority … --scope …
--topology-epoch … --revision … --subject … [--rights attach]
--minimum-generation N --verifier ENTITY@HOST:PORT#PUBKEY … [--psk-hex]
[--dry-run] [--wait]`:
- It signs the floor once and attaches to each named verifier over its own
  session.
- It reports each verifier's `state`: `applied`, `applied_not_persisted`,
  `not_applied`, `refused` (attested), `request_refused`, or
  `no_attestation`.
- It reports `applied` and `pending` counts, `complete` (true only when every
  named verifier attested a persisted floor; never on a dry run), and
  `coverage` ("only the named verifiers were checked").

Witnesses:
- Unit `floor_status` 2/2: a round trip; the attestation is bound to its exact
  request (another nonce gives `WrongChallenge`); a foreign signer gives
  `WrongVerifier`; tampering gives `InvalidSignature`; a request cannot carry
  another subject's floor.
- Core `readback_reports_each_verifier_separately_and_cannot_be_forged`:
  - durable → `Applied` and persisted;
  - volatile → `Applied`, not persisted;
  - older (no service) → `NoAnswer`;
  - stranger authority → `Refused(unknown_authority)`;
  - non-root → `Refused(issuer_not_authorized)`;
  - stale → `Refused(expired)`;
  - status-only → `NotRequested` with the held `(1, [2,0,0])`;
  - re-apply → `Unchanged`;
  - a request for durable sent to volatile → `Refused(wrong_verifier)`.
- CLI `cli/tests/subnet_remove.rs`, with a real subprocess against in-process
  verifiers:
  - a dry run is `not_requested` and never complete;
  - a removal gives durable `applied`, volatile `applied_not_persisted`, older
    `no_attestation`, 1 applied / 2 pending, not complete;
  - B's old grant is then refused at the durable verifier;
  - naming only the durable verifier re-runs as `unchanged` and complete.

Inverse mutations, 7 of 7 caught by their witnesses and then restored
byte-identically:
- the verifier answering any signer;
- the verifier answering a request for another verifier;
- the verifier answering stale requests;
- the verifier claiming persistence it lacks;
- the caller accepting an attestation for another request;
- the CLI counting a volatile floor as applied;
- the CLI reporting complete with verifiers pending.

Regressions:
- `cargo tl` 5813/5813.
- The 14 CI-pinned subnet binaries 117/117, and `subnet_auth_e2e` 23/23.
- The full SDK suite as CI runs it: 806/806.
- `net-cli` 350/350.
- Clippy (core all-features all-targets, lib/bins all, default and
  no-default; SDK `full`; `net-cli`) and rustdoc (root, SDK `full`) are clean.

**Exact-head CI:** full `CI` passed at `952b70190` (run 35858946215), including
the Node, Python and Go kind tests and the pinned `subnet_subject_floor`
binary. The subject-floor head `ba4aa1ccd` also passed (run 35854878363).

**Still open (V3-4).**
- `up` does not yet act as a subnet verifier: no CLI-run node configures an
  authority, floor store or readback service. Operators run verifiers through
  the SDK/core until subnet enrollment (V3-2) wires them.
- Discovering enforcement points: the caller names them. Gateway
  advertisements could seed that list later.
- Organization removal is unchanged.

### V3-5 — public journey, CI and release acceptance

**Modify:** `cli/README.md`, `cli/CHANGELOG.md`, `web/src/content/docs/reference/cli.md`, relevant enrollment/security/subnet docs and `.github/workflows/ci.yml`.
**Proposed new deliverables:** fixtures under `cli/tests/fixtures/enrollment/` and `cli/tests/enrollment_workflow.rs`.

Tasks:
1. Ship runnable operator/joiner/provider fixtures and public instructions with prerequisites, terminal ownership, mesh/org/channel/subnet join links, separate grants, inspection, voluntary leave, operator removal where supported, restart and cleanup. Demonstrate the distinction between channel credential installation, live subscribe/publish evidence, offline local departure and remote revocation.
2. Extend V2's same-runner two-node CLI journey. Add a third identity/participant for the selective-removal positive control; loopback proves multi-node/process behavior, not multiple physical computers.
3. Exercise native publisher/consumer behavior and the existing MCP route without adding generic RPC hosting. Match accepted calls to provider-side effects and rejected calls to actual gate decisions.
4. Add platform gates for Unix permissions, Windows secret storage/replacement, process lifetime and default/optional features. Pin new root integration binaries and required authority witnesses; CLI tests remain auto-discovered.
5. Publish only accepted syntax/guarantees. Record actual commands/results, exact commit, topology and remaining off-host evidence. Do not make off-host/NAT or optional-browser claims on loopback evidence.

**Exit:** Cumulative acceptance below is complete, exact-head required CI is green, and the public instructions work without manual credential surgery or repository-internal knowledge.

#### V3-5 evidence audit and slices (2026-09-24)

A read-only audit mapped E1–E26 to the witnesses that exist.
- **Covered:** E1, E6, E13 and E15. E15 needs an exact-head re-run.
- **Missing:** E16.
- **Partial:** every other row.

Gaps the audit found:
- `enrollment_workflow` / `enrollment_status` / `enrollment_removal` and
  the fixtures do not exist.
- No V3 command appears in `cli/README.md`, `cli/CHANGELOG.md` or the web
  CLI reference.
- The CLI had no channel publish verb.
- The two-node MCP journeys (`capability_workflow`) run on `--psk-hex`,
  not on an enrolled identity.
- No V3 CLI test asserts a platform permission property. The Windows job
  does run the whole CLI suite.
- There are no barrier races of join, renew or subscribe after leave, and
  no concurrent-redeem test.
- **Design deviation from E23, by user decision:** the channel offer is
  `{channel, root, rights}`. TTL is the grant's, depth is fixed, and the
  publisher is the issuer.

Slices:

| Slice | Content |
|---|---|
| **W1** | `channel publish` |
| **W2** | `cli/tests/enrollment_workflow.rs`: three participants on one runner (operator, B, C). Covers joins, a subscribe ACK, a gated publish, an enrolled-identity provider effect, removal of B while C keeps working, leave, restart and cleanup. |
| **W3** | Public docs and fixtures. |
| **W4** | Gap witnesses, prioritized by claim risk: E2 inspect leaves the ledger untouched; E21 corrupt/insecure PSK store; E22 an unrelated node stays untouched; E3 concurrent redeem. |

**W1 receipt: `channel publish <name> --data <text>` (2026-09-24).**
- It is a control op, `channel_publish`, on the running node. It performs
  one real `MeshNode::publish`, so the node's own production gate decides.
- The reply has three outcomes:
  - `gate: passed` on a gated channel;
  - `open: this node does not gate the channel` when no gated config
    exists here, so it is never presented as credential evidence;
  - an error carrying `gate: denied` and the gate's own reason (`publish
    denied by channel ACL`).
- `attempted` / `delivered` / `failed` are this node's sends, not
  subscriber receipts. The payload is capped at 16 KiB.
- When a joined device's managed chain is installed and the gate passed,
  the channel link records `published_at`. That is live-active evidence,
  distinct from `publish_ready`.

Witness: `channel_join::a_publishing_device_is_ready_only_under_its_own_trust_and_leaves_exactly`.
1. Ungated: `open`, and no `published_at`.
2. Stranger root served: a real gate denial.
3. Right root: `passed` and `published_at` set.
4. After `channel leave`: denied again, so no fallback credential remains.

Inverse mutations, all RED (3):

| # | Mutation |
|---|---|
| 1 | Ungated reported passed |
| 2 | Live-active recorded without the gate |
| 3 | Denial reported as success |

Gates: CLI clippy `--all-targets`; CLI 365/365.

**W2 receipt: the three-participant journey and `--joined` (2026-09-24).**

`wrap` and `mcp serve` gain `--joined <state-dir>` (user decision).
- **What it loads.** It opens the join store and holds it for the
  consumer's lifetime, then loads the enrolled device's identity, mesh PSK
  and contact. It attaches to the contact direct first, then the relay.
- **Refusals:**
  - `up` running on that state (the store is `in use`);
  - `--psk-hex`;
  - a partially named peer;
  - a state that never joined;
  - a device that left.
- **Peer override (user decision).** An explicit
  `--node-addr/--node-pubkey/--node-id` names another peer of the same mesh
  to attach to, still as the enrolled device with the enrolled PSK.
- **Found while building W2, now disclosed.** Two devices attached only to
  the operator cannot invoke each other's tools:
  - announcements flood through the hub only when they are sent, so a
    device that attaches after the provider announced never sees it;
  - nRPC needs a direct or relayed session to the provider, which neither
    `mcp serve` nor the gateway opens.

  Invoking across the hub remains future work. **Resolved by S6**: the hub
  replays held announcements to late attachers, and the gateway opens an
  endpoint-authenticated session (direct first, relay fallback) before
  calling. The peer override remains available but is no longer needed.

Witness `cli/tests/enrollment_workflow.rs` (auto-discovered): all real
subprocesses on loopback, `--no-port-mapping`.
- `three_participants_join_publish_are_selectively_removed_and_invoke_as_enrolled_devices`:
  1. **Offline ceremonies.** The subnet root and issuer grant, then the
     channel root (an operator identity) and a grant to the learned
     enrollment issuer. Neither root reaches the node.
  2. **Operator.** `up --enroll --subnet-issuer-grant … --channel-grant …`,
     then `channel serve`.
  3. **Joins.** B and C each join with one composed link (mesh + subnet 3.7
     + channel). Both `up`s are admitted to the subnet and ACKed on the
     channel.
  4. **Publish.** B's `channel publish` is `open` while ungated, denied at
     its gate under a stranger root, then `passed`.
  5. **Selective removal.** `subnet remove` of B with `--verifier self`
     completes. B's restart is refused admission (`revoked`) while C's is
     admitted, and the operator observes only C at 3.7.
  6. **Enrolled tool call.** B runs `wrap --joined` with `--allow` for C's
     origin. C runs `mcp serve --joined` attached to B. C discovers the
     tool. The consent gate refuses with no provider effect. After a pin
     approval exactly one call leaves exactly one provider-side record.
  7. **Leave and cleanup.** C `leave`s with `channel_unsubscribed: true`.
     C's next `up` is refused, and the operator goes `down`.
- `joined_consumers_refuse_what_they_cannot_honour`: the five refusals
  above.

Inverse mutations, all RED (4):

| # | Mutation |
|---|---|
| 1 | Ephemeral identity instead of the enrolled one (the provider's owner scope refuses) |
| 2 | A left join is not refused |
| 3 | Peer override ignored |
| 4 | `--psk-hex` accepted |

Gates: CLI clippy `--all-targets`; CLI 367/367. The journey runs in ~44 s.

E16 coverage from this receipt:
- an acknowledged full-chain subscription;
- a caller-requested publish accepted by the local production gate;
- actual gate denials: the channel ACL, the consent gate and subnet
  revocation;
- a provider-side accepted effect under enrolled identities;
- selective removal, restart and cleanup.

Subscriber receipt of the published payload is not requested by this
fixture.

**W3 receipt: public instructions (2026-09-24).**
- **`cli/tests/fixtures/enrollment/README.md`.** The
  operator/joiner/provider walkthrough, in the harness's own command order.
  It covers:
  - terminal ownership;
  - offline authority;
  - the operator node;
  - composed join links;
  - publishing through the device's own gate;
  - selective removal;
  - the enrolled tool call;
  - leave, restart and cleanup.

  It spells out credential installation versus live
  subscribe/publish evidence, local leave versus revocation, the hub
  invocation limit, and loopback-only evidence.
- **`cli/README.md`.** A "Managed nodes, join links and leave" section,
  with the command-table rows added or updated (`up`/`down`, `invite`,
  `join`/`leave`, `enrollment`, `relay`, `org`, `node`, `subnet`,
  `channel`, `wrap`/`mcp --joined`).
- **`cli/CHANGELOG.md`.** An "Unreleased — managed nodes, join links,
  relations and leave" entry.
- **Web CLI reference.** A matching section and command table.
- **Top-level help.** The stale help strings for `org`, `node`, `subnet`
  and `channel` now describe their current verbs.

Every flag named in these docs was checked against the binary's `--help`.
Checks:
- `npm run check` (web): 180 docs, links, release sync and types all pass;
- CLI `readme_commands` + `help` + `help_is_self_contained`: 10/10.

**W4 receipt: gap witnesses (2026-09-24).**

- **E2** — `enrollment_workflow::inspection_leaves_the_ledger_untouched_and_the_link_redeemable`:
  - two rounds of `invite inspect` and `invite status` leave the ledger
    snapshot byte-for-byte unchanged;
  - the same link then redeems;
  - the redemption does change the ledger, which shows the comparison is
    sensitive.
- **E3** — `sdk/enrollment_redeem::concurrent_redeemers_of_one_link_bind_exactly_one_identity`:
  six devices race one bearer link, released together by a barrier right
  before redemption. Exactly one is issued; the rest get `Conflict`. There
  is one issuance, and the ledger names the winner.
- **E21** — `node_lifecycle::a_corrupt_node_store_refuses_rather_than_regenerating_and_a_live_node_never_rotates`:
  - a second `up` with another PSK source while the node is live is
    refused, and the live trust domain is unchanged;
  - a corrupted node store refuses to start (bounded), and nothing is
    written over it;
  - the original bytes restored start the same trust domain.
- **E21 on Unix** — `insecure_node_state_and_psk_files_are_refused_on_unix`:
  a 0644 node snapshot and a 0644 `--psk-from file:` are refused, the
  latter before any state exists. It is `#[cfg(unix)]`, so it runs in the
  Linux CLI job; the Windows job cannot compile it.
- **E22** — `node_lifecycle::down_stops_only_its_own_node`: `down` of A
  leaves B running with the same incarnation.

Inverse mutations:

| # | Mutation | Result |
|---|---|---|
| 1 | Node-store checksum ignored | RED: the bounded start panics "up started over a corrupt node store" |
| 2 | Only the claim/issue conflict checks disabled | GREEN: a racer arriving after issuance is refused by the later state's check |
| 2b | All six claimant-binding checks disabled | RED: a second device "recovers" the winner's bundle, and `a_second_device_cannot_redeem_a_claimed_bearer_link` fails too |

- The E2 comparison carries no separate mutation. Its sensitivity is shown
  inside the test.
- Gates: CLI clippy `--all-targets`; SDK clippy for the CI feature test
  target; `node_lifecycle` + `enrollment_workflow` 11/11.

#### V3 decisions of 2026-09-25 (user) and the resulting slices

Decisions:
1. **Device-to-device tool calls.** Implement discovery replay and relayed
   session establishment.
   - Replay only currently valid announcements the new peer may see.
     Origin authentication, expiry and withdrawal are preserved, and replay
     never refreshes stale authority.
   - Session establishment lives in the reusable SDK path, and MCP consumes
     it.
   - Direct first, relay as fallback: "no session" does not prove that
     direct is impossible.
   - The session through the hub is endpoint-authenticated caller to
     provider. Nothing is ever invoked under the operator's identity.
   - Witness: B publishes before C attaches. C then discovers B and calls
     it with its own authority, while an unauthorized C stays denied.
2. **Standalone channel enrollment** (`channel invite` / `channel join` for
   an already-connected device). It uses the shared ledger and the existing
   identity. Adding a channel, or rejoining after a leave, never repeats
   mesh enrollment, and an explicit rejoin fences delayed work from the
   previous incarnation.
3. **Leave stays local.** There is no issuer-notification queue. Real
   protocol cleanup (unsubscribe, verifier withdrawal) stays. The completed
   local departure and unconfirmed remote cleanup are separate states. E18
   is amended.
4. **The channel link stays `{channel, root, rights}`.**
   - The redemption expiry and the credential expiry are distinguished and
     both shown.
   - The effective lifetime and delegation limits are shown.
   - Unsupported lifetime, depth or publisher overrides are refused.
   - Every canonical-identity, root, subject and rights check stays.
   - E23 is amended.
5. **`--detach` is deferred** (OS supervision instead). E20 keeps its
   readiness, duplicate-start, exact-incarnation, startup-failure-cleanup
   and stale-metadata requirements.
6. **`kms:` is deferred.** Only protected `file:`, `stdin` and the generated
   default exist. Unsupported schemes fail before bind. E21 is amended.
7. **Ledger crash injection.** Deterministic fault hooks sit at the real
   durable-write transitions, and the test stops a subprocess and restarts
   it against the resulting disk state.
   - Cover issuance commit, receipt recovery and profile publication.
   - If the hooks are gated behind `fixtures`, the owning CI job enables
     that feature and requires nonzero witness discovery.
8. **One active subnet attachment per peer and verifier in V3,** made
   deterministic:
   - supervisors never flip one admission with another;
   - a conflicting activation refuses unless a switch is explicitly
     requested;
   - status separates stored membership from active attachment;
   - leaving an inactive relation never withdraws the active one;
   - no multi-attachment redesign.
9. **Infrastructure.**
   - R2 phase 5 comes next: relayed → direct, keeping identity and
     authority, with no duplicated operations, and a failed upgrade keeps
     the relay.
   - TCP/443 fallback follows as its own bounded slice. It is not
     advertised as traversing every proxy.
   - The default relay is project-operated, keeping the self-hosted
     override and `--no-relay`. `DEFAULT_RELAY` stays empty until a real
     endpoint is deployed and externally verified; hosting spend is a
     separate operational authorization.

**Standing rule:** no security or recovery witness is weakened to finish
the matrix. The optional features are narrowed; the pairing lifecycle is
completed.

Slices, in order:

| Slice | Item | Content |
|---|---|---|
| **S1** | 4 | Channel-link lifetime display and override refusal (E23) |
| **S2** | 8 | Deterministic single active subnet attachment |
| **S3** | 2 | Standalone channel invite/join with incarnation fencing |
| **S4** | 3 | Leave witnesses (E17 / E18 / E19 / E26, including stop-unconfirmed) |
| **S5** | 7 | Crash-injection hooks |
| **S6** | 1 | Device-to-device discovery replay and relayed sessions |
| **S7** | 9 | R2 phase 5 |
| **S8** | 9 | TCP/443 |

Then the remaining test-only rows: E4, E5, E9, E11, E12 and E25.

**S1 receipt: E23 (2026-09-25).**
- **`invite create --channel`** reports, next to the invitation's own
  redemption `expires_at`:
  - `channel.credential_expires_at`: the grant's `not_after`, which every
    device leaf inherits;
  - `channel.delegation: none…`;
  - `channel.publisher`: this node for a subscribe right, "none: publish is
    local" for publish.
- **`join`** reports the delivered leaf's own `credential_expires_at` and
  the delegation limit.
- **Overrides.**
  - The CLI has no lifetime, depth or publisher override flags, so each is
    refused as an unknown argument.
  - The `invite_create` control op refuses `channel_ttl`,
    `channel_depth` and `channel_publisher` rather than ignoring them. This
    is defense in depth: the CLI never sends them, so no subprocess witness
    reaches it.
- **Witness:** `channel_join::a_device_joins_with_a_channel_credential_minted_from_an_offline_grant`:
  - the created and joined credential expiry each equal the grant's
    `not_after`;
  - the credential expiry differs from the redemption expiry;
  - the delegation limit and publisher are shown;
  - the three override flags are refused.
- **Inverse mutations, RED (2):** the create-side expiry replaced, and the
  join-side expiry dropped.
- **Gates:** CLI clippy `--all-targets`; CLI 370/370.

**S2 receipt: one active subnet attachment per verifier, deterministic (2026-09-25).**

A new module, `cli/src/commands/subnet_active.rs`, keeps a durable record,
`<state>/subnet.active.json` (verifier node → scope). A corrupt record
fails `up` closed.

How a relation becomes active:
- It takes its verifier's slot by itself only when the slot is vacant. At
  start, the join's own relation claims a vacant slot first.
- Replacing an active attachment is always explicit:
  - `subnet join --switch` refuses without the flag, before redemption, so
    the link is not consumed;
  - `subnet activate <scope>` records the new choice first, then withdraws
    the previous attachment (`previous_withdrawal`), then reports the
    supervisor's verdict.

The supervisor:
- presents only the active relation. Stored ones are renewed but never
  presented, and are reported `active: false` with no admission claimed.
- withdraws again any presentation that raced a switch away.
- never switches when an approval-gated membership completes: it stays
  stored while another attachment is active.

Leave: `subnet leave` of a stored relation withdraws nothing
(`was_active: false`, `withdrawal: not_active`). Leaving the active one
withdraws it and vacates the slot.

Witness: `subnet_join::a_joined_device_joins_another_subnet_with_a_standalone_link`,
rewritten. The old version asserted 3.7 and 3.8 both admitted, the flipping
false claim this decision removes. It now proves:
1. A non-switch join is refused.
2. `--switch` makes 3.8 active, 3.7 is withdrawn, and the operator's own
   observation shows exactly `[3.8]`.
3. After a restart only 3.8 is presented, and three samples over ~4.5 s
   show no flipping.
4. `subnet activate 3.7` switches back, confirmed by the operator's
   observation, and a repeat is a no-op.
5. A stolen link is still refused, and so is a foreign issuer.
6. An approved gated 3.9 completes stored.
7. Leaving 3.9 leaves 3.7 attached.

Inverse mutations, all RED (5):

| # | Mutation |
|---|---|
| 1 | A stored standalone relation presented anyway |
| 2 | The join relation presented while stored |
| 3 | A conflicting join not refused |
| 4 | Leaving a stored relation withdraws |
| 5 | A switch leaves the previous attachment standing |

Limits:
- Scopes are keyed by path per verifier; one authority per verifier is
  assumed.
- A `--switch` given with an approval-gated link is not kept for its later
  completion. The completion stays stored, and `subnet activate` finishes
  the switch.

Gates: CLI clippy `--all-targets`; CLI 370/370.

**S3 receipt: standalone channel enrollment (2026-09-25).**

SDK:
- **The link.** `Relation::Channel` may stand alone
  (`is_standalone_channel`): a channel link for a device already on the
  mesh. Any other pairing without mesh membership is refused.
- **The standalone redemption service** (`net.enroll.standalone.redeem`,
  now taking the node's channel grants) answers these links with
  `SubnetRedeemReply::ChannelIssued`:
  - the chain goes only to the entity the delivering session proved, for
    exactly the offer, through the ledger's claim, approval and issue;
  - a retry by the same device gets **the committed chain back from the
    ledger** (byte-identical, never re-minted; E24);
  - another device gets `Conflict`;
  - a node with no grant for the channel answers `Unavailable`.
- **The device store.** A new `ChannelMembership` (`NMCM`, owner-locked,
  checksummed) holds each link's chain. It installs only the offered chain
  for this device, and once left it installs nothing again: a delayed
  delivery cannot reinstate a departed incarnation.

CLI:
- `channel invite <name> --rights …` creates the standalone link (relations
  `[channel]`).
- `channel join <token> --yes` redeems it over the session with the enrolled
  node, installs it in `<state>/channels/<key>`, and starts using it:
  subscribe per session, and install the publish chain.
- **One active credential per channel.** A second is refused until the
  active one is left.
- **A rejoin is a fresh link, a new membership and a new incarnation.** The
  spent link is refused with "left that channel membership". Supervisor
  tracks are per incarnation, and the publish chain is removed exactly by
  fingerprint, so delayed work from the old incarnation never lands on the
  new one.
- **The runtime holds a list of channel credentials:** the join's own plus
  standalone memberships.
  - `up` reloads standalone memberships from disk and fails closed on a
    corrupt one.
  - `channel leave [<name>]` targets the named or only active credential
    and records through its own durable owner (the join marker, or the
    membership store); a repeat is idempotent.
  - A whole `leave` also leaves the channel memberships and withdraws every
    subscription.
  - `channel status` reports `joined` plus `standalone`.
- **Limit:** an approval-gated channel link completes when `channel join` is
  run again after `invite approve`; the supervisor does not poll for it.

Witnesses:
- `sdk/enrollment_channel::a_standalone_channel_link_issues_one_chain_and_recovers_it_exactly`.
- `cli/channel_join::a_joined_device_adds_leaves_and_rejoins_a_channel_with_standalone_links`,
  in order:
  1. A mesh-only device adds the channel, and `--yes` is required.
  2. The link inspects as `[channel]`.
  3. The device is ACKed.
  4. A second active credential is refused.
  5. After a restart it resubscribes by itself.
  6. Leave is acknowledged.
  7. The spent link is refused.
  8. A fresh link rejoins.
  9. Status shows one left and one active.
  10. Leave targets the new incarnation, and a repeat is idempotent.
  11. The mesh relation stays attached.
- The earlier relation witness was updated: `[channel]` alone is now the
  standalone form, and `[subnet, channel]` is refused.

Inverse mutations, all RED (5):

| # | Mutation |
|---|---|
| 1 | Recovery re-mints |
| 2 | Install not fenced after leave |
| 3 | A second active credential allowed |
| 4 | Memberships not loaded at start |
| 5 | A standalone leave not recorded |

Gates:
- SDK clippy (`full` and default) and SDK rustdoc `full`;
- CLI clippy `--all-targets`;
- SDK 831/831; CLI 371/371.

Also fixed: `enrollment_lifecycle`'s `free_port`. It tried only 50
TCP-first ports, which ran dry on the Windows runner ("no free port").
It now alternates UDP-first and TCP-first up to 1000 times. Its remaining
pick-then-bind window is disclosed; the Linux "Address already in use" flake
came from it.

**S4 receipt: leave contract, E17/E18/E19/E26 (2026-09-25).**

**Offline relation leave.** `subnet leave <scope>` and
`channel leave [<name>]` also work with no node running. The CLI probes the
lifetime lock; a held lock goes through the running node as before.
- **The departure is recorded durably by the owning store:**
  - the join relation's marker (`subnet.left` / `channel.left`), or
  - the standalone membership store (`SubnetMembership` /
    `ChannelMembership::leave`, which erases and fences).

  Subnet leave also releases the active-attachment record when that
  relation was active.
- **Local departure and remote cleanup are separate fields:**
  - `runtime: not_running` with `newly_left`;
  - subnet `withdrawal: unconfirmed…` (was active) or `not_active`;
  - channel `unsubscribed: false`, with `unsubscribe_detail: unconfirmed…`;
  - `publish_stop: confirmed`, because no runtime holds the credential.
- **It is idempotent.** A left join, or a state that never joined, is
  refused.

**Stop-unconfirmed (E17/E26).** The in-source unit test
`channel_link::tests::publish_stop_is_unconfirmed_while_another_publish_source_remains`
runs against a real SDK mesh:
- with a direct root PUBLISH token cached next to the managed chain, leave
  removes exactly the chain but reports `publish_stop: unconfirmed`;
- without it, the stop is `confirmed`.

Witnesses:
- `subnet_join::an_offline_subnet_leave_completes_locally_and_restart_honours_it`:
  1. With the node stopped, the offline leave is recorded, `was_active`,
     with the withdrawal unconfirmed.
  2. A repeat is idempotent.
  3. The next `up` reports `left` and stays attached.
  4. The operator observes no admission.
- `channel_join::an_offline_channel_leave_completes_locally_and_restart_honours_it`:
  the same shape for a subscribe credential, with the unsubscribe
  unconfirmed. The next `up` does not subscribe.
- The earlier E19 witnesses stand: the subnet late-renewal fence (a
  mutation plus the 15 s leaf) and the channel membership install fence.

Inverse mutations, all RED (4):

| # | Mutation |
|---|---|
| 1 | Another publish source ignored |
| 2 | Offline subnet leave not recorded |
| 3 | Offline channel leave not recorded |
| 4 | Offline withdrawal claimed confirmed |

Gates: CLI clippy `--all-targets`; CLI 374/374.

**S5 receipt: crash injection at the real durable-write transitions (2026-09-25).**

The hook:
- **Location.** `EnrollmentStorage::replace_using` in the core, compiled
  only with `net/fixtures`. It is exposed as `net-cli`'s test-only
  `fixtures` feature, never enabled in a shipped build.
- **Arming.** `NET_MESH_FIXTURE_CRASH=<before_replace|after_replace>:<store>:<nth>`
  aborts the process at the nth transition of the store whose directory is
  named `<store>` (`ledger`, `join`, `node`).
  - `before_replace`: nothing of that write is on disk.
  - `after_replace`: the snapshot is durable and nothing after it ran.
- **Tracing.** `NET_MESH_FIXTURE_TRACE` prints every transition. The
  ordinals below were read off it: an operator's ledger does a startup
  write, then offer, claim and issue (issue = 4th); the device's join does
  its intent, then the bundle install (2nd).

`cli/tests/enrollment_crash.rs` (`#![cfg(feature = "fixtures")]`, real
subprocesses restarted against the resulting disk state):

| Test | Crash | Proves |
|---|---|---|
| `an_operator_crash_after_the_issuance_commit_is_recovered_not_reissued` | The operator aborts right after the issuance commit, before replying | After restart there is exactly one issued record. The device's retry joins with the committed receipt: recovered, no second issuance. |
| `an_operator_crash_before_the_issuance_commit_issues_exactly_once_on_retry` | The operator aborts before the issuance write | Nothing is issued. A second device cannot take the committed claim. The first device's retry is issued once. |
| `a_device_crash_after_installing_its_bundle_resumes_without_redeeming_again` | The device aborts right after its join store installed the bundle, before attach or output | Re-running `join` resumes from the installed state with the same identity, `enroll_path: null` (no redemption ran), and still one issuance. |

The third test is the "profile publication" transition: in V3 the profile
is the protected join store.

**CI.** The "Net CLI tests" job gains a `--features fixtures --test
enrollment_crash` step that pins all three by name. The default runs
compile the binary to zero tests, so without this step it would be
silently skipped.

Inverse mutations, all RED (3):

| # | Mutation |
|---|---|
| 1 | The crash point never fires |
| 2 | A committed issuance is not recoverable |
| 3 | An installed join redeems again |

Gates:
- core clippy `--all-features`, strict, and root rustdoc;
- CLI clippy, default and `--features fixtures`, all-targets;
- `enrollment_storage` units 7/7; CLI 374/374.

**Limit:** the crash-before-issue case recovers because the committed
claim resumes. Crash points inside `write_atomic_phased`, between the temp
write and the rename, are covered by the existing storage phase unit tests,
not by a subprocess.

**Still open against the matrix (disclosed, not claimed):**

| Row | Open item |
|---|---|
| E3 | Crash-point injection at the ledger (no seam exists) |
| E4 | Explicit join-time denials of INVOKE / DELEGATE / dispatch / ROUTE / EXPORT, beyond the positive controls and channel refusals |
| E5 | A four-relation link (the journey composes mesh+subnet+channel) and standalone channel links |
| E7 | CLI fault injection for partial issuance and profile interruption |
| E9 | `no_answer` / stale observation rows in a CLI test |
| E11 | The removal-path delayed-old-incarnation race |
| E12 | The export-path witness |
| E17 / E26 | The `publish_stop: unconfirmed` branch |
| E18 | A "notification pending" state: leave notifies no one, by design |
| E19 | Barrier-driven join/renew/subscribe-after-leave races. The subnet-leave late-renewal fence is covered by mutation only. |
| E20 | `--detach`, which does not exist |
| E21 | `kms:` adapters, which do not exist |
| E23 | The offer shape is `{channel, root, rights}` by user decision |
| E24 | Channel-specific crash recovery; the generic ledger recovery covers it |
| E25 | Expired and revoked chains at the gates, in a CLI test |

**S6 receipt: device-to-device calls through the hub (decision 1, 2026-09-25).**

Two gaps blocked the call, and fixing them exposed a third.

1. **Discovery replay (core `MeshNode`).**
   - What is cached: each forwarder keeps the latest *signature-verified*
     announcement of every other origin, in its forwarded form, keyed by
     origin and superseded only by a higher version. A withdrawal is a newer
     version without the capability, so it supersedes too.
   - When it is replayed: to every newly established session. That covers
     `connect`, `accept` and the routed-handshake responder, after short
     settle delays so the peer has installed its session first.
   - What is filtered out: anything already expired by the origin's own
     signed `timestamp_ns`, and the peer's own announcement.
   - Authentication: the bytes are exactly as forwarded, so the origin's
     signature is what authenticates them. Replay adds no authority.
2. **No lifetime refresh.** A receiver now bounds a forwarded announcement
   (`hop_count > 0`) by its origin's timestamp. `effective_ttl_secs` is
   `ttl - max(0, age - 30s skew)`, so neither forwarding nor replay can
   extend stale authority. A directly received announcement keeps its full
   TTL, as before.
3. **Session establishment in the SDK.**
   - API: `MeshNode::ensure_session`, exposed as `Mesh::ensure_session`,
     returns `SessionPath::{Existing, Direct, Relayed}`.
   - Key source: the target's Noise static key comes from its own signed
     announcement. It is opt-in via `MeshNodeConfig::announce_noise_key`,
     off by default because older peers verify a different transcript.
     `up` and `--joined` turn it on.
   - Order: a direct attempt to the announced reflex address with
     `min(budget/2, 3s)`, then a routed handshake through the hop the
     forwarded announcement installed.
   - Authentication: the Noise handshake authenticates the provider's key
     end to end. The hub relays opaque frames, and nothing runs under the
     operator's identity.
   - MCP consumer: the gateway's `call_once` calls `ensure_session` with at
     most half the call timeout, capped at 5s, before invoking.
4. **Relay transit (root cause found while witnessing 3).**
   - The bug: every per-peer sender addressed `peer.addr()`. For a routed
     session that is the relay's address, and the relay has no session for
     frames addressed to it, so it dropped them. The handshake worked; the
     first RPC did not.
   - The fix: `PeerTransport::Routed` gains `transit`. The responder sets
     it from the arriving `routing_header.hop_count > 0`; the initiator
     takes it from msg2's hop count, carried out of the handshake with the
     keys. `PeerInfo::wire_route` yields a `WireRoute { addr, header }`
     that frames a packet in a `RoutingHeader` (dest, src, TTL 8) only for
     a transit session.
   - Sites that use it:
     - subprotocol send;
     - membership ACK and identity-proof frames;
     - control chunks;
     - the grant drainer;
     - retransmit and NACK resend;
     - `send_to_peer_node`;
     - the stream publish path.
   - Heartbeats are not wrapped: they stay hop-local, per the approved
     design.
   - Unchanged: a direct session, or a routed session the relay terminates
     itself, keeps the bare framing.
5. **Removed as redundant.** An extra identity proof over the new session
   was dropped: its mutation (skipping it) stayed GREEN, because the Noise
   handshake against the announced key already binds the endpoint.

Witnesses (`tests/capability_multihop.rs`, already pinned):

| Test | Proves |
|---|---|
| `a_late_attacher_discovers_and_reaches_a_provider_through_its_hub` | B announces before C attaches; C still discovers B through the hub's replay |
| `forwarding_or_replay_never_refreshes_an_announcement_lifetime` | A forwarded or replayed announcement expires on its origin's clock |
| `devices_attached_to_a_hub_by_routed_handshakes_reach_each_other` | Two hub-attached devices open a relayed session to each other |
| `a_call_to_a_provider_crosses_the_hub_over_the_relayed_session` | An nRPC call and its reply cross a relayed session |
| `a_call_crosses_a_hub_the_endpoints_are_directly_attached_to` | The same, when both endpoints hold direct sessions only to the hub |
| `membership_crosses_a_two_hop_relayed_session` | Membership and control frames cross the relay (the grant drainer and ACK sites) |

CLI journey (`cli/tests/enrollment_workflow.rs`, `three_participants`):
- C's `mcp serve --joined` attaches only to the operator, with no peer
  override, and calls B's tool.
- Device D is enrolled mesh-only and is not in B's allow list:
  - B's tool does not appear in D's `net_search_capabilities`;
  - after a local pin, invoking the known `cap_id` is refused ("Denied"),
    with no provider record.

Inverse mutations, all RED (6):

| # | Mutation |
|---|---|
| Z1 | No replay to new sessions |
| Z2 | A forwarded announcement's lifetime is refreshed |
| Z3 | Per-peer sends skip the routing header |
| Z5 | The gateway does not open a session (journey) |
| Z6 | The responder ignores the arriving hop count |
| Z7 | The noise key is never announced |

Z4 (skip the extra identity proof) stayed GREEN, so the proof was removed
(item 5).

**Compatibility.** `announce_noise_key` defaults to off, so the default
announcement is byte-identical to before; the `rtc_signalling` wire-compat
test is unchanged. `transit` affects only sessions established through a
relay hop.

**Limit.** The direct attempt needs `nat-traversal`, where the reflex
address is announced. Without it `ensure_session` goes straight to the
relay.

Gates:
- core clippy strict (all, default, no-default features) and all-targets;
- root and SDK rustdoc `-D warnings`;
- SDK, MCP and CLI clippy;
- `cargo tl` 5818/5818 and `cargo t` 7046/7046;
- SDK 831/831, MCP adapter 275/275 (`dependency_boundary` included), CLI
  374/374 (journey included); `capability_multihop` 13/13.


**S7 receipt: R2 phase 5, relayed → direct (decision 9, 2026-09-25).**

**What already existed and what did not.** The core's background direct-path
upgrade (`auto_direct_upgrade`, on by default) already treats a blind-relay
channel as a relayed peer. When driven by hand, a probe moved a
blind-relayed session to direct.

In the CLI topology it could never fire:
- An enrolled device whose only peer is its operator cannot classify its NAT
  (a sweep needs two observers), so neither end announces a reflex address
  and the upgrade has nothing to dial.
- C1 lets only the lower node id initiate, so a device with the higher id
  could not dial even an address it knew.
- A session installed by a routed handshake never pushed the node's own
  announcement, so the device waited for the ~150 s re-announce to learn
  anything about the operator.

**User decisions (2026-09-25):**
1. **Announced direct hint.** An enrollment node announces the address its
   tokens name.
2. **Deterministic initiator swap.** The higher id initiates when only the
   lower one announced an address.

**Core (`MeshNode`):**
- **`set_direct_hint(Option<SocketAddr>)`** (nat-traversal) stores the
  hint and republishes the current baseline.
  - The hint rides the existing signed reflex field only while no reflex was
    observed or overridden (`announced_reflex`), so no wire field is added.
  - It never changes the NAT class: it claims reachability at one address,
    not openness.
  - A wrong hint fails a Noise handshake authenticated against the
    operator's key, and the relay keeps serving.
  - Republishing with no baseline publishes the same (empty) set the
    re-announce loop already publishes on every started node; it only
    arrives earlier.
- **`upgrade_initiates_for`** makes C1 total, and the loop's candidate
  filter uses it:
  - The higher id claims the upgrade exactly when it announced no address,
    the lower id announced one, and the pair action is `Direct`.
  - Otherwise the lower id initiates, as before.
  - Both ends evaluate the same claim; the pair matrix is symmetric. They
    can disagree only while an announcement is in flight, and the C2
    compare-and-swap install settles that race.
- **`spawn_routed_announcement_push`**: both ends of a routed or relayed
  session (`connect_via_endpoint`, and the routed-handshake responder after
  commit) push their own announcement after the replay settle delay, as
  `connect`/`accept` do on a direct session.
  - The task holds only a `Weak` across the settle. A first version held a
    strong `Arc`, which kept a dropped node alive for 250 ms.
    `subnet_subject_floor`'s in-process restart caught it: the store was
    still owned. That test is the witness for this contract.

**CLI.** `up --enroll` calls `set_direct_hint` with the UDP address matching
its default token endpoint (`--public-addr`, the full router mapping, or a
concrete bind).

**Unchanged:**
- the upgrade's busy gate (C3: open streams or unacked data defer the
  swap);
- the CAS install;
- the relay-kept-on-failure contract.

The existing `direct_upgrade` mesh-relay tests pass as before.

Witnesses (`tests/direct_upgrade.rs`, already pinned under the "fixtures
port-mapping" CI step). Each uses a real `BlindRelay`, an operator
registered with it, and a device attached only through it, with no other
peers, so neither end can classify its NAT:

| Test | Proves |
|---|---|
| `a_blind_relayed_device_upgrades_when_its_operator_is_the_lower_id` | The device learns the operator's hint over the relay. Only the device initiates, even as the higher id. The background loop moves the session to the operator's address. The same entity id holds, and the operator never dialled. The subscription granted over the relay stays in the roster once, with no re-subscribe. A publish is delivered once and the relay forwards nothing afterwards. |
| `a_blind_relayed_device_upgrades_when_it_is_the_lower_id` | The same, with the device as the lower id (classic C1) |
| `a_failed_upgrade_keeps_the_blind_relayed_session` | The hint does not answer. The attempt fails with backoff, the same session id stays relayed, and delivery still rides the relay. |

Inverse mutations, all RED (5):

| # | Mutation |
|---|---|
| M1 | Strict C1: the higher id never claims |
| M2 | The lower id ignores the claim, so both dial |
| M3 | The hint is never announced |
| M4 | No own-announcement push on a routed session |
| M5 | A failed upgrade drops the relayed session |

Gates:
- core clippy (all, default and no-default features; strict and all-targets);
- root rustdoc and CLI clippy;
- `direct_upgrade` 19/19 with CI's features;
- `cargo tl` 5818/5818 and `cargo t` 7049/7049;
- SDK 831/831 and CLI 374/374.

**CI.** Since S5, the "Net CLI tests" job overran its 20-minute limit: two
~9-minute net-cli runs, then the crash step's own `fixtures` build. That
cancelled the S5 and S6 heads. The limit is now 35 minutes.

**Limits:**
- **Which end becomes direct.** Only the initiator's end is recorded as a
  direct adjacency. The responder's end is installed at the initiator's
  address as routed. That is pre-existing upgrade behaviour: a responder
  cannot tell a direct handshake from a blind-relayed one by hop count.
  It is framing-correct, and adjacency predicates stay conservative.
- **No CLI-level success witness.** On loopback, a token address that fails
  at join time also fails as a hint. The success path is witnessed in the
  core; a CLI or natsim row, where direct fails at join time and works
  later, is left for the natsim harness.
- **Pair actions.** `SinglePunch` pairs (Cone×Cone, …) still defer, as
  before: single-punch upgrades are not wired.


**S7 correction (found by S7's CI, fixed in the S8 commit).** The
own-announcement push on routed sessions also ran on mesh-relayed
(`connect_via`) sessions. There it stalled the RTC upgrade dialog:
`rtc_classifier::an_ice_pair_schedules_the_upgrade_attempt` never installed
the RTC endpoint, and it passes with the push disabled. I did not isolate
the exact mechanism.

The push is now limited to blind-relayed sessions (`PeerAddr::Relayed`),
which are the only kind that needs it: a blind relay is not a mesh member,
so nothing floods between the two ends. A mesh relay already floods both
announcements.

**Open.** The same hop-0 announcement over a mesh-relayed session still
arrives at the 150 s re-announce, so if the interaction is real it can
still occur later in a session's life. It is recorded as a follow-up, not
claimed fixed.

After the fix:
- the RTC harness family passes 207/208; the other, `rtc_repairs`' 0.3 s
  fragment test, failed once under the full-family load and passed 3/3
  alone;
- the S7 witnesses still pass.

**S8 receipt: TCP/443 last-resort tunnel (decision 9, 2026-09-25).**

User decisions (2026-09-25):
- plain TCP with length-prefixed frames, no TLS;
- automatic last resort;
- the same port as UDP.

**Relay (`BlindRelay`):**
- **Opening a tunnel.** A new preamble kind, `splice::TUNNEL`, on the
  existing TCP listener. After the `OK` status byte the stream carries
  exactly the relay's datagrams, each as a big-endian `u16` length and the
  bytes.
- **Endpoint.** Each tunnel gets a synthetic endpoint in the RFC 6666
  discard-only prefix `100::/64`, and the unchanged state machine runs
  against it. Registration still signs that observed endpoint; channels,
  forwarding, rate limits, bounds and splice offers are all as before.
- **Delivery.** `Shared::deliver` sends to a tunnel endpoint down its
  stream (dropped if the tunnel is gone or its 1024-frame queue is full,
  keeping datagram semantics), and everything else over UDP.
- **Isolation.** The UDP loop drops any datagram whose source claims the
  tunnel prefix.
- **Bounds.** A tunnel holds one of the existing `max_tcp_connections`
  permits and closes after `channel_idle` without a frame.
- **Stats.** `tunnels_opened` is counted and printed in `relay serve`'s
  stop row.

**Node:**
- **Fallback.** `RelayClient::exchange` tries UDP twice (about 3 s). Only
  when that gets no answer does it open the tunnel (`open_tunnel`, which
  serializes opens) and retry over it.
- **Stickiness.** While the tunnel lives, everything for that relay uses
  it. A node's registration and channel binds must share one endpoint, so
  returning to UDP mid-stream would split them. A node tries UDP first
  again only when it next needs a new tunnel.
- **`PeerSink` routing.** `RelayTunnels` routes `PeerAddr::Relayed` frames
  into the tunnel.
- **Hot path.**
  - Direct (`PeerAddr::Udp`) sends are unchanged.
  - A relayed send pays one relaxed atomic load while no tunnel exists.
  - Tunnel frames are drained by their own task (`spawn_relay_tunnel_ingress`,
    exits on shutdown) into the same `relay_ingress` → `dispatch_packet`
    path. The UDP receive loop, and the batched receiver, are unchanged.
- **Observability.** `MeshNode::relay_tunneled`.

**CLI:**
- `up --enroll` readiness reports `relay_transport: udp|tcp`.
- `join` and joined `up` report `attach_path` / `path: "relay_tcp"` when
  attached through the tunnel. The UDP relay case keeps `"relay"`, so
  existing consumers are unaffected.
- `RELAY_REGISTER_WAIT` rose from 4 s to 6 s, leaving room for the UDP
  attempts and then the tunnel before readiness.
- `relay serve` documents TCP/443.
- Docs: the CLI reference (`web/.../reference/cli.md`).

Witnesses:

| Test | Proves |
|---|---|
| `blind_relay::tunnel_endpoints_are_discard_prefix_and_distinct` | Synthetic endpoints sit in `100::/64`, are distinct, and are never a real address |
| `blind_relay::a_mesh_session_runs_through_the_tcp_tunnel_when_udp_is_blocked` | With the relay's UDP blocked, device registration and joiner bind both fall back to tunnels. A routed handshake plus a sealed request/ack runs end-to-end through them (2 tunnels opened). |
| `blind_relay::udp_is_used_when_it_answers` | With UDP answering, no tunnel opens |
| `blind_relay::a_byte_stream_is_spliced_to_a_device_registered_over_the_tunnel` | Enrollment splice offers reach a tunneled device through its tunnel |
| `cli/tests/relay_join.rs::a_relay_whose_udp_is_blocked_is_reached_over_its_tcp_tunnel` | A real `relay serve` with UDP blocked (`fixtures`-only `NET_MESH_FIXTURE_RELAY_BLOCK_UDP`). The operator reports `relay_transport: tcp`. The agent enrolls through the relay (`enroll_path: relay`, the splice) and attaches with `attach_path: relay_tcp`; joined `up` reports `path: relay_tcp`. |

**CI.** A new "net-cli relay TCP-tunnel witness (fixtures)" step pins the
CLI witness by name, reusing the crash step's `fixtures` build. The
blind-relay units run in the core unit job.

Inverse mutations, all RED (5 core, plus 1 end-to-end):

| # | Mutation |
|---|---|
| T1 | Never fall back to the tunnel (RED in the core and in the CLI witness) |
| T2 | Relayed sends ignore the tunnel |
| T3 | The relay sends to tunnel endpoints over UDP |
| T4 | Tunnel tried before UDP |
| T5 | Tunnel ingress never dispatched |

Gates:
- core clippy (all, default and no-default features; strict and all-targets);
- root rustdoc;
- CLI clippy, default and `fixtures`;
- `cargo tl` 5822/5822 and `cargo t` 7053/7053;
- `direct_upgrade` 19/19;
- the RTC harness family (CI's list) as described above;
- SDK 831/831, MCP 275/275, CLI 374/374 and CLI `fixtures` 9/9.

**Limits:**
- **No TLS.** DPI or proxies that require TLS on 443 are not crossed, and
  nothing claims they are.
- **Stickiness.** Once tunneled, a node stays on TCP until that stream
  ends.
- **Joiner recovery (not witnessed).** A joiner has no registration
  refresh. If its tunnel drops, the relayed session stays down until the
  joined runtime re-attaches; that re-bind tries UDP first again.
- **Unwitnessed guard.** The relay's drop of UDP datagrams that claim the
  tunnel prefix has no test, since such a source cannot be produced on
  loopback.
- **No `DEFAULT_RELAY`.** It stays empty until a real relay is deployed.


**Post-S8 CI repairs (2026-09-25).** Three side workflows were red on the
branch.

- **natsim `rtc_anchor_direct` / `rtc_anchor_stun_endpoint`, broken since
  S6.**
  - *Cause.* The routed-handshake responder replayed its held announcements
    to every newly attached peer, including a peer that reached it through
    a mesh relay. The replay opens a capability stream on that routed
    session. The RTC install fence (`session_is_busy` in the RTC install
    path) counts any non-signalling stream as busy, and streams are never
    closed, so every ICE install to that peer was refused.
  - *Fix.* The replay now runs only when the routed handshake crossed no
    mesh hop (`hop_count == 0`: a peer attached to this node). A peer behind
    a mesh relay already gets the flood from that relay.
  - *Witness.*
    `capability_multihop::a_peer_behind_a_mesh_relay_gets_no_replay_on_its_session`,
    RED under `replay = true`.
  - *S7 explained.* The same mechanism accounts for the S7 regression
    recorded above, so that entry's open question has an answer.
- **Examples compile.** The Rust `tokenchannel` skill example constructed
  `SubscribeOptions` without the `chain` field added in V3-2A C1. It now
  uses `..Default::default()`. A comment in `check-skill-examples.sh`'s
  unquoted heredoc carried backticks (shell command substitution), which
  produced the "bytes::Bytes: command not found" noise; removed.
- **Documented API exists.** Two pages said there is no `net up`. That is
  stale (and the check rejects a bare `net` invocation). They now point at
  `net-mesh up --enroll`, `join`, and `wrap --joined`.

**Open, needs a decision (not changed).** The RTC install fence treats the
capability-announcement stream as busy. The mesh's own `session_is_busy`
excludes control streams, but the RTC one excludes only the signalling
stream. So any mesh-relayed session that has ever carried a forwarded
announcement can never upgrade to RTC. Ordinary flood forwarding does
reach routed peers, so this can happen in a long-lived session without
either of this branch's changes. Aligning the RTC gate with the mesh gate
(excluding control streams) would fix it, but that is a change to an RTC
security gate and is left for an explicit decision.

## 8. Cumulative acceptance matrix

All rows are required unless explicitly marked feature-conditional; narrow slices can be accepted independently without calling the entire plan complete.

| ID | Required positive and negative evidence |
|---|---|
| E1 | A default preauthorized mesh link joins a clean device without hand-installed PSK or a second human approval; wrong issuer/pin/domain fails. Optional require-approval mode cannot issue before its exact-claim approval. |
| E2 | Inspect/preview leaves nonce and stores untouched; revoked/expired/altered link cannot issue credentials. |
| E3 | Two concurrent redeemers produce one bound identity; intended-subject mismatch and invalid proofs cannot claim an invite. Same-identity lost-response retry proves fresh key possession and returns the committed receipt, not a new grant; crash/restart cannot duplicate issuance. |
| E4 | Join never implicitly emits agent INVOKE/DELEGATE, org dispatcher rights, capability grants, channel `ADMIN`/wildcard, or subnet ROUTE/EXPORT. A separately authorized positive control works. |
| E5 | Mesh+subnet, mesh+org+channel+subnet and already-connected standalone org/channel/subnet joins bind the same proven identity and each exact independent scope. |
| E6 | Wrong owner/issuer/epoch/subject or broadened scope refuses; existing owner is not replaced. |
| E7 | Partial issuance, filesystem failure and profile publication interruption are resumable without fabricated joined output. |
| E8 | A normal restarted SDK/CLI consumer loads enrollment without the original invite or root key; revoked credentials cannot silently renew. |
| E9 | Inventory, credential installation, live admission/subscription/publish observation and unknown/stale states are distinguishable; protected inventory and mutation require actual management authority. A live channel roster is never reported as durable membership. |
| E10 | Removing B leaves C working in the same subnet; B's old credential/session and real reconnect are denied before protected effects. |
| E11 | Removal survives authority/verifier restart and replay of old valid state; delayed old-incarnation work cannot delete newly authorized state. |
| E12 | Org/subnet removals affect their own authority axes; alternate credential/ancestor/export paths are accounted for and residual access disclosed. |
| E13 | Commit, delivery, application and denial are separate observations; unreachable verifiers cannot satisfy a wait; retry does not duplicate removal. |
| E14 | No secrets in debug/error/stdout by default, no real user directories touched in tests, and Unix/Windows persistence checks actually execute on their platforms. |
| E15 | V2 target/framing/deadline/confirmation/typegen/journey regressions pass; service startup timeout does not kill a successfully started listener. |
| E16 | Public CLI journey proves a provider-side accepted effect, an acknowledged full-chain channel subscription, a caller-requested publish accepted by the local production channel gate (and subscriber receipt where the fixture requests end-to-end evidence), actual unauthorized gate denials, selective removal, restart and cleanup. Optional bootstrap adapters require their own feature-enabled tests. |
| E17 | Mesh/org/channel/subnet leave disables the exact relation, stops controlled live use and automatic renewal/rejoin/re-subscribe across restart, preserving identity, unrelated authority and data; unmanaged live use is stop-unconfirmed. |
| E18 | *(Amended 2026-09-25: leave is local, with no issuer-notification queue.)* An offline leave durably disables the selected local relation. Retries are idempotent, and neither restart nor a delayed renewal can undo it. Unconfirmed remote cleanup (unsubscribe, verifier withdrawal) is reported separately from the completed local departure. Leave never implies issuer-side revocation. |
| E19 | Concurrent join/renew/subscription completion cannot undo leave; delayed old leave/notification cannot revoke or deactivate an explicitly authorized successor; rejoin preserves floors and the single-owner guard. |
| E20 | *(Amended 2026-09-25: `--detach` is deferred. `up` stays in the foreground and compatible with systemd, launchd and Windows service supervision.)* `up` starts one production node for the selected profile. It reports ready only after bind, identity/config install and runtime-loop start. Duplicate startup, startup failure and stale metadata cannot produce false readiness. Ownership is exact to the incarnation, and a failed start cleans up after itself. |
| E21 | *(Amended 2026-09-25: `kms:` is deferred. There are no placeholder adapters, and every unsupported scheme fails before bind.)* With no supplied PSK, first start generates and durably protects one random profile PSK and restart reuses it; corruption/insecurity refuses rather than regenerating. `file:` and `stdin`, supply the exact runtime secret without argv/environment/output leakage. Malformed, insecure, unsupported or unavailable sources fail before bind; changing a source while live does not silently rotate the node. |
| E22 | `down` authenticates the exact local incarnation, drains and stops it, and verifies termination while leaving another profile/node untouched. Timeout/unknown/forced termination and already-stopped states remain distinct; shutdown never claims authority revocation or secure erasure. |
| E23 | *(Amended 2026-09-25: the link shape is `{channel, root, rights}`. The issuing node is the supported subscription publisher, and the leaf expiry is inherited from the grant; there is no per-device expiry variant in V3. Invitation redemption expiry and issued-credential expiry are distinct and are both shown. The effective credential lifetime and delegation limits are shown. Unsupported lifetime, depth or publisher overrides are refused, never ignored.)* A channel link computes and binds the canonical name plus the `u64 ChannelHash`, the configured root and explicit rights. `SUBSCRIBE` binds the issuing node as publisher only after its own config proves the root is trusted; `PUBLISH` has no remote publisher target and never auto-installs a root. A `u16` input, canonical name/`u64` mismatch, wrong route/root/subject/action, empty/delegate-only rights, `ADMIN`, wildcard or widening is refused. Two names sharing a `u16` hint remain separate and both can work without authority swap. |
| E24 | One valid channel link has one durable subject-bound `TokenChain` result. Same-identity crash/lost-response recovery returns that result; a second identity cannot redeem it; link inspection and failed proof do not consume it; no token bytes or authority private key appear in output/logs. |
| E25 | The SDK carries a delegated multi-link `TokenChain` without flattening. `SUBSCRIBE` reports live only after core full-chain ACK and re-presents that chain to regain ACK on reconnect/restart; intended `EntityId` metadata is not called authenticated. `PUBLISH` reports credential-ready only when the subject's local config trusts the root and live-active only after real `publish`/`publish_many` clears that local gate. Self-issued/untrusted, expired, revoked or attenuated chains fail at their respective production gates. |
| E26 | Subscribe leave durably disables exact target/channel/incarnation use before acknowledged unsubscribe. Publish leave conditionally removes only its exact managed profile/channel chain and targeted cache authority; stale leave cannot remove a successor, conflicting same-channel install refuses, and cache/unmanaged fallback yields stop-unconfirmed. Both survive restart, preserve unrelated relations and report copied-token validity/issuer revocation separately. Same-root cross-publisher portability remains disclosed. |

For denial witnesses, observe a real authenticated request at the relevant production gate and its refusal, then run an authorized positive control. A timeout or absence of output alone is insufficient. For durability/ordering claims, barriers/fault injection must reach the production transition; sleep-based contention is supplemental. Disable the claimed mechanism in a disposable review worktree and require the named witness to fail.

## 9. Verification commands and handoff

These are **future implementation gates**, not executed results. Run Rust commands from `net/crates/net/`; read current `AGENTS.md`, `TESTS.md` and CI first. Proposed tests below must exist and discover nonzero cases before running them.

```sh
cargo check -p net-cli --all-targets

# NEW V3 DELIVERABLES: run after their files have been added.
cargo nextest run -p net-cli --test enrollment_lifecycle --test node_lifecycle --test org_join --test channel_join --test subnet_join --test enrollment_leave --test enrollment_status --test enrollment_removal --test enrollment_workflow --no-tests=fail --retries 0

# Existing CLI compatibility coverage, then one complete default sweep.
cargo nextest run -p net-cli --test org_adopt --test org_grant --test subnet_issuance --test target_resolution --test remote_inspection --no-tests=fail --retries 0
cargo test -p net-cli

# Existing owning core tests; aliases preserve the repository feature graph.
cargo t --test channel_auth --test channel_auth_hardening --test channel_auth_origin_binding --test channel_identity_readiness --test subnet_auth_e2e --test subnet_revocation --test subnet_session_auth --test subnet_control_facts --test org_ownership --retries 0

cargo clippy -p net-cli --bin net-mesh -- -D warnings
cargo fmt --all -- --check
```

Add focused SDK/unit tests at the actual owning module with the current supported feature set; verify discovery counts rather than guessing filters. Run touched-crate rustdoc/clippy and the current repository pre-push checks. Default versus `rtc-bootstrap` coverage stays distinct. Run `npm run check` from `web/` after public documentation changes. No fabricated all-features result, ignored authority witness, or retries masking an inverse failure.

Every slice handoff names exact HEAD, changes, executed commands/counts, inverse results, persistence/propagation limits, platforms and exact-head CI. Do not mark optional future work as release debt unless explicitly accepted into scope.

## 10. Non-goals and stop line

No new global node registry, central scheduler, remote Deck protocol, generic administrative shell, arbitrary delegation engine, replicated membership consensus, custom revocation anti-entropy system, universal credential vault, compliance workflow, QR/mobile app, browser landing-page product, public invitation directory, serverless capability provider, agent migration, transfer holder, generic streaming CLI, or redesign of nRPC/storage. The bounded `kms:` PSK-source adapters are startup integrations with named secret managers, not a general KMS abstraction, arbitrary secret broker or key-lifecycle product.

The narrow service/state/API additions necessary for this enrollment-and-removal loop are in scope after V3-0 acceptance. A generalized framework is not. Preserve native SDK ownership of mechanisms, but do not turn this CLI plan into blanket five-language feature expansion; wire/ABI changes must still update every affected decoder/binding and conformance witness or remain disabled for incompatible peers.

**Stop after the proved lifecycle.** The purpose is to make Net usable for real operator conversations and deployments, not to finish every administrative command before starting those conversations.

## 11. Planning validation

This planning document is the only intended existing file changed by this CLI-plan edit; V2 and its active implementation files remain outside it. The managed-node amendment adds proposed `up`/`down`/`node status` and protected PSK-source contracts. The channel amendment adds proposed `channel invite`/`join`/`leave`, canonical channel identity, root-anchored token-chain, live-evidence and lifecycle acceptance contracts. Neither amendment implements or advertises shipped commands. Validate Markdown structure, relative links, existing source paths, labels for proposed files/commands and whitespace. Record any concurrently observed user changes separately; do not restore, stage or commit them. No implementation, Cargo tests, public docs, commits or pushes are part of editing this plan.
