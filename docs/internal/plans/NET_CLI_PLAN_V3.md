# Net CLI V3 — enrollment and operator lifecycle implementation plan

> **For Hermes:** After V2 acceptance and explicit implementation authorization, use the subagent-driven-development skill for one accepted slice at a time, with independent review. This document authorizes planning, not production edits or protocol publication.

**Status:** V3 continuation authorized by the user on 2026-09-20. V3-0 source re-survey/design preparation started at `346b4b8bfe74ee8399a5f4191bfc7b86eb7f3a84`; its exit gate is **not passed**. V2's agreed implementation is complete, including generated protected-client coverage, but accepted exact-head CI evidence is still outstanding. The first narrow V3 code slice implements local invitation policy only; no working enrollment service or protocol publication is claimed. See the V3-0 decision record below before implementing wrappers.
**Goal:** An operator sends a join link; a new device joins the intended mesh, optionally its organization and exact subnet, survives restart, can voluntarily leave, and can be selectively removed by an operator with verifiable, honestly scoped enforcement.
**Architecture:** Thin Rust/Clap commands over reusable SDK enrollment and authority mechanisms, backed by an explicitly running operator service and durable local state. Enrollment, observation, and removal refer to real identities and real enforcement points; temporary supervisors, inventory records, and credential files never stand in for deployment effects.
**Tech stack:** Existing `net-cli` / `net-mesh` executable, Tokio, `net-mesh-sdk`, signed organization/subnet credentials, current native transport and optional bootstrap adapters. No new global control plane.
**Planning source snapshot:** `234c3685a285353d89fd92a540eb33a826c44194`, in `C:/Users/chief/orca/workspaces/net/net-cli`. V2 implementation was actively editing the checkout during inspection. This is a source survey, not runtime acceptance or the eventual V3 implementation baseline.

## 1. Relationship to V2

V2 makes the existing publisher/consumer experience truthful and usable. V3 removes manual enrollment plumbing and supplies the next bounded operator lifecycle. It does not supersede, reopen, or silently enlarge V2.

Before starting V3:

- V2's accepted scope is complete: restore safety/persistence, DOC-0, CLI-2A/2B target resolution, CLI-DX automation, live typegen, and its documented MCP/native capability journey.
- Record the accepted V2 commit and actual required exact-head CI results. Do not infer completion from the current plan's partial implementation notes.
- Re-survey affected APIs at that commit; reconcile names, features, and tests below without undoing V2 decisions.
- Inherit V2's `--inspect-target`, profile precedence, bind validation, explicit execution modes, deadline budget, stdout/stderr framing, confirmation and error-code contracts.
- Retain temporary-supervisor `--local` warnings for legacy commands. Adding narrow live enrollment operations does not turn every Deck command into remote administration.

V2's deferred transfer holder, generic unary RPC, remote Deck, and crash-safe NetDB replacement are **not** prerequisites and are **not** implicitly moved into V3. Organization streaming remains a separate parallel plan; V3 can prove enrollment with unary calls. Neither serverless capability providers nor serverless anchors are required.

## 2. The complete operator journey

