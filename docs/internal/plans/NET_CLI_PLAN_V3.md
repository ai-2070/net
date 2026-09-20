# Net CLI V3 — enrollment and operator lifecycle implementation plan

> **For Hermes:** After V2 acceptance and explicit implementation authorization, use the subagent-driven-development skill for one accepted slice at a time, with independent review. This document authorizes planning, not production edits or protocol publication.

**Status:** Proposed follow-on to [NET_CLI_PLAN_V2.md](NET_CLI_PLAN_V2.md). Do not interleave V3 implementation with the active V2 work.
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
4. A validates B's proof of identity, checks the issuing authority and current policy, and approves the exact bundle. Approval defaults to a human decision; headless preauthorization is explicit and bounded.
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
- The default is approve-before-issue. A headless invite is explicit preauthorization for a bounded operation, one redeeming identity and expiry; document its bearer risk. Never silently select `serve_enrollment_auto` for convenience.
- Prefer the existing authenticated bootstrap mechanisms for obtaining first-contact transport material after approval. V3-0 must prove a native CLI path from a clean device with no PSK. If those mechanisms require an adapter, scope that adapter explicitly before implementation; do not require the parked serverless project.
- Do not embed an existing private deployment's standing PSK in a supposedly harmless invite. Any chosen secret-bearing link mode requires explicit operator opt-in, a named trust domain, secret-file/stdin support and a clear warning: invite expiry/revocation does not expire or erase the standing PSK. Never call that mode approval-only transport access.
- No root private keys in links, device responses, examples, logs or generated profiles. Redact bearer strings, PSKs, audience secrets and request proofs from diagnostics/traces; only explicit secret export may emit credential material to a protected destination.
- Validate size, version, signature, expiry, operation, endpoint scheme/address and trust binding before connection or mutation. Do not follow redirects to an unpinned authority, fetch arbitrary URLs while inspecting a link, or claim a self-signed root is independently trusted. Human confirmation trusts the intended issuer; crypto preserves that binding thereafter.

### 5.3 Durable identity and redemption

Model the minimal durable transitions: offered → identity-bound approval/claim → issued receipt → device installed → live admission observed. Expired/revoked/denied and partial states are explicit, not exceptions hidden by a success message.

- Persist the device key before the first redeem request. A retry uses that key, not a regenerated identity.
- Persist the winning identity and exact scope/result before returning issued credentials. Same-identity retry returns the same committed result; another identity cannot redeem the same invitation. Bind the complete request intent, not only its nonce.
- Serialize competing redeem/revoke/expiry operations at the owner. Approval completion rechecks current policy and expiry. Do not hold a storage mutex across an unbounded human prompt.
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
3. Add invite create/inspect/revoke and join with explicit scope, redaction, approval and protected link input. Keep the default CLI build usable without silently enabling browser/server features.
4. Persist device identity and enrollment/profile references; consume them through one existing V2 hosted/client path after CLI exit and restart.
5. Prove concurrent same/different identity redemption, lost response after commit, operator/device crash points, revoke during approval, corruption and saturation. A second redemption is not an implicit grant reissue.

**Exit:** Two clean participants enroll through a link without manual PSK handling; restart and retry recover the same identity/result; an unapproved or mismatched requester cannot get application authority. No org/subnet success is claimed yet.

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
| E1 | Mesh link joins a clean device without hand-installed PSK; wrong issuer/pin/domain fails. |
| E2 | Inspect/preview leaves nonce and stores untouched; revoked/expired/altered link cannot issue credentials. |
| E3 | Two concurrent redeemers produce one bound identity; same-identity lost-response retry returns committed receipt; crash/restart cannot duplicate issuance. |
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