1. On operator A, select the real identity, authority stores, trust domain, and reachable listener. Start or attach to the explicitly named enrollment service. The listener's lifetime is visible.
2. Create a short-lived join link for the intended target. Mesh and organization invitations can carry an explicitly authorized subnet attachment scope. A device already on the mesh can receive a standalone subnet link.
3. On device B, inspect the link locally, confirm the intended roots/scope, generate or load its own persistent identity, and explicitly redeem. Root private keys never leave A.
4. A validates B's proof of identity, the invitation and current policy, and issues exactly the preauthorized bundle. Creating the invitation is the inviter's authorization: by default there is no second human approval. Only invitations explicitly created with `--require-approval` wait for a subsequent operator decision.
5. B durably installs the returned configuration/credentials and proves the requested live admission. A membership certificate is not yet an admitted subnet session. If only part completes, report the individual stages and provide a same-identity resume path.
6. B starts an ordinary provider/caller using the saved enrollment/profile through existing SDK/CLI paths. The operator sees identity, credential state, actual observed admission, and the source/freshness of that observation.
7. An invocation without separately granted execution authority is denied. Apply the explicit existing dispatcher/capability/provider permissions and prove the authorized call reaches the exact provider.
8. Remove B from one selected subnet. The selected enforcement points refuse B's old credentials, including after reconnect/restart; unaffected device C still works. B's unrelated organization/capability/subnet authority is not silently revoked.
9. Revoke organization membership separately and prove its own admission consequences. Report remaining transport and independently granted access rather than claiming the device has disappeared from the entire mesh.
10. On a separately admitted device, voluntarily leave a selected subnet/org/mesh, including while its authority is unreachable. Prove local participation and automatic renewal/rejoin stop across restart, unrelated relations remain intact, and authority notification is reported separately from revocation. Later rejoining requires explicit intent and current authorization.

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
| Subnet authority | `src/adapter/net/subnet/auth.rs`, `src/adapter/net/subnet/admission.rs`, `src/adapter/net/subnet/control.rs`, `src/adapter/net/subnet/provision.rs`, `cli/src/commands/subnet.rs` | Reuse `SubnetRef`, verified credential sets, challenge-bound admission and signed control facts. Existing `SubnetRevocationFloor` is subtree/generation scoped, not a per-subject removal object. |
| CLI/security witnesses | `cli/tests/org_adopt.rs`, `cli/tests/org_grant.rs`, `cli/tests/subnet_issuance.rs`, `tests/subnet_auth_e2e.rs`, `tests/subnet_revocation.rs` | Starting points for compatibility and inverse evidence, not a substitute for real CLI enrollment/removal subprocesses. |

### Additional verified mechanism boundaries

- **Org per-member revocation already exists.** `src/adapter/net/behavior/org_revocation.rs` keys floors by `(OrgId, EntityId)`. `OrgRevocationStore::apply_bundle` verifies signed bundles, reloads persisted maxima under an interprocess lock, merges monotonically, persists, and publishes its live view. Raise above the membership certificate's generation. V3 needs a thin explicit apply/status path, not a replacement org revocation protocol.
- **Disk coordination is not live propagation.** Same-process, same-path org store handles share a live core. This does not make a CLI write automatically refresh another running process or remote verifier. `MeshNode::install_org_revocation_store` wires the live runtime to its installed store; the V3 service must drive that actual owner. Preserve `DurabilityUncertain` and poisoned-store errors: an uncertain durable write cannot become ordinary success.
- **Org adoption already has an owner.** `src/adapter/net/behavior/org_authority.rs` provides `NodeAuthority::adopt/open`; `sdk/src/org/provision.rs::install_org_authority_node` loads and installs it. Reuse that persistence rather than creating another membership directory schema.
- **Local subnet withdrawal is not revocation.** `MeshNode::withdraw_subnet_admission` clears one peer's challenges/context; valid retained credentials can re-admit. `subnet_context_for` is a real current-context observation hook. `known_subnets` is observer topology, not an admitted-member roster.
- **Gateway installation is not endpoint enrollment.** `sdk/src/subnet.rs::admin` already wraps control-fact application, gateway credentials and boundary declarations. Gateway credentials and boundary declarations are wholesale replacements, not append operations; adding B must not discard unrelated local authority. Reuse these advanced hooks only for their actual role.
- **Subnet path zero is real authority scope.** A `SubnetRef` combines authority and topology path; path `0` is that authority's entire hierarchy, not a missing/default selector. Reject omitted scope instead of silently selecting root-wide authority.

Three gaps must be resolved before user-facing success claims: **first-contact provisioning without hand-managed PSKs; membership-only enrollment without ambient execution grants; selective subnet revocation without collateral scope-wide removal.** A bounded SDK/core extension is allowed in the future accepted implementation slice when these existing mechanisms do not compose. Do not conceal a protocol gap with a CLI-only workaround.

## 4. Product surface and execution ownership

The following is **proposed V3 syntax**, not shipped commands. Final parser factoring may change at V3-0; preserve the user journeys and existing executable name. Use one implementation for link parsing/redemption, not separate mesh/org/subnet engines.

| Proposed surface | Meaning and execution owner |
|---|---|
| `net-mesh enrollment serve` | Foreground, operator-owned enrollment/status service using selected durable stores. Explicit bind and readiness; stopping the process stops service. No implicit background daemon or hosted SaaS. |
| `net-mesh invite create` | Create a mesh invitation through the actual authority owner; optional exact `--subnet` scope. |
| `net-mesh invite inspect` | Offline, redacted parse/signature/expiry inspection where verifiable; never consumes a nonce or claims current authority. |
| `net-mesh invite revoke <invite-id>` | Invalidate an unredeemed invitation at its owner. This does not revoke previously issued credentials or a leaked standing PSK. |
| `net-mesh join <join-link>` | Enroll a new device into the stated mesh; persist and apply the selected bundle. Also support protected file/stdin input to avoid shell-history leakage. |
| `net-mesh org invite <org-ref>` / `org join <join-link>` | Explicit organization membership enrollment, optionally composed with mesh connectivity and subnet attachment. No owner replacement. |
| `net-mesh subnet invite <subnet-ref>` / `subnet join <join-link>` | Explicit endpoint attachment invitation; standalone join reuses an already-connected identity. |
| `net-mesh leave <mesh-ref>` | Voluntary local departure from the selected mesh enrollment; disables its automatic attachment/renewal and stops its controlled runtime participation, without deleting device identity or claiming transport-secret revocation. |
| `net-mesh org leave <org-ref>` | Voluntary local deactivation of that organization enrollment and dependent use; not an owner transfer or issuer-side membership revocation. |
| `net-mesh subnet leave <subnet-ref>` | Voluntary local withdrawal from that exact qualified subnet; unrelated memberships and authority remain unchanged. |
| `net-mesh enrollment status` / `enrollment devices` | Own service/enrollment status and issuer inventory, with separately attributed live observations. Not an omniscient mesh roster. |
| `net-mesh org members <org-ref>` / `subnet members <subnet-ref>` | Authorized issuer inventory plus scoped enforcement-point observations, distinguishing issued from live-admitted and unknown. |
| `net-mesh org remove <org-ref> <entity-id>` | Revoke the selected membership relation using the actual organization floor/admission mechanism. |
| `net-mesh subnet remove <subnet-ref> <entity-id>` | Revoke the selected subject's covered attachment authority; requires the selective-revocation mechanism below. Not a synonym for list deletion or disconnect. |

Use full cryptographic subject identifiers for mutations, not ambiguous names, truncated fingerprints, or a routing `u64`. Display names are untrusted labels. A shortened display is not a signing/lookup identity.

Local operator commands must operate on the same durable authority state as the running service, through a locked local control path or authenticated bounded SDK service. Choose the minimum existing-compatible path at V3-0. Spawning a fresh `OperatorEnrollment` for every invite while the server owns a different in-memory map is invalid. Remote issuance/removal must never be exposed merely because a caller holds a PSK or membership certificate; keep remote mutation unavailable unless the exact management authority is implemented and tested.

## 5. Non-negotiable authority and secret boundaries

### 5.1 Independent relations

| Relation | What it establishes | What it never implies |
|---|---|---|
| Transport configuration/session | Connectivity within a stated trust domain | Organization membership, subnet attachment, management or invocation rights |
| Mesh/device enrollment record | Issuer-approved device association and its named credentials | Generic agent delegation or universal mesh access |
| Organization membership | One device belongs to its owner org | Dispatcher rights, foreign-provider access, or permission to replace an existing owner |
| Subnet credential and live proof | An exact subject can attach at a qualified target under current policy | ROUTE, EXPORT, administration, or capability execution |
| Dispatcher/capability grants | Their explicitly signed rights and scope | Bypass of provider-local admission |
| Inventory/control-channel visibility | Information observed or recorded | Authority to mutate or to manufacture admission proof |

Use current production identity bindings; document where `EntityId`, transport keys and routing IDs differ. Never substitute a self-asserted packet field for authenticated subject identity.

### 5.2 Link and bootstrap contract

- Join links are the primary interface; QR encoding, hosted landing pages, OS URL-handler installation, and mobile-specific UI are deferred.
- Explicit `join` performs redemption. Inspection, preview GET/HEAD, unfurling, completion and help must not redeem or approve anything. If no HTTP landing page ships, test the offline inspector and any existing bootstrap endpoints that a preview could reach.
- A versioned, integrity-bound invitation names issuer/root, allowed operation, exact target scope, expiry, single-use identifier, and optional intended device identity. Org and subnet authority must be independently verifiable; one issuer's signature does not grant another authority's rights.
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
- Partial bundle issuance does not claim distributed atomicity. Record which authorities issued which credentials; resume only the same approved intent and identity, rechecking current revocation. Never undo an already-issued grant merely by deleting local files.
- File failures leave a recoverable state and no final `joined` result. Profile publication must not point at missing identity/credential files. Avoid clobbering existing ownership, credentials or profiles; conflicting ownership is a refusal, not an implicit migration.
- Bound outstanding requests, receipts, pending approvals and retained history; fail closed on capacity. Retain replay protection for its required lifetime and require tested cleanup rules across restart.
- Ordinary renewal must not revive a removed member or expand scope. Re-enrollment after removal is a separate explicit, floor-aware issuance operation; never lower floors or switch identity automatically to make it succeed.

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

- Resolve the selected enrollment and preview its dependent providers, callers, attachments and renewal tasks. Require the existing V2 confirmation contract for consequential changes; `--yes` bypasses the prompt, not scope validation. Never select all meshes/orgs/subnets by default.
- Persist a scoped **left/disabled intent** before attempting authority notification. Fence in-flight join/renew/install callbacks so delayed success cannot reactivate it. Startup and renewal paths must honor this state; explicit later join is the only way to restore intent, and it still needs current authorization.
- Stop new work under the selected relation and deactivate its controlled runtime use, renewal and automatic reattachment. Define a bounded shutdown/drain policy for already-active work; do not replay calls or claim cancellation reverses completed remote effects. Remove affected advertisements/attachments where the runtime owns them.
- A CLI process cannot stop arbitrary independent SDK processes by editing a profile. Use the actual local runtime/control owner selected at V3-0. If an existing consumer cannot be fenced or acknowledged, report **local configuration disabled; runtime stop unconfirmed** and do not emit complete-left success. An explicitly offline mode may disable next-start use, but must disclose its weaker guarantee.
- Deactivate only the selected enrollment's credential references. Preserve the device key, unrelated memberships, retained application data, audit receipts and revocation maxima. No secure-erasure claim and no automatic deletion of shared PSKs or credential files used by another relation.
- Leaving an org does not erase historical ownership or authorize adoption by a different org. Retain the existing single-owner guard; ownership migration remains outside this plan. Leaving a mesh makes dependent use over that connection unavailable without pretending it revoked independent org/subnet credentials.
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
| Operator ownership | `sdk/src/operator.rs::OperatorEnrollment` owns an in-memory `pending` map; `EnrollmentAuthority` separately tracks spent nonces. Inventory/revocation file persistence does not persist either invitation ledger. | Proposed: one foreground service holds a lifetime lock on its selected authority store and owns durable invite/claim/receipt transitions. Mint/approve/revoke clients must talk to that owner; never instantiate a fresh coordinator. Root keys stay with that owner. Choose and test protected local IPC on Unix and Windows before adding mutation commands; remote management stays unavailable. |
| Native first contact | `sdk/src/mesh_enroll.rs::Rendezvous` contains address, Noise public key and routing ID, but explicitly assumes an out-of-band PSK. `Mesh::join` starts from an already-built mesh. | Product choice resolved: preauthorized, single-use invitations redeemed through an authenticated adapter, without a standing PSK in the link or a second approval by default. Optional `--require-approval` is invitation-bound policy. The concrete transport/control proposal below remains subject to security witnesses and V3-0 exit, not a shipped guarantee. No public/default PSK workaround or secret-bearing mode is selected for V3-1. |
| Existing browser bootstrap | `sdk/src/bootstrap_credential.rs::BrowserBootstrapCredential` is signed and secret-bearing. SDK HTTP/TLS dependencies and the CLI listener are gated by `rtc-bootstrap`, which also enables WebRTC. | Reuse verification/secret-redaction concepts, not a silent browser-feature dependency. This is not evidence for native secure redemption. Do not enable `rtc-bootstrap` globally to make a new default command appear to work. |
| Membership-only outcome | `sdk/src/enrollment.rs::JoinOutcome::Admitted` contains a delegation chain; `sdk/src/delegation.rs::derive_device` issues `INVOKE_ACTION | DELEGATE`. `InviteToken` itself has no issuer signature or operation/scope fields. | Preserve existing agent enrollment and `NMI1`/`NMO1` behavior. V3 needs a separately versioned integrity-bound invite and membership-only receipt/bundle, never an empty/fake delegation chain. Proposed receipt binds issuer, full subject, invitation/operation ID, exact requested relations, request-intent digest and committed result; finalize encoding after transport/store decisions. |
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

**Proposed V3-1 adapter boundary:** one native HTTPS redemption listener owned
by `enrollment serve`, separate from the mesh UDP listener and local management
IPC. Reuse the workspace's rustls/Tokio/HTTP dependency versions and explicit
crypto-provider construction, not the browser RTC offer/trickle service.
Initially use operator-provisioned TLS certificates with normal certificate,
validity and hostname verification plus the endpoint-key pin bound into the
signed invite. No plaintext fallback, redirects, certificate-ignore switch,
WebRTC prerequisite, ACME automation or public management routes are added.
Local CI uses a disposable test CA trusted only by the fixture client; never
modify the workstation trust store. Certificate/pin rotation invalidates old
unredeemed links unless a later explicit rotation contract is accepted.

Dependency placement is an explicit review item: propose a separate optional
SDK/CLI `enrollment-bootstrap` feature for native HTTP/TLS, independent of
`rtc-bootstrap`. Offline inspect and typed receipt/store mechanisms must not
need the listener feature. A build lacking it must refuse live enrollment
before effects with a precise feature diagnostic, not fall back to a permissive
transport. Decide release-binary inclusion and add a real feature-enabled CI job
before claiming the advertised join journey is available in shipped binaries.
This proposal does not change current Cargo features.

**Invite/request/response contract (semantic, not allocated wire bytes):**

1. The versioned issuer-signed invite binds full issuer identity, named trust
   domain, HTTPS endpoint and transport key pin, random invitation identifier,
   expiry, exact authorized relations, optional intended full device identity
   and approval policy. The link contains no standing PSK, root key or audience
   secret, but **is still sensitive bearer authorization** when subject-unbound.
   Signature verification preserves the supplied issuer binding; the recipient
   must confirm that this is the intended issuer, not trust any self-signed root.
2. Offline inspect verifies what it can and performs no network request or claim.
   Redemption/receipt recovery use POST bodies, never secret query parameters.
   GET/HEAD cannot reserve, approve, issue or return a credential bundle. Redact
   invitation identifiers/proofs/bearer material from diagnostics; return only
   non-secret operation identifiers by default.
3. Before redeeming, the device persists its identity and canonical request intent.
   Over verified TLS it requests a fresh, bounded, short-lived server challenge.
   The device signs a domain-separated transcript binding that challenge, the
   signed invite digest, full subject and complete intent digest. The challenge
   is bound to this live TLS connection and consumed once; a captured signature
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
Local mint/approve/revoke/status use this owner. Device-side lifecycle control is
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

Tasks:
1. Pin accepted V2 HEAD and verify its real completion evidence; map the final CLI contract into V3 commands.
2. Select one real operator-service ownership/control path and prove no per-command fresh-store split. Document root-key custody and supported local/remote management boundary.
3. Pin a clean-device bootstrap path with no manual PSK exchange, including listener owner, transport features, trust establishment and secret delivery. Produce a minimal source-backed flow, not an unattended permissive demo.
4. Specify membership-only enrollment outcome and its consumer path, preserving the existing agent delegation APIs unchanged. Pin only the new typed fields/version boundaries actually necessary.
5. Specify the minimal selective subnet-removal mechanism and verification set; identify exact SDK/core/wire owners and cross-language compatibility work if wire changes are required.
6. Map each required outcome to an existing mechanism, missing hook and witness. Record final CLI syntax/defaults and state transitions in this document.
7. Pin the local lifecycle owner for voluntary leave, its durable intent/fencing and dependent-service stop policy. Identify which consumers acknowledge live stop and which can only honor next-start disablement; do not promise control over arbitrary SDK processes.

**Exit:** No unresolved authority/bootstrap/state-owner decision may be passed to a command wrapper. If a mechanism needs a wider protocol redesign than this bounded lifecycle, stop that slice for an explicit scope decision; do not declare V3 complete or reopen serverless/remote Deck. This is a bounded design gate for named blockers, not a new platform-foundation project.

### V3-1 — durable operator service and mesh join links

**Modify:** `sdk/src/enrollment.rs`, `sdk/src/operator.rs`, `sdk/src/mesh_enroll.rs`, applicable persistence/bootstrap owners, CLI `main.rs`, `context.rs`, `target.rs`, `config.rs`.
**Proposed new files:** `sdk/src/enrollment_store.rs`, `cli/src/commands/enrollment.rs`, `cli/tests/enrollment_lifecycle.rs`. Module factoring may reuse an existing owner instead; record the final paths.

Tasks:
1. Write RED tests for clean-device bootstrap, preview-without-redemption, no implicit INVOKE/DELEGATE, and mint/serve using the same durable state.
2. Implement the selected listener lifecycle, reusable receipt store and membership-only SDK path; reuse current crypto/parser/storage conventions.
3. Add invite create/inspect/revoke and join with explicit scope, redaction and protected link input. Default creation preauthorizes one scoped redemption; optional `--require-approval` adds a later human decision. Show bearer-versus-intended-subject policy without exposing the link. Keep the default CLI build usable without silently enabling browser/server features.
4. Persist device identity and enrollment/profile references; consume them through one existing V2 hosted/client path after CLI exit and restart.
5. Prove default redemption succeeds without a second approval and optional approval cannot be bypassed. Cover concurrent same/different identity redemption, lost response after commit, operator/device crash points, revoke during claim/approval, corruption and saturation. A second redemption is not an implicit grant reissue.

**Exit:** Two clean participants enroll through a link without manual PSK handling or a second approval in default mode; restart and retry recover the same identity/result. Invalid/mismatched requesters cannot redeem; optional approval cannot be bypassed. Even a valid enrolled device receives no implicit application authority. No org/subnet success is claimed yet.

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

### V3-2A — voluntary leave through the same lifecycle

**Modify:** shared SDK enrollment/persistence/lifecycle modules selected in V3-1/2; CLI `enrollment.rs`, `org.rs`, `subnet.rs`, `config.rs` and relevant runtime adapters.
**Proposed new test:** `cli/tests/enrollment_leave.rs`. Keep this inside the existing lifecycle work, not a new control-plane project.

Tasks:
1. Write RED for mesh/org/subnet leave, offline authority, repeated leave and an in-flight renewal completing after departure. Assert preserved device identity and unaffected memberships/data.
2. Implement durable scoped disabled intent, callback fencing, controlled runtime stop and profile/credential deactivation. Preserve the existing owner guard and revocation floors.
3. Add explicit-target leave commands, bounded stop/notification behavior and partial receipts. Authenticate any remote self-notification and keep advisory notification separate from revocation.
4. Prove restart does not renew/rejoin; genuine explicit rejoin uses current authorization; delayed old leave/renew callbacks cannot affect the successor. An unmanaged live consumer must produce stop-unconfirmed, not success.
5. In a disposable review worktree, bypass disabled-intent checks and allow a late renewal to install: the relevant witnesses must fail. Restore the candidate and run compatibility controls.

**Exit:** Local voluntary departure is usable online/offline and remains in effect across restart, with honest runtime and remote-state reporting. It neither depends on remote approval nor masquerades as issuer-enforced revocation.

### V3-3 — live inspection without false completeness

**Modify:** shared enrollment SDK/service and CLI `enrollment.rs`, `org.rs`, `subnet.rs`; narrow runtime read interfaces where necessary.
**Proposed new test:** `cli/tests/enrollment_status.rs`.

Tasks:
1. Write RED controls distinguishing issuer inventory, credential validity, live admission, expired observation and an unreachable node.
2. Add read-only SDK queries and CLI views with exact responder/instance, subject, scope, observation time, known revision, completeness boundary and pending stages. Authorize protected inventory reads.
3. Prove `--inspect-target` has no service startup/network/storage side effects and normal execution consumes the same resolved target.
4. Exercise empty-but-reachable, unreachable, unauthorized, truncated/bounded inventory, stale receiver and a second independent admitted device. Redact all secret fields.

**Exit:** The user can answer “what was issued, what is currently admitted here, and what is unknown?” without source inspection. Never relabel old `subnet ls --local` output as live state.

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

### V3-5 — public journey, CI and release acceptance

**Modify:** `cli/README.md`, `cli/CHANGELOG.md`, `web/src/content/docs/reference/cli.md`, relevant enrollment/security/subnet docs and `.github/workflows/ci.yml`.
**Proposed new deliverables:** fixtures under `cli/tests/fixtures/enrollment/` and `cli/tests/enrollment_workflow.rs`.

Tasks:
1. Ship runnable operator/joiner/provider fixtures and public instructions with prerequisites, terminal ownership, join links, separate grants, inspection, voluntary leave, operator removal, restart and cleanup. Demonstrate the distinction between offline local departure and remote revocation.
2. Extend V2's same-runner two-node CLI journey. Add a third identity/participant for the selective-removal positive control; loopback proves multi-node/process behavior, not multiple physical computers.
3. Exercise native publisher/consumer behavior and the existing MCP route without adding generic RPC hosting. Match accepted calls to provider-side effects and rejected calls to actual gate decisions.
4. Add platform gates for Unix permissions, Windows secret storage/replacement, process lifetime and default/optional features. Pin new root integration binaries and required authority witnesses; CLI tests remain auto-discovered.
5. Publish only accepted syntax/guarantees. Record actual commands/results, exact commit, topology and remaining off-host evidence. Do not make off-host/NAT or optional-browser claims on loopback evidence.

**Exit:** Cumulative acceptance below is complete, exact-head required CI is green, and the public instructions work without manual credential surgery or repository-internal knowledge.

## 8. Cumulative acceptance matrix

All rows are required unless explicitly marked feature-conditional; narrow slices can be accepted independently without calling the entire plan complete.

| ID | Required positive and negative evidence |
|---|---|
| E1 | A default preauthorized mesh link joins a clean device without hand-installed PSK or a second human approval; wrong issuer/pin/domain fails. Optional require-approval mode cannot issue before its exact-claim approval. |
| E2 | Inspect/preview leaves nonce and stores untouched; revoked/expired/altered link cannot issue credentials. |
| E3 | Two concurrent redeemers produce one bound identity; intended-subject mismatch and invalid proofs cannot claim an invite. Same-identity lost-response retry proves fresh key possession and returns the committed receipt, not a new grant; crash/restart cannot duplicate issuance. |
| E4 | Join never implicitly emits agent INVOKE/DELEGATE, org dispatcher rights, capability grants or subnet ROUTE/EXPORT. A separately authorized positive control works. |
| E5 | Mesh+subnet, mesh+org+subnet and already-connected standalone subnet joins bind the same proven identity and exact qualified scope. |
| E6 | Wrong owner/issuer/epoch/subject or broadened scope refuses; existing owner is not replaced. |
| E7 | Partial issuance, filesystem failure and profile publication interruption are resumable without fabricated joined output. |
| E8 | A normal restarted SDK/CLI consumer loads enrollment without the original invite or root key; revoked credentials cannot silently renew. |
| E9 | Inventory, live admission and unknown/stale states are distinguishable; protected inventory and mutation require actual management authority. |
| E10 | Removing B leaves C working in the same subnet; B's old credential/session and real reconnect are denied before protected effects. |
| E11 | Removal survives authority/verifier restart and replay of old valid state; delayed old-incarnation work cannot delete newly authorized state. |
| E12 | Org/subnet removals affect their own authority axes; alternate credential/ancestor/export paths are accounted for and residual access disclosed. |
| E13 | Commit, delivery, application and denial are separate observations; unreachable verifiers cannot satisfy a wait; retry does not duplicate removal. |
| E14 | No secrets in debug/error/stdout by default, no real user directories touched in tests, and Unix/Windows persistence checks actually execute on their platforms. |
| E15 | V2 target/framing/deadline/confirmation/typegen/journey regressions pass; service startup timeout does not kill a successfully started listener. |
| E16 | Public CLI journey proves a provider-side accepted effect, actual unauthorized gate denial, selective removal, restart and cleanup. Optional bootstrap adapters require their own feature-enabled tests. |
| E17 | Mesh/org/subnet leave disables the exact relation, stops controlled live use and automatic renewal/rejoin across restart, preserving identity, unrelated authority and data; unmanaged live use is stop-unconfirmed. |
| E18 | Offline leave completes locally with notification pending; retries are idempotent; notification never implies revocation and cannot target another identity. |
| E19 | Concurrent join/renew completion cannot undo leave; delayed old leave/notification cannot revoke an explicitly authorized successor; rejoin preserves floors and the single-owner guard. |

For denial witnesses, observe a real authenticated request at the relevant production gate and its refusal, then run an authorized positive control. A timeout or absence of output alone is insufficient. For durability/ordering claims, barriers/fault injection must reach the production transition; sleep-based contention is supplemental. Disable the claimed mechanism in a disposable review worktree and require the named witness to fail.

## 9. Verification commands and handoff

These are **future implementation gates**, not executed results. Run Rust commands from `net/crates/net/`; read current `AGENTS.md`, `TESTS.md` and CI first. Proposed tests below must exist and discover nonzero cases before running them.

```sh
cargo check -p net-cli --all-targets

# NEW V3 DELIVERABLES: run after their files have been added.
cargo nextest run -p net-cli --test enrollment_lifecycle --test org_join --test subnet_join --test enrollment_leave --test enrollment_status --test enrollment_removal --test enrollment_workflow --no-tests=fail --retries 0

# Existing CLI compatibility coverage, then one complete default sweep.
cargo nextest run -p net-cli --test org_adopt --test org_grant --test subnet_issuance --test target_resolution --test remote_inspection --no-tests=fail --retries 0
cargo test -p net-cli

# Existing owning core tests; aliases preserve the repository feature graph.
cargo t --test subnet_auth_e2e --test subnet_revocation --test subnet_session_auth --test subnet_control_facts --test org_ownership --retries 0

cargo clippy -p net-cli --bin net-mesh -- -D warnings
cargo fmt --all -- --check
```

Add focused SDK/unit tests at the actual owning module with the current supported feature set; verify discovery counts rather than guessing filters. Run touched-crate rustdoc/clippy and the current repository pre-push checks. Default versus `rtc-bootstrap` coverage stays distinct. Run `npm run check` from `web/` after public documentation changes. No fabricated all-features result, ignored authority witness, or retries masking an inverse failure.

Every slice handoff names exact HEAD, changes, executed commands/counts, inverse results, persistence/propagation limits, platforms and exact-head CI. Do not mark optional future work as release debt unless explicitly accepted into scope.

## 10. Non-goals and stop line

No new global node registry, central scheduler, remote Deck protocol, generic administrative shell, arbitrary delegation engine, replicated membership consensus, custom revocation anti-entropy system, universal credential vault, compliance workflow, QR/mobile app, browser landing-page product, public invitation directory, serverless capability provider, agent migration, transfer holder, generic streaming CLI, or redesign of nRPC/storage.

The narrow service/state/API additions necessary for this enrollment-and-removal loop are in scope after V3-0 acceptance. A generalized framework is not. Preserve native SDK ownership of mechanisms, but do not turn this CLI plan into blanket five-language feature expansion; wire/ABI changes must still update every affected decoder/binding and conformance witness or remain disabled for incompatible peers.

**Stop after the proved lifecycle.** The purpose is to make Net usable for real operator conversations and deployments, not to finish every administrative command before starting those conversations.

## 11. Planning validation

This creation changes only `NET_CLI_PLAN_V3.md`; V2 and its active implementation files are outside this edit. Validate Markdown structure, relative links, existing source paths, labels for proposed files/commands and whitespace. Record any concurrently observed user changes separately; do not restore, stage or commit them. No implementation, Cargo tests, public docs, commits or pushes are part of creating this plan.
