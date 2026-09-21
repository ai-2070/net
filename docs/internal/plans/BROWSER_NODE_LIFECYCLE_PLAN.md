# `@net-mesh/browser` Node Lifecycle Implementation Plan

> **For Hermes:** Use `subagent-driven-development` to implement this plan task-by-task. Each slice requires a spec review and then a code-quality review before the next slice starts.

**Goal:** Give browser applications one obvious way to bring an origin-scoped Net node up, with or without caller-supplied bootstrap secret material, inspect it, join and leave mesh/organization/channel/subnet relations through bounded invitations, and bring it down without collapsing those independent authority domains.

**Architecture:** `up()` is a small high-level façade over the existing one-node-per-origin `openSession()` path, not a second browser node implementation. The browser never accepts a naked PSK: it either receives an opaque `BrowserBootstrapCredential` or asks an application-provided credential source for one when the elected leader actually needs to bootstrap. A shared signed invitation envelope and durable relation ledger drive mesh, organization, channel and subnet join/leave, while each domain installs and enforces its own native credential type. `down()` performs observable, idempotent teardown while preserving identity and relation intent by default; leaving a relation and erasing identity are explicit, separate operations.

**Tech stack:** TypeScript 6, Vitest 5, `wasm-bindgen`, Rust `net-mesh-leaf`, Web Locks, BroadcastChannel, IndexedDB, WebCrypto, WebRTC DataChannels.

**Baseline:** `046e2959e0335875f71fcf884bcbc064b1bbd582` on `LZL0/net-cli`.

---

## 1. Scope and product contract

The browser equivalent of:

```text
net-mesh up [--psk-from <SOURCE>]
net-mesh down
net-mesh node status
```

is:

```ts
import { up } from '@net-mesh/browser';

const node = await up({
  bootstrap: { credential },
  capabilities: ['camera.preview'],
});

node.status();
await node.down();
```

or, when the page must not receive secret material until this origin's elected leader needs it:

```ts
const node = await up({
  bootstrap: {
    credentialFrom: async ({ signal }) => {
      const response = await fetch('/api/net/bootstrap-credential', {
        method: 'POST',
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { 'content-type': 'application/json' },
        body: '{}',
        signal,
      });
      if (!response.ok) throw new Error(`credential issuer refused: ${response.status}`);
      return new Uint8Array(await response.arrayBuffer());
    },
  },
});
```

`credentialFrom` is the browser analogue of `--psk-from`: it is an indirection that resolves secret bootstrap material at the moment it is needed. It can call a same-origin backend backed by a KMS, an authenticated issuer, or another application-owned secret source without teaching this package any KMS vendor API.

### Required modes

| Mode | Caller supplies | Result |
|---|---|---|
| Opaque credential | `bootstrap.credential` | The elected leader bootstraps immediately with the existing signed browser credential. |
| Deferred credential | `bootstrap.credentialFrom` | Only the elected leader invokes the source; followers do not fetch or redeem credentials merely by opening. A promoted follower invokes it when it becomes leader. |
| Existing compatibility surface | `credential` or `credentialB64` | Continues to work through `openSession()` and `connect()`; internally normalized into the opaque-credential path. |
| No raw caller PSK | no `psk`, `pskHex`, or `pskFrom` field | Supported through `credentialFrom`; the page need not possess or persist a standalone PSK. |

### What “without a PSK key” means here

It means **without a caller-supplied raw PSK**. It does not mean a PSK-free Noise handshake.

The current browser bootstrap credential contains all of the bound material needed for first contact:

- the single-use enrollment invite;
- the anchor's pinned Noise static public key;
- the trust domain's 32-byte NKpsk0 PSK;
- the HTTPS bootstrap URL;
- independent invite and standing-PSK expiries;
- the issuer identity and signature.

A naked PSK cannot replace that object: it does not identify or pin an anchor, carry an enrollment nonce, or prove issuer authorization. `@net-mesh/browser` therefore must **not** add `psk`, `pskHex`, `pskB64`, or a browser-side KMS client. Applications that possess a raw PSK must mint a `BrowserBootstrapCredential` on a trusted server or native node and hand the browser only the resulting opaque credential.

A completely empty `up()` is not a connected-node mode. A browser cannot listen for arbitrary UDP peers or act as its own native anchor. This plan does not pretend otherwise. If a future product needs an offline/unattached browser identity before enrollment, that is a separate state-machine plan; it must not be presented as an online Net node.

### Authority boundaries

The implementation and documentation must keep these facts separate:

1. `up()` starts or attaches this tab to the origin's node lifecycle.
2. A valid bootstrap credential permits first contact and enrollment with its named anchor.
3. Enrollment establishes transport reachability; it does not grant every capability.
4. `capabilities` publishes names; it does not create invocation authority.
5. Provider-local policy and capability grants remain final for invocation.
6. `down()` disconnects; it does not revoke the origin identity or every grant issued to it.
7. `forgetIdentity()` erases local identity material; it still does not revoke copies of grants held elsewhere. Remote revocation is an authority operation, not storage cleanup.

---

## 2. Current state and exact gaps

### Already implemented

- `connect(options)` creates a tab-local `BrowserNode`.
- `openSession(options)` enforces one node per origin using a Web Lock and followers over `BroadcastChannel`.
- `MeshSession.close()` and `BrowserNode.close()` synchronously fence local use and close streams/events/inner handles.
- `openSession()` persists an origin identity in IndexedDB and restores subscriptions/capabilities after leader handoff.
- `ConnectOptions` accepts `credential`, `credentialB64`, `bootstrapUrl`, identity injection, ICE configuration, and failure-typing options.
- The credential pins the anchor Noise key and contains the trust-domain PSK; the leaf validates both independent expiries.
- Existing errors distinguish identity, control-plane, RTC, RPC, session, and leader failures.

### Missing or misleading today

- There is no explicit browser `up()`/`down()` lifecycle pair parallel to the CLI surface.
- `openSession()` requires resolved credential bytes before the leader election, so an application cannot safely defer a one-time or expensive credential source to the elected leader.
- A follower can be forced to obtain secret material even though it will not bootstrap.
- There is no stable lifecycle status object for UI, diagnostics, or automation.
- `close()` is immediate and synchronous; it does not expose a shared shutdown promise or distinguish graceful shutdown from local emergency fencing.
- There is no explicit identity-erasure operation or contract explaining what shutdown preserves.
- The README exposes `credentialB64` directly in the happy path, encouraging applications to treat the encoded credential as ordinary configuration.
- There are no tests proving that credentials are never persisted into IndexedDB, localStorage, URLs, logs, thrown error text, or lifecycle events.
- Abort-driven ownership for SPA/component scopes is not defined.

---

## 3. Public API

### 3.1 Bootstrap input

Add a discriminated source while retaining the legacy fields on `ConnectOptions`:

```ts
export interface CredentialContext {
  /** Aborts when startup is cancelled, the session is closed, or leadership is lost. */
  readonly signal: AbortSignal;
  /** The origin whose persistent identity is being bootstrapped. */
  readonly origin: string;
  /** Stable public fingerprint of that identity; never secret key material. */
  readonly identityFingerprint: string;
  /** Why a credential is needed. */
  readonly reason: 'initial' | 'leader-promotion' | 'reconnect';
}

export type CredentialValue = string | Uint8Array;
export type CredentialSource =
  (context: CredentialContext) => Promise<CredentialValue>;

export type BrowserBootstrap =
  | {
      readonly credential: CredentialValue;
      readonly credentialFrom?: never;
      /** Required for automatic promotion/reconnect after the one-shot value was used. */
      readonly refreshFrom?: CredentialSource;
    }
  | {
      readonly credential?: never;
      readonly credentialFrom: CredentialSource;
      readonly refreshFrom?: never;
    };
```

Rules:

- Exactly one of `bootstrap.credential` or `bootstrap.credentialFrom` is present.
- `up()` requires `bootstrap`; it never silently creates a trust domain that no anchor shares.
- A `Uint8Array` is the canonical raw `BrowserBootstrapCredential` wire bytes, not UTF-8 text and not already-base64 data. Normalize it exactly as `"net-bootstrap:" + base64url_no_pad(bytes)`, copy it into an owned bounded buffer, and zero the temporary copy after crossing into wasm where practical. Do not use generic padded/base64 encoding.
- A string must already be the canonical `net-bootstrap:` plus URL-safe unpadded-base64 form. Decode and re-encode equality is required; error text reports shape/version/expiry without reproducing the string.
- `credentialFrom` is invoked by the lock holder only.
- `credentialFrom` serves initial acquisition and later promotion/reconnect attempts. A static `credential` is one-shot: once its handle has attempted bootstrap, automatic promotion/reconnect is disabled unless `refreshFrom` exists. A promoted follower invokes its own deferred source; it never reuses a consumed static value.
- Concurrent demand for the same generation shares one in-flight promise. It is not invoked once per call.
- Any source invocation, credential parse, or enrollment/admission failure parks that handle in terminal `failed`/`credential-required` state and suppresses later promotion-loop callback invocation. Web-Lock contention and healthy-follower waiting may requeue without calling a source. A fresh attempt requires explicit `down()` followed by `up()`; static mode must supply a fresh credential and deferred mode may reuse its source by explicit caller choice.
- Rejection is typed as `IdentityError` for malformed/expired credentials and `ControlPlaneError` for issuer/network failures only when that distinction is actually established.

### 3.2 Lifecycle façade

```ts
export interface UpOptions extends Omit<
  SessionOptions,
  'credential' | 'credentialB64' | 'origin' | 'lockScope' | 'dbName'
> {
  readonly bootstrap: BrowserBootstrap;
  /** Closing this signal brings this caller's handle down. */
  readonly signal?: AbortSignal;
}

export interface BrowserNodeStatus {
  readonly phase:
    | 'starting'
    | 'leader'
    | 'follower'
    | 'reconnecting'
    | 'credential-required'
    | 'stopping'
    | 'stopped'
    | 'failed';
  readonly role: 'leader' | 'follower' | null;
  readonly generation: string | null;
  readonly nodeIdHex: string | null;
  readonly enrolled: boolean | null;
  readonly identityFingerprint: string;
  readonly scope: string;
  readonly lastError: LeafError | null;
}

export async function up(options: UpOptions): Promise<MeshSession>;
```

`up()` resolves only when this tab is attached as a follower or the elected leader is connected and enrolled. It delegates all node operations to the existing `MeshSession`; it does not construct a second event hub, election, or transport.

The high-level façade derives its identity/election scope from the browser's actual `location.origin` and the package-owned canonical identity database. It rejects or omits caller-controlled `origin`, `lockScope`, and `dbName`. Those overrides remain only on the documented lower-level/testing entry points and cannot be used to make two `up()` leaders over one stored identity.

Add to `MeshSession`:

```ts
status(): BrowserNodeStatus;
onStatus(handler: (status: BrowserNodeStatus) => void): Unsubscribe;
down(options?: { deadlineMs?: number }): Promise<void>;
```

Compatibility:

- `openSession(options)` remains supported and is documented as the lower-level pre-resolved-credential entry point.
- `close()` remains synchronous, idempotent emergency/local teardown.
- `down()` is the preferred lifecycle operation for code that can await completion.
- `connect()` remains the deliberate tab-local/testing escape hatch and does not become origin-scoped implicitly.

### 3.3 Shutdown contract

`down()` must:

1. Transition the handle to `stopping` exactly once.
2. Abort any in-flight credential acquisition and startup work owned by this handle.
3. Fence new calls, subscriptions, publishes, streams, announcements, and peer attempts immediately.
4. End every stream and settle every local waiter observably.
5. If this tab is the leader, stop accepting follower work before releasing leadership.
6. Close RTC peers, control-plane sockets, event pumps, timers, and listeners.
7. Release this tab's Web Lock/follower attachment.
8. Resolve only when local teardown is complete or reject with an `AggregateError` containing typed failures.
9. Be idempotent: teardown has one underlying completion promise. A caller-specific `deadlineMs` races that completion and rejects with typed `ShutdownTimeoutError` if it expires; expiry never aborts or rolls back teardown, and a later/no-deadline caller may still await the same completion.
10. Leave the encrypted origin identity in IndexedDB by default.
11. Never persist a bootstrap credential or its PSK during shutdown.

`down()` does **not** promise a network-wide synchronous capability withdrawal. A sudden page close cannot await one, and peers may retain signed announcements until their existing expiry/refresh rules remove them. If an empty/versioned announcement is already the canonical withdrawal mechanism, a bounded best-effort withdrawal may run before transport close; the implementation must first prove that semantic in an existing native test. This plan does not create a new wire message solely for browser shutdown.

The current synchronous Rust/WASM `close()` is not an awaitable completion boundary. Add a narrow completion primitive that resolves only after promotion cancellation, RTC/control-plane closure, follower detachment, and Web-Lock release. `close()` fences immediately and starts that same teardown; `down()` awaits it. Deadline expiry leaves fencing and background teardown in force rather than making the handle usable again.

`pagehide`, browser crash, process kill, and tab discard use immediate local cleanup and normal Web Lock release. No document may claim that `beforeunload` or `unload` completed asynchronous network teardown.

### 3.4 Identity erasure

Add a separate static operation, not a flag on ordinary shutdown:

```ts
export async function forgetIdentity(): Promise<void>;
```

Contract:

- Every high-level session holds a shared **identity attachment lease** for its entire lifetime, including followers. The lease name is derived only from actual `location.origin` and the canonical package database, never caller `lockScope`.
- `forgetIdentity()` attempts the corresponding exclusive lease with no wait and returns a typed live-attachment refusal while any leader or follower holds it. The existing leader-election Web Lock is insufficient because followers only queue for that lock.
- Lower-level custom `lockScope` sessions still hold the canonical attachment lease, so an override cannot evade erasure fencing.
- Deletes the encrypted identity record and wrapping key atomically enough that the next `up()` cannot load half of an old identity.
- Does not claim remote revocation.
- Does not delete unrelated application IndexedDB state.
- Is explicit enough that accidental component unmount cannot call it through `down()`.

If the existing database layout cannot delete only this identity safely, add `IdentityVault::forget()`; do not call `indexedDB.deleteDatabase()` unless the database is documented as package-exclusive and blocked/versionchange behavior is tested.

### 3.5 Invitation and relation lifecycle

The browser needs the same human journey for every relation:

```ts
const preview = await inspectInvite(joinLink);
renderConsent(preview);                    // no redemption effect

const joined = await node.join(joinLink);  // explicit POST/nRPC redemption
const relations = await node.relations();

await node.leave(joined.relation);
```

An authority-bearing browser may also create an invitation through the authority's typed service:

```ts
const invitation = await node.invite({
  kind: 'channel',
  target: {
    publisher: publisherEntityId,
    channel: 'orders.shipments',
  },
  rights: ['subscribe'],
  expiresInMs: 15 * 60_000,
});
```

`invite()` does not place an organization root key, subnet root key, channel token-root key, or mesh root PSK in JavaScript. It invokes an explicit authority service under separately verified issuer/delegation authority. A browser without that authority receives a typed refusal.

#### Shared invitation envelope

Use one versioned, signed, size-bounded envelope for routing and consent, then dispatch to a domain-specific redeemer. The envelope binds:

```ts
export type RelationKind = 'mesh' | 'organization' | 'channel' | 'subnet';

export interface InvitePreview {
  readonly version: 1;
  readonly kind: RelationKind;
  readonly issuer: string;
  readonly target: RelationTarget;
  readonly rights: readonly string[];
  readonly expiresAt: string;       // exact decimal Unix milliseconds
  readonly oneTime: true;
  readonly requiresApproval: boolean;
  /** Signature/integrity result only; never a claim of current authority. */
  readonly verification: 'verified' | 'unverified' | 'invalid';
  readonly warnings: readonly string[];
}
```

The signed bytes also bind a random invitation ID/nonce, exact redemption locator, issuer key, subject-binding policy and any domain-specific scope. The rendered link contains only the bounded signed envelope or an opaque one-time locator. It never contains a standing PSK, private key, organization audience key, channel token-root secret, subnet authority seed, or reusable bearer grant.

Rules common to all domains:

- `inspectInvite()` is offline/read-only where possible. GET, HEAD, link previews, email scanners and unfurlers cannot redeem.
- `join()` requires an explicit application/user action and performs redemption with POST or a typed Net invocation.
- Redemption binds the complete invitation intent to this origin's already-persisted entity identity before any credential is issued.
- The authority durably records the winning subject and result before returning secret-bearing or authoritative material.
- Same-identity retries return the committed result or resume its incomplete stages; another identity cannot take over the nonce.
- Invalid proof, malformed input and inspection do not consume or reserve the invitation.
- A partial multi-domain bundle reports each committed stage. It never claims distributed atomicity and never “rolls back” a credential already issued by deleting browser state.
- Invitation expiry is short. Standing credential expiry is independent and explicit.
- `leave()` first persists local disabled intent and fences delayed join/renew callbacks; authority notification is a separate result.
- `remove`/revoke is an issuer operation and is not an alias for voluntary `leave()`.

#### Mesh relation

A mesh invitation establishes first-contact bootstrap and enrollment for the existing browser identity.

- A clean browser may redeem it over authenticated HTTPS before it has a Net route.
- The link itself does not carry the standing trust-domain PSK. Successful explicit redemption returns the existing opaque `BrowserBootstrapCredential` over TLS.
- The credential's single-use enrollment nonce and standing PSK expiry remain independent.
- `join()` reports success only after the browser is connected and enrollment is live, not merely after downloading a credential.
- Mesh leave disables automatic bootstrap/reconnect for that enrollment and stops controlled participation. It preserves the entity identity and does not claim to revoke a shared PSK already learned.

#### Organization relation

An organization invitation installs belonging only:

- redemption issues an `OrgMembershipCert` for the proven browser entity and commits the corresponding local owner/relation metadata;
- it never silently issues an `OrgDispatcherGrant`, `OrgCapabilityGrant`, invocation proof, audience key or owner transfer;
- a node has one owner root in v1; joining a conflicting owner organization fails rather than replacing ownership;
- cross-organization use is represented by grants, not second ownership membership;
- organization leave disables renewal and local use of that membership and dependent owner-scoped projections, while preserving identity, historical records and unrelated grants;
- leave does not authorize adoption by another organization and does not remotely revoke the certificate unless the authority separately commits a supported self-revocation.

Invariant:

```text
OrgMembershipCert != dispatch authority != provider capability grant
```

#### Channel relation

A channel invitation grants bounded data-plane participation. It is not a synthetic organization or subnet membership.

The exact target binds:

- publisher/authority identity;
- canonical channel name;
- canonical `ChannelHash` (`u64`), never the 16-bit wire routing hint;
- rights: `subscribe`, `publish`, and optionally bounded `delegate` only when the issuer explicitly holds and grants it;
- expiry, delegation depth and token-root identity.

Redemption returns the existing `PermissionToken`/`TokenChain` form rooted in a `ChannelConfig.token_roots` authority. A self-issued token is not accepted merely because its signature verifies. Joining may install the token and create a live subscription only if `subscribe` was granted; publishing remains independently checked. Channel leave:

1. persists disabled auto-subscribe/renew intent;
2. unsubscribes this tab/origin claim through the existing shared subscription reference count;
3. stops local publish use under that relation;
4. removes the active token reference when no other local relation depends on it;
5. preserves unrelated channel tokens and subscriptions;
6. reports remote revoke separately.

`unsubscribe()` alone is ephemeral runtime state and is not durable channel leave. Conversely, holding a channel token does not grant org dispatch, subnet attachment or provider effects.

Invariant:

```text
channel subscribe/publish authority != org membership != subnet attachment != service invocation
```

#### Subnet relation

A subnet invitation targets an exact authority-qualified `SubnetRef` and the proven browser entity.

- Default rights are endpoint `ATTACH` only.
- `ATTACH` grants a subnet-scoped transport session and ordinary traffic; it grants no channel access, capability invocation, file access, administration or broader discovery.
- `ROUTE`, `EXPORT` and `DELEGATE` are omitted by default and require explicit issuer authority, consent text and acceptance witnesses.
- Successful join requires installed credentials **and** live challenge-bound subnet admission. A downloaded `SubnetGrant` is not joined status.
- Subnet leave disables renewal/re-attachment and withdraws that exact attachment while preserving other subnets, organization relations, channel grants and identity.
- Authority-side removal requires the actual selective subject-revocation mechanism; a local ledger deletion or disconnect is not revocation.

Invariants:

```text
SubnetGrant != service invocation authority
Subnet ATTACH != channel access
Subnet EXPORT != provider authorization
```

#### Browser relation ledger

Persist a ledger beside the existing identity vault, encrypted under the same origin trust boundary but in separate records. Each entry contains only what restart/renew/leave needs:

```ts
export interface RelationStatus {
  readonly id: string;
  readonly kind: RelationKind;
  readonly target: RelationTarget;
  readonly state:
    | 'joining'
    | 'active'
    | 'left-locally'
    | 'stop-unconfirmed'
    | 'notification-pending'
    | 'revoked'
    | 'expired'
    | 'failed';
  readonly credentialExpiresAt: string | null;
  readonly liveAdmission: 'verified' | 'not-required' | 'unknown' | 'denied';
  readonly lastError: LeafError | null;
}
```

The ledger stores canonical credentials only when the domain requires restart persistence; secret-bearing records use encrypted storage and never appear in JSON diagnostics. It separately records local intent, installed credential generation, live observation and remote notification/revocation state. A row is not evidence that admission is live.

Public surface:

```ts
inspectInvite(link: string): Promise<InvitePreview>;

interface MeshSession {
  invite(request: CreateInviteRequest): Promise<CreatedInvite>;
  join(link: string, options?: { signal?: AbortSignal }): Promise<JoinResult>;
  leave(relation: RelationRef, options?: { deadlineMs?: number }): Promise<LeaveResult>;
  relations(): Promise<readonly RelationStatus[]>;
}
```

`join()` is serialized per invitation and identity. `leave()` is serialized per relation incarnation. A delayed callback from an old join/leave cannot activate, disable or revoke a newer explicit relation.

---

## 4. Security and secret-handling rules

1. Do not expose a raw-PSK public option.
2. Do not put credentials or PSKs in URLs, query strings, fragment identifiers, environment variables, DOM attributes, analytics, lifecycle events, `Debug`, or exception messages.
3. Do not persist browser bootstrap credentials or transport PSKs in IndexedDB, localStorage, sessionStorage, Cache Storage, or service-worker state. Domain relation credentials follow the explicit encrypted relation-ledger rules in §3.5 and must never be reused as transport bootstrap material.
4. Browser devtools can observe JavaScript memory and HTTPS requests from the same origin. Document the actual boundary: XSS on the origin owns browser identity and credential access.
5. `credentialFrom` receives an `AbortSignal`; late results after cancellation are discarded and not used to bootstrap a successor generation.
6. Bound returned credential size before decoding or copying into wasm. Reuse the credential format's existing maximum rather than inventing a looser TypeScript-only ceiling.
7. No automatic source reinvocation after any credential-source, parse, or enrollment/admission failure. Mark that handle's promotion attempt terminal and cancel `PROMOTION_RETRY_MS` requeue for it. A consumed-invite refusal in particular must cause no later callback invocation. Recovery is a caller-visible `down()` followed by an explicit new `up()`; backend/generation failures that occur before credential acquisition may retain the existing automatic follower requeue behavior.
8. Followers never receive credential bytes over `BroadcastChannel` or `MessagePort`.
9. Status values and diagnostics may expose public node/fingerprint/anchor identifiers, never credential data.
10. `bootstrap.credential` and legacy `credential`/`credentialB64` are mutually exclusive; conflicting secret inputs fail before any network effect.
11. Supplied custodial identity and persisted identity remain mutually exclusive under the existing rules. Do not silently replace one with another.
12. Starting, joining, announcing, invoking, delegating, and revoking remain separate operations.
13. Join links never carry standing PSKs or reusable grants. A link may carry a signed bounded invitation or opaque nonce/locator only.
14. GET, HEAD, link previews and `inspectInvite()` are effect-free. Redemption requires an explicit POST or typed invocation after user/application consent.
15. Organization roots, subnet roots and channel token-root private keys remain outside browser JavaScript. Browser invitation creation uses a separately authorized service or bounded delegated issuer.
16. Channel policy uses canonical `u64` channel identity and the configured token root; never widen a 16-bit wire hint into an authority key.
17. A relation ledger entry is local state, not proof of live admission or remote revocation.
18. Leaving one relation cannot erase or deactivate unrelated relations merely because they share an identity, anchor, channel subscription or storage database.
19. Late join, renewal, leave and notification callbacks are incarnation-bound; stale completions fail closed against a newer explicit relation.
20. Multi-domain invitations preserve separate result and authority records. A successful mesh stage cannot make a failed org/channel/subnet stage appear joined.

---

## 5. Browser lifecycle behavior

### Multiple tabs

- One origin identity has one elected node.
- Every `up()` caller obtains a handle to that origin session.
- A follower does not resolve `credentialFrom` while a healthy leader exists.
- When a deferred follower promotes, it resolves a fresh credential with `reason: 'leader-promotion'`. A static-credential follower without `refreshFrom` does not attempt promotion: it enters `credential-required`, invokes no issuer, and requires explicit down/new-up with fresh bootstrap material.
- If credential acquisition or enrollment fails during promotion, extend the Rust promotion state machine with a terminal parked disposition. Do not feed that failure back into the existing unconditional `PROMOTION_RETRY_MS` loop; only pre-acquisition backend/generation failures may requeue automatically.
- Closing one follower removes only that tab's capability/subscription claims. It does not stop the leader or other followers.
- Calling `down()` on the leader stands that tab down; a remaining follower may promote and keep the origin node alive. Therefore `down()` means “release this handle/tab,” not “force every tab on this origin offline.”
- A future all-tabs administrative stop would require a separately authorized origin-wide broadcast and is out of scope.

### Backgrounding, freezing, and navigation

- `visibilitychange` is not shutdown. Hidden tabs remain valid.
- A frozen leader may retain its Web Lock; existing `rpc-indeterminate` behavior remains observable and is not masked by retries.
- `pagehide` performs immediate `close()` for this handle where the runtime still allows tasks; Web Lock release on document destruction remains the final recovery.
- No service worker ownership is added: `RTCPeerConnection` availability and lifetime are not portable enough to make a service worker the node owner.
- Dedicated Worker/SharedWorker migration is out of scope until the target browser matrix proves `RTCPeerConnection` and the required lock/identity APIs there.

### Failure states

- Startup never reports `leader` before enrollment succeeds.
- An expired credential, consumed invite, issuer refusal, anchor-key mismatch, ICE timeout, UDP-blocked evidence, and unavailable browser primitive retain their current distinct typed errors.
- A startup failure transitions to `failed`, emits one terminal status, runs teardown, and releases the lock.
- A static handle that lacks fresh promotion material transitions to `credential-required` without acquiring/consuming its old credential. A deferred acquisition/admission refusal transitions terminally and produces no delayed retry callback.
- A caller may call `down()` after startup failure; it remains idempotent.
- No failed startup silently falls back to a generated public/zero PSK, another origin identity, relay-only authority, or a different anchor.

---

## 6. Non-goals

- A PSK-free Noise protocol or new handshake pattern.
- A browser acting as a native UDP anchor.
- KMS vendor SDKs in `@net-mesh/browser`.
- Persisting bootstrap credentials for silent long-term login.
- Global email-to-agent discovery or identity lookup.
- Automatic capability grants from enrollment.
- Treating a subnet as a synthetic channel or treating a channel subscription as durable org/subnet membership.
- Loading organization roots, subnet roots, channel token-root secrets or a mesh root PSK into browser JavaScript.
- Authority-side `remove`/revocation as an alias for voluntary browser `leave()`.
- A universal “membership” credential spanning mesh, organization, channel and subnet authority.
- Service-worker ownership of WebRTC.
- Origin-wide forced shutdown across every tab.
- New wire messages for capability withdrawal.
- Replacing `connect()` or the existing browser/store APIs.
- Changing the game-store authority model.
- Turning page close into remote identity revocation.

---

## 7. Implementation slices

### Task 1: Pin the lifecycle and secret-source contracts in TypeScript tests

**Objective:** Establish the public API and reject ambiguous/raw-secret modes before implementation.

**Files:**
- Modify: `net/crates/net/browser-ts/src/node.ts`
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Create: `net/crates/net/browser-ts/test/lifecycle.test.ts`

**Step 1: Add compile-time/public-shape tests**

Test:

- `up({ bootstrap: { credential } })` is accepted.
- `up({ bootstrap: { credentialFrom } })` is accepted.
- neither source is rejected;
- both sources are rejected;
- raw `psk`, `pskHex`, and `pskB64` are not public options;
- high-level `origin`, `lockScope`, and `dbName` overrides are rejected while lower-level `openSession()` retains its documented testing/advanced overrides;
- static credentials typecheck with optional `refreshFrom`, and static-without-refresh is explicitly non-promotable after use;
- legacy `openSession({ credentialB64 })` still typechecks.

**Step 2: Run the test and prove RED**

```bash
cd net/crates/net/browser-ts
npm test -- --run test/lifecycle.test.ts
```

Expected: fail because `up`, `BrowserBootstrap`, lifecycle status, and `down()` do not exist.

**Step 3: Add types and exported stubs only**

Add the interfaces from §3 with methods that deliberately throw a test-visible “not implemented” error. Do not add a second session implementation.

**Step 4: Run the test and prove the shape GREEN**

Expected: type surface passes; behavioral tests remain RED.

**Step 5: Commit**

```bash
git add net/crates/net/browser-ts/src net/crates/net/browser-ts/test/lifecycle.test.ts
git commit -m "test(browser): pin node lifecycle surface"
```

### Task 2: Normalize legacy and new bootstrap inputs without leaking secrets

**Objective:** Build one bounded credential resolver shared by `connect`, `openSession`, and `up`.

**Files:**
- Modify: `net/crates/net/browser-ts/src/node.ts`
- Modify: `net/crates/net/browser-ts/src/errors.ts`
- Modify: `net/crates/net/browser-ts/test/node.test.ts`
- Modify: `net/crates/net/browser-ts/test/lifecycle.test.ts`
- Modify or create the exact built-WASM credential witness under: `net/crates/net/leaf/tests/`

**Step 1: Add RED tests**

Cover:

- canonical raw credential bytes normalize to exactly `net-bootstrap:` plus URL-safe unpadded base64, and canonical string input decodes to the same bytes;
- padded/standard-alphabet, double-encoded, UTF-8-text-as-bytes, and non-canonical string forms fail before bootstrap;
- conflicting legacy/new sources fail before `loadLeafWasm` or fetch;
- missing source fails without printing the option object;
- malformed credential errors contain no credential substring;
- oversized credentials fail before wasm;
- source rejection text does not gain serialized callback/context data;
- a late credential result after abort is discarded.

**Step 2: Run focused tests and verify RED**

```bash
npm test -- --run test/node.test.ts test/lifecycle.test.ts
```

**Step 3: Implement the normalizer**

Keep secret values out of returned diagnostics. Preserve the existing omission rule for optional WASM fields: absence stays absent rather than becoming an explicit empty/default value.

**Step 4: Run focused tests and verify GREEN**

Expected: all selected tests pass. Also run a real built-WASM positive witness; fake-WASM equality alone cannot prove the leaf accepts the exact normalized representation.

**Step 5: Commit**

```bash
git add net/crates/net/browser-ts/src/node.ts net/crates/net/browser-ts/src/errors.ts net/crates/net/browser-ts/test
git commit -m "feat(browser): normalize bootstrap credential sources"
```

### Task 3: Move deferred acquisition behind leader ownership

**Objective:** Ensure followers do not resolve, receive, or redeem bootstrap credentials.

**Files:**
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/leaf/src/wasm.rs`
- Modify: `net/crates/net/browser-ts/src/leader/wasm.ts`
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/test/fake-leader-wasm.ts`
- Modify: `net/crates/net/browser-ts/test/leader.test.ts`
- Create or modify the narrow wasm browser witness under: `net/crates/net/leaf/tests/`

**Step 1: Add RED witnesses**

Prove:

1. Two tabs call `up()` concurrently; exactly the elected leader calls `credentialFrom`.
2. The follower receives no credential bytes in proxy traffic.
3. Leader exit promotes a follower; the successor calls its own source once with `reason: 'leader-promotion'`.
4. A source result from a superseded generation is never used.
5. Closing during source acquisition aborts it and releases the Web Lock.
6. Source rejection releases the lock and leaves no half-live leader.
7. A consumed-invite/admission refusal parks promotion terminally; waiting beyond `PROMOTION_RETRY_MS` produces no second callback.
8. Static bootstrap without `refreshFrom` becomes `credential-required` instead of reusing bytes; static bootstrap with `refreshFrom` invokes only the refresh callback on promotion.

**Step 2: Run TypeScript tests and the selected wasm witness; verify RED**

Use the package test command plus the exact leaf browser-test command already pinned in CI. Do not substitute a native fake for the Web Lock property.

**Step 3: Extend the WASM boundary narrowly**

Pass a callable credential source into the leader lifecycle and invoke it only after the tab owns the lock and has loaded the identity fingerprint. Do not send the result through the follower protocol. Share one in-flight acquisition per generation.

Split promotion outcomes in Rust: pre-acquisition lock/backend/generation failure may return the existing requeue disposition; source/parse/enrollment/admission failure returns a terminal parked disposition that cancels automatic retry for that handle. A static source without `refreshFrom` returns `credential-required` before bootstrap. This is a deliberate change to `fall_back_to_follower()`/promotion scheduling, not TypeScript-only error labeling.

The callback context must contain only:

- abort signal/abort bridge;
- origin;
- public identity fingerprint;
- reason.

**Step 4: Run witnesses and verify GREEN**

The two-tab real-browser witness is required; a fake-WASM pass is insufficient for lock ownership.

**Step 5: Commit**

```bash
git add net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): acquire credentials only on the elected leader"
```

### Task 4: Implement `up()` as a façade over `openSession()`

**Objective:** Add the easy startup path without duplicating the node/session stack.

**Files:**
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Modify: `net/crates/net/browser-ts/test/lifecycle.test.ts`
- Modify: `net/crates/net/browser-ts/test/leader.test.ts`

**Step 1: Add RED behavior tests**

Prove:

- `up()` delegates to the same `MeshSession` implementation and method behavior;
- supplied capabilities/subscriptions restore through leader promotion;
- `up()` resolves only after leader enrollment or follower attachment;
- startup failure closes partial resources and releases election state;
- an already-aborted caller signal performs no network effect;
- abort after startup initiates teardown once;
- `up()` derives actual origin/canonical storage scope and cannot create concurrent leaders by varying caller origin, lock scope, or database name.

**Step 2: Run and verify RED**

```bash
npm test -- --run test/lifecycle.test.ts test/leader.test.ts
```

**Step 3: Implement the façade**

`up()` may normalize options and lifecycle ownership, but it must return the actual `MeshSession` and use the existing event/error/stream surfaces.

**Step 4: Run and verify GREEN**

Expected: the selected suites pass without changing existing `openSession` semantics.

**Step 5: Commit**

```bash
git add net/crates/net/browser-ts/src net/crates/net/browser-ts/test
git commit -m "feat(browser): add origin-scoped up lifecycle"
```

### Task 5: Add observable status

**Objective:** Give applications one immutable lifecycle reading and a change stream.

**Files:**
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/leaf/src/wasm.rs`
- Modify: `net/crates/net/browser-ts/src/leader/wasm.ts`
- Modify: `net/crates/net/browser-ts/src/leader/events.ts`
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Modify: `net/crates/net/browser-ts/test/events.test.ts`
- Modify: `net/crates/net/browser-ts/test/lifecycle.test.ts`

**Step 1: Add RED transition tests**

Pin legal transitions:

```text
starting -> leader
starting -> follower
starting -> failed
leader/follower -> reconnecting
reconnecting -> leader/follower
follower/reconnecting -> credential-required
leader/follower/reconnecting -> stopping -> stopped
credential-required -> stopping -> stopped
failed -> stopping -> stopped
```

Also prove:

- generation values remain exact decimal strings;
- no credential value appears in status or status-event serialization;
- duplicate lower-level events do not produce contradictory status;
- a subscriber sees snapshots, not a mutable shared object;
- terminal status emits once;
- promotion-start, terminal-promotion-failure, and enrollment-change are visible in the authoritative snapshot before TypeScript emits their projected status;
- synchronous status cannot report stale role, generation, or enrollment across a forced handoff race.

**Step 2: Run and verify RED**

**Step 3: Implement one Rust/WASM lifecycle snapshot and transition stream**

Add an atomic/coherent Rust-owned snapshot containing phase, role, exact generation, node ID and enrollment. Emit explicit promotion-start, enrollment-change and terminal-promotion-failure transitions. Expose a synchronous WASM snapshot read plus ordered transition events; TypeScript projects these immutable readings and adds only public identity/scope/error presentation. Do not infer `reconnecting` from timing or call asynchronous `isEnrolled()` inside a synchronous status method.

**Step 4: Run and verify GREEN**

**Step 5: Commit**

```bash
git add net/crates/net/browser-ts/src net/crates/net/browser-ts/test
git commit -m "feat(browser): expose node lifecycle status"
```

### Task 6: Implement idempotent `down()` and AbortSignal ownership

**Objective:** Provide awaitable teardown with immediate operation fencing and one shared result.

**Files:**
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/node.ts`
- Modify: `net/crates/net/browser-ts/src/stream.ts`
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/leaf/src/wasm.rs`
- Modify: `net/crates/net/browser-ts/src/leader/wasm.ts`
- Modify: `net/crates/net/browser-ts/test/lifecycle.test.ts`
- Modify: `net/crates/net/browser-ts/test/stream-ownership.test.ts`
- Modify: `net/crates/net/browser-ts/tests/abi_real_package.mjs`

**Step 1: Add RED teardown witnesses**

Prove:

- two concurrent `down()` calls share one promise;
- new operations fail immediately after `stopping` begins;
- open stream iterators settle;
- pending credential acquisition is aborted;
- pending follower requests settle with typed failures;
- RTC/control-plane handles close;
- leader release permits follower promotion;
- one follower's `down()` does not stop another tab;
- teardown failures aggregate and remaining teardown steps still run;
- identity remains loadable after ordinary shutdown;
- `close()` remains synchronous and idempotent;
- deadline expiry returns `ShutdownTimeoutError`, leaves the handle fenced, and allows teardown to finish; a later/no-deadline waiter observes the shared completion;
- completion is not reported until promotion cancellation, follower detachment and Web-Lock release; an immediate successor can then acquire and promote;
- a retained handle cannot act after timeout or address a successor generation.

**Step 2: Run and verify RED**

**Step 3: Implement teardown ordering**

Store one private teardown completion promise. Fence first, settle local consumers second, close transport/election third, await the new Rust/WASM completion boundary, then emit `stopped`. Race each caller's deadline around—not inside—that shared completion; timeout never cancels teardown. Never hold a mutable borrow or JS callback across an await in the WASM boundary.

**Step 4: Run and verify GREEN**

Include the real-package ABI witness so fake WASM cannot hide a missing close/fence method.

**Step 5: Commit**

```bash
git add net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): add idempotent down lifecycle"
```

### Task 7: Add explicit identity erasure

**Objective:** Separate stopping a node from deleting its persistent identity.

**Files:**
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/leaf/src/storage.rs`
- Modify: `net/crates/net/leaf/src/wasm.rs`
- Modify: `net/crates/net/browser-ts/src/wasm.ts`
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Create: `net/crates/net/browser-ts/test/identity-lifecycle.test.ts`
- Modify the relevant real-browser IndexedDB witness under: `net/crates/net/leaf/tests/`

**Step 1: Add RED tests**

Prove:

- ordinary `down()` preserves identity and node ID across restart;
- `forgetIdentity()` refuses while a session is live;
- a leader plus follower both hold shared attachment leases; erasure cannot acquire the exclusive lease after the leader exits while the follower remains;
- a lower-level session using custom `lockScope` still blocks canonical identity erasure;
- erasure removes only package identity records;
- next `up()` creates a different identity;
- blocked/versionchange IndexedDB deletion is typed and does not report success;
- a partial failure does not leave an identity record pointing at a missing wrapping key or vice versa;
- remote revocation is never reported.

**Step 2: Run and verify RED**

**Step 3: Implement the shared attachment lease, `IdentityVault::forget()`, and wrapper**

Acquire the canonical shared lease before attaching every leader or follower and hold it until that session's teardown completion. `forgetIdentity()` requests the same lease exclusively with `ifAvailable`; null means typed live-attachment refusal. This lease is distinct from leader election and ignores caller `lockScope`. Prefer deleting the exact records in one committed IndexedDB transaction. If identity and wrapping key are in different stores, use one transaction spanning both stores. Wait for transaction completion, not merely request success.

**Step 4: Run and verify GREEN**

**Step 5: Commit**

```bash
git add net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): separate identity erasure from shutdown"
```

### Task 8: Add secret-retention and browser-lifecycle adversarial tests

**Objective:** Falsify the security and lifecycle claims rather than relying on happy-path unit tests.

**Files:**
- Create: `net/crates/net/browser-ts/test/secret-retention.test.ts`
- Modify: `net/crates/net/browser-ts/test/lifecycle.test.ts`
- Modify: `net/crates/net/tests/rtc_browser/run.sh`
- Modify: `net/crates/net/tests/rtc_browser/run.ps1`
- Modify: `net/crates/net/tests/rtc_browser/driver/driver.mjs`
- Modify the exact lifecycle pages/scripts under: `net/crates/net/tests/rtc_browser/page/`
- Modify: `.github/workflows/ci.yml` only if a new test binary/script is introduced and the repository guard requires a pin.

**Step 1: Add adversarial cases**

- Search IndexedDB stores, localStorage, sessionStorage, Cache Storage, BroadcastChannel frames, status/events, thrown errors, and captured console lines for a unique credential marker.
- Freeze the leader while a follower waits; confirm no false takeover is reported and calls become `rpc-indeterminate` under the existing rule.
- Close during initial acquisition, initial ICE, enrollment, and leader promotion.
- Trigger `pagehide` and verify immediate local fencing without claiming remote withdrawal.
- Run concurrent `up()` from two tabs and assert one node ID/one leader.
- Promote after leader close and assert a fresh source call, not reuse of a consumed one-time credential.
- Return an oversized/malformed credential and prove no partial bootstrap effect.
- Force source completion after cancellation and prove its bytes are ignored.

**Step 2: Run the repository browser matrix with named witness floors**

Chromium and Firefox are gating engines. Run `run.sh --engine chromium` and `run.sh --engine firefox` (plus `run.ps1` where Windows CI owns it), pin every new `RTCB PASS` witness name and raise the exact per-engine floor. Run `--engine webkit` as the existing recorded best-effort WebKit leg; call it WebKit evidence, never Safari evidence. Do not turn an unavailable engine into a silent skip. Record exact browser/version and missing API for every non-gating result.

**Step 3: Repair until every required witness passes**

No new fallback semantics may be introduced solely to make one browser green.

**Step 4: Commit**

```bash
git add net/crates/net/browser-ts net/crates/net/leaf .github/workflows/ci.yml
git commit -m "test(browser): prove lifecycle and secret retention"
```

### Task 9: Replace credential-first documentation with lifecycle-first documentation

**Objective:** Make the safe, easy path the first path developers copy.

**Files:**
- Modify: `net/crates/net/browser-ts/README.md`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Modify: `net/crates/net/browser-ts/CHANGELOG.md`
- Modify relevant browser pages under: `web/src/content/docs/`

**Step 1: Add documentation examples**

The README opening example uses `up()` and `await node.down()`. Include:

- opaque credential mode;
- its one-shot/non-promotable behavior without `refreshFrom`;
- deferred same-origin issuer mode;
- status rendering;
- `AbortSignal` for component ownership;
- `openSession()` as the lower-level compatibility API;
- `connect()` as the tab-local advanced/test API;
- `forgetIdentity()` with an explicit revocation disclaimer.

**Step 2: State the non-guarantees plainly**

Document:

- no raw PSK option;
- no completely unconfigured connected browser node;
- `down()` is local teardown, not remote revocation;
- closing one tab need not stop the origin node if followers remain;
- page unload cannot promise graceful network withdrawal;
- same-origin JavaScript is inside the browser identity trust boundary.

**Step 3: Run docs/package checks**

```bash
cd net/crates/net/browser-ts
npm test
npm run build

cd ../../../../web
npm run check
```

Expected: tests, TypeScript declarations, package build, links, and docs checks all pass.

**Step 4: Commit**

```bash
git add net/crates/net/browser-ts web/src/content/docs
git commit -m "docs(browser): teach up and down lifecycle"
```

### Task 10: Pin the shared invitation envelope and browser authority boundary

**Objective:** Define one inspectable/redeemable invitation format without pulling the native `net` core, authority roots or duplicate cryptography into `net-mesh-leaf`.

**Files:**
- Create: `net/crates/net/sdk/src/relation_invite.rs`
- Modify: `net/crates/net/sdk/src/lib.rs`
- Create: `net/crates/net/browser-ts/src/relations.ts`
- Modify: `net/crates/net/browser-ts/src/index.ts`
- Create: `net/crates/net/browser-ts/test/relations/invite.test.ts`
- Modify: `net/crates/net/leaf/tests/dependency_boundary.rs`
- Add shared codec files to `net/crates/net/wire/` only if the source survey proves browser-side decoding cannot remain a bounded TypeScript parser over golden vectors.

**Step 1: Record the dependency decision before code**

The leaf is deliberately a separate workspace depending on `net-mesh-wire`, not the native `net` core. Source-check every credential needed by mesh/org/channel/subnet join and classify it as:

- browser must parse/verify;
- browser may carry opaquely while the native authority/verifier enforces;
- browser must sign a challenge with its existing entity key;
- browser must never receive it.

Prefer opaque carry plus native verification. If common canonical encoding/signature verification is genuinely required in the browser, extract the smallest audited wasm-clean authority codec; do not copy `PermissionToken`, org or subnet verification into TypeScript.

**Stop gate:** independent architecture review approves the exact portable boundary. No relation method is added until this passes.

**Step 2: Add RED golden-vector tests**

Cover all four kinds, exact expiry boundary, unknown version/kind, oversized envelope, non-canonical encoding, tampered signed bytes, ambiguous target, and effect-free inspection. Prove the preview contains no secret-bearing field.

**Step 3: Implement the versioned envelope and parser**

Use full canonical identifiers. For channel targets bind publisher plus canonical name and `u64 ChannelHash`; for subnet bind full authority plus exact `SubnetRef`; for org bind full `OrgId`; for mesh bind the exact enrollment authority/locator.

**Step 4: Prove GET/HEAD/inspect do not touch the redemption ledger**

A mutation test that marks inspection as spent must fail the named witness.

**Step 5: Commit**

```bash
git add net/crates/net/sdk net/crates/net/wire net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): define bounded relation invitations"
```

### Task 11: Add the durable browser relation ledger and generic join/leave state machine

**Objective:** Make join/leave resumable, scoped, incarnation-fenced and honest about local versus remote state.

**Files:**
- Create: `net/crates/net/leaf/src/relations.rs`
- Modify: `net/crates/net/leaf/src/storage.rs`
- Modify: `net/crates/net/leaf/src/lib.rs`
- Modify: `net/crates/net/leaf/src/wasm.rs`
- Create: `net/crates/net/browser-ts/src/relations.ts`
- Modify: `net/crates/net/browser-ts/src/leader/session.ts`
- Modify: `net/crates/net/browser-ts/src/leader/wasm.ts`
- Create: `net/crates/net/browser-ts/test/relations/lifecycle.test.ts`

**Step 1: Add RED state-machine tests**

Prove durable `joining → active`, local leave states, authority notification pending, expiry/revocation, same-identity resume, wrong-identity refusal, competing redemption, partial bundle results, restart, and old-callback fencing.

**Step 2: Add RED cross-tab tests**

Only the leader mutates the relation ledger or performs redemption/leave effects. Followers proxy requests and receive redacted results. A leader change during redemption resumes from the committed stage rather than replaying issuance.

**Step 3: Implement transactional ledger records**

Use IndexedDB transactions that wait for completion/abort. Store relation intent, credential generation/reference, live-admission evidence source/freshness and remote notification separately. Encrypt secret-bearing credential records under the existing origin storage boundary. Never serialize them through status/events.

**Step 4: Implement generic `join`, `leave`, `relations` dispatch**

The generic state machine owns idempotency and lifecycle only. Domain adapters own issuance, install, live admission, renewal, local fencing and notification.

**Step 5: Commit**

```bash
git add net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): add durable relation lifecycle"
```

### Task 12: Implement mesh and organization invite/join/leave adapters

**Objective:** Reuse existing browser bootstrap and organization credentials without promoting membership into invocation authority.

**Files:**
- Modify: `net/crates/net/sdk/src/enrollment.rs`
- Modify: `net/crates/net/sdk/src/bootstrap_credential.rs`
- Modify: `net/crates/net/sdk/src/rtc_bootstrap.rs`
- Modify: `net/crates/net/sdk/src/org.rs`
- Modify: `net/crates/net/leaf/src/bootstrap.rs`
- Modify: `net/crates/net/leaf/src/relations.rs`
- Create: `net/crates/net/browser-ts/test/relations/mesh.test.ts`
- Create: `net/crates/net/browser-ts/test/relations/org.test.ts`

**Step 1: Add mesh RED witnesses**

Prove explicit POST redemption, no standing PSK in the link, durable same-identity result, live enrollment before joined status, restart/reconnect, offline local leave, shared-PSK non-revocation disclosure and no automatic retry of a consumed nonce.

**Step 2: Add organization RED witnesses**

Prove membership-only issuance, exact entity binding, conflicting owner refusal, no dispatcher/capability grant or audience key in the result, leave preserving identity/unrelated grants, and membership alone failing a protected invocation.

**Step 3: Implement bounded authority services**

The native owner signs/mints. The browser proves its entity and receives only the opaque browser credential or `OrgMembershipCert`/public relation material it requires. Creation through `node.invite()` requires a separately authorized typed authority capability; no root private key crosses into the page.

**Step 4: Implement local leave adapters**

Persist disabled intent before stopping reconnect/renewal. Report local stop, live-stop uncertainty and authority notification independently.

**Step 5: Run inverse mutations**

- Put the standing PSK in the link: mesh witness fails.
- Add a dispatcher grant to org join: membership-only witness fails.
- Treat local leave as remote revocation: status witness fails.

**Step 6: Commit**

```bash
git add net/crates/net/sdk net/crates/net/leaf net/crates/net/browser-ts
git commit -m "feat(browser): add mesh and organization relation lifecycle"
```

### Task 13: Implement channel invite/join/leave

**Objective:** Turn the existing channel token gate into an explicit, durable browser participation flow without inventing ambient membership.

**Files:**
- Modify: `net/crates/net/src/adapter/net/identity/token.rs`
- Modify: `net/crates/net/src/adapter/net/channel.rs`
- Modify: `net/crates/net/sdk/src/identity.rs` or the current SDK token façade selected by source survey
- Modify: `net/crates/net/leaf/src/relations.rs`
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/browser-ts/src/relations.ts`
- Create: `net/crates/net/browser-ts/test/relations/channel.test.ts`
- Modify or create a native publisher integration witness under: `net/crates/net/tests/`
- Modify: `.github/workflows/ci.yml` if a new root integration binary is introduced.

**Step 1: Add RED authority tests**

Prove exact publisher, canonical channel name/hash, subject, right, expiry, generation and configured token root. Reject self-issued/untrusted roots, 16-bit hash collisions, wrong publisher, wrong right, expired/revoked token and widened delegated scope.

**Step 2: Add RED lifecycle tests**

Prove invite creation through an authorized channel issuer service, explicit redemption, subscribe-only versus publish-only behavior, leader handoff, durable disabled intent, `unsubscribe()` not equalling leave, leave of one relation preserving another tab/relation, and explicit rejoin requiring current authorization.

**Step 3: Implement opaque token-chain carriage**

Keep verification at the publisher's existing channel gate. The leaf carries canonical token bytes and relation metadata; it does not become an independent token authority or trust a locally self-issued signature.

**Step 4: Implement live subscription/publish fencing**

Channel join installs only granted rights. Leave decrements the existing per-origin subscription claim and prevents renewal/auto-subscribe before it removes the active token reference.

**Step 5: Run inverse mutation**

Replace canonical `ChannelHash` policy lookup with the 16-bit wire hint; the collision witness must fail.

**Step 6: Commit**

```bash
git add net/crates/net/src/adapter/net net/crates/net/sdk net/crates/net/leaf net/crates/net/browser-ts .github/workflows/ci.yml
git commit -m "feat(browser): add channel relation lifecycle"
```

### Task 14: Implement subnet invite/join/leave

**Objective:** Add endpoint attachment lifecycle while preserving the separation between topology, channels, invocation and provider policy.

**Files:**
- Modify: `net/crates/net/src/adapter/net/subnet/auth.rs`
- Modify: `net/crates/net/src/adapter/net/subnet/admission.rs`
- Modify: `net/crates/net/src/adapter/net/subnet/provision.rs`
- Modify: `net/crates/net/sdk/src/subnet.rs`
- Modify: `net/crates/net/leaf/src/relations.rs`
- Modify: `net/crates/net/leaf/src/leader_session.rs`
- Modify: `net/crates/net/browser-ts/src/relations.ts`
- Create: `net/crates/net/browser-ts/test/relations/subnet.test.ts`
- Modify: `net/crates/net/tests/subnet_auth_e2e.rs`
- Modify: `.github/workflows/ci.yml` if the existing pinned witness set changes.

**Step 1: Add RED grant/admission tests**

Prove exact authority-qualified `SubnetRef`, full subject identity, topology epoch, credential generation, rights attenuation, challenge-bound live admission, expiry and current revocation floor. Download/install alone is not joined.

**Step 2: Add RED least-authority tests**

Default invite issues `ATTACH` only. It cannot publish/subscribe to a protected channel, invoke a protected capability, route, export, delegate or administer. Explicit `ROUTE`/`EXPORT`/`DELEGATE` requests require matching issuer authority and distinct consent output.

**Step 3: Add RED leave tests**

Prove exact-subnet detach, disabled reattachment/renewal across restart, unrelated subnet/org/channel preservation, offline local completion with notification pending, and no false claim of issuer revocation.

**Step 4: Implement native verification and browser carriage**

The browser proves its entity and presents opaque credential material/challenge signatures through the leaf. Native subnet admission remains final. Do not import native gateway/admin machinery into the page.

**Step 5: Run inverse mutations**

- Treat installed grant as joined without live admission: witness fails.
- Add channel access to `ATTACH`: witness fails.
- Make local leave clear unrelated relations: witness fails.

**Step 6: Commit**

```bash
git add net/crates/net/src/adapter/net/subnet net/crates/net/sdk net/crates/net/leaf net/crates/net/browser-ts .github/workflows/ci.yml
git commit -m "feat(browser): add subnet relation lifecycle"
```

### Task 15: Document the four relation types as separate authority domains

**Objective:** Make invite/join/leave easy without teaching “joining Net grants everything.”

**Files:**
- Modify: `net/crates/net/browser-ts/README.md`
- Modify: `net/crates/net/browser-ts/CHANGELOG.md`
- Modify relevant pages under: `web/src/content/docs/`

**Step 1: Add one coherent browser journey**

Show `up`, `inspectInvite`, `join`, `relations`, scoped `leave`, `down` and `forgetIdentity` in that order. Include a composed mesh + org + subnet invitation with separate stage results, plus a standalone channel invitation.

**Step 2: Add an authority matrix**

State positively:

```text
mesh        -> reachability/enrollment
organization-> belonging
channel     -> publish/subscribe data-plane authority
subnet      -> attachment/routing/export topology authority
provider    -> effect/invocation policy
```

Then state the four invariants from §3.5. Avoid one generic “member” term across these rows.

**Step 3: Run package and docs checks**

Use Task 9's commands plus every new relation fixture and golden-vector test.

**Step 4: Commit**

```bash
git add net/crates/net/browser-ts web/src/content/docs
git commit -m "docs(browser): explain relation invitations and leave"
```

---

## 8. Acceptance matrix

| ID | Requirement | Evidence |
|---|---|---|
| BNL-1 | A page starts an origin-scoped node with an opaque browser bootstrap credential. | `lifecycle.test.ts` + real browser bootstrap witness. |
| BNL-2 | A page starts without caller-supplied raw PSK material by using `credentialFrom`. | Real issuer callback fixture; successful enrollment. |
| BNL-3 | Only the elected leader invokes `credentialFrom`. | Two-tab real-browser counter and follower traffic inspection. |
| BNL-4 | A deferred/refreshable promoted follower obtains one fresh credential and restores declared capabilities/subscriptions; static mode without `refreshFrom` parks as credential-required without reusing bytes. | Leader-close promotion witness with issuer counters. |
| BNL-5 | `up()` with no bootstrap source fails before network effects. | Fake-WASM call count remains zero. |
| BNL-6 | No raw PSK option exists and a raw PSK alone cannot bootstrap. | Type-level negative tests and README contract. |
| BNL-7 | A coherent Rust/WASM snapshot reports exact phase/role/generation/node/enrollment state without secrets or stale handoff projections. | Forced-handoff transition tests, snapshot/event ordering and marker scan. |
| BNL-8 | `down()` fences immediately, settles streams/waiters, and performs teardown once; deadline timeout leaves teardown running and an awaitable completion eventually proves lock release. | Concurrent shutdown, timeout, successor-promotion and stream ownership tests. |
| BNL-9 | One follower's shutdown does not terminate another tab's origin node. | Three-tab lifecycle witness. |
| BNL-10 | `down()` preserves identity; restart retains the same node identity. | IndexedDB restart witness. |
| BNL-11 | `forgetIdentity()` is excluded by every leader/follower shared attachment lease—including custom lower-level lock scopes—and creates a new identity only after explicit erasure. | Multi-tab/custom-scope Web-Lock plus IndexedDB erasure witness. |
| BNL-12 | Browser bootstrap credentials/transport PSKs do not persist or cross follower IPC; separately authorized encrypted relation credentials obey the relation ledger rather than this bootstrap ban. | Unique-marker scan across storage, IPC, logs, errors, and events. |
| BNL-13 | Abort during acquisition/bootstrap/enrollment releases ownership and leaves no half-live session. | Phase-by-phase cancellation tests. |
| BNL-14 | A credential-source, parse or enrollment/admission refusal is terminal for that promotion attempt; waiting beyond `PROMOTION_RETRY_MS` invokes no later callback. | Issuer counter + exact typed failure + delayed no-retry witness. |
| BNL-15 | Existing `connect()` and `openSession()` callers remain source-compatible. | Existing package suite unchanged plus declaration build. |
| BNL-16 | Chromium and Firefox pass named lifecycle witnesses and raised per-engine floors; the best-effort WebKit result is explicit and is never labeled Safari evidence. | `rtc_browser` CI artifacts with browser versions, witness names and counts. |
| BNL-17 | Documentation never equates enrollment, announcement, or transport PSK possession with invocation authority. | Documentation review against §1 authority boundaries. |
| BNL-18 | Mesh, organization, channel and subnet invitations share one effect-free inspection envelope but dispatch to separate credential/admission mechanisms. | Four golden vectors plus domain adapter tests. |
| BNL-19 | GET, HEAD, unfurl and `inspectInvite()` cannot consume an invitation. | Redemption-ledger before/after witness and inverse mutation. |
| BNL-20 | Same-identity retry resumes the committed result; another identity cannot redeem the same invitation. | Durable race/restart witness. |
| BNL-21 | Organization join issues belonging only and cannot invoke a protected capability without independent dispatcher/provider grants. | Org membership inverse admission test. |
| BNL-22 | Channel join binds exact publisher, canonical `u64` channel identity, subject and rights; a 16-bit collision or self-issued root fails. | Native publisher integration witness. |
| BNL-23 | Channel leave is durable and distinct from `unsubscribe()`, while unrelated tab/relation claims remain live. | Cross-tab channel lifecycle witness. |
| BNL-24 | Subnet join defaults to `ATTACH` only and reports joined only after challenge-bound live admission. | `subnet_auth_e2e` positive/inverse tests. |
| BNL-25 | Subnet `ATTACH` gives no channel, invocation, route, export, delegate or administration authority. | Least-authority denial matrix. |
| BNL-26 | Mesh/org/channel/subnet leave persists local disabled intent before effects, survives restart, fences stale callbacks and reports authority notification/revocation separately. | Per-domain offline/restart/incarnation tests. |
| BNL-27 | A composed invitation reports each domain stage independently; one successful stage cannot mask another failed stage. | Partial bundle crash/resume witness. |
| BNL-28 | No authority root/private issuer key or standing mesh PSK appears in invitation links, browser storage, follower IPC, diagnostics or events. | Unique-marker and serialized-envelope scans. |
| BNL-29 | `net-mesh-leaf` remains independent of the native `net` core and does not duplicate authority verification in TypeScript. | Dependency-boundary test and architecture review record. |
| BNL-30 | Raw `Uint8Array` credentials normalize to `net-bootstrap:` plus URL-safe unpadded base64 and are accepted by the real built leaf; padded, standard-alphabet, text-byte and double-encoded forms fail. | Golden encoding vectors plus built-WASM positive/negative witnesses. |
| BNL-31 | High-level `up()` derives actual origin/canonical database scope and cannot create parallel nodes by caller origin/lock/database overrides. | Type-negative tests plus two-tab attempted-scope-split witness. |

---

## 9. Verification commands

Run from `net/crates/net/browser-ts` unless noted:

```bash
npm install
npm test
npm run build
npm run size
```

Run the independent leaf workspace checks from `net/crates/net/leaf`; it is intentionally not a package in the parent `net/crates/net` workspace:

```bash
cd net/crates/net/leaf
cargo test --features mock-control-plane
cargo check --target wasm32-unknown-unknown --all-targets
cargo clippy --all-targets --features mock-control-plane -- -D warnings
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings

CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
WASM_BINDGEN_TEST_TIMEOUT=120 \
cargo test --target wasm32-unknown-unknown --features mock-control-plane \
  --test wasm_leader -- --nocapture
```

The implementation PR must add each new leader witness to the `LEADER_WITNESSES` roster and raise the current `wasm_leader` floor in `.github/workflows/ci.yml`; a passing unpinned filter is not evidence.

Run the actual browser harness from `net/crates/net/tests/rtc_browser`:

```bash
./run.sh --engine chromium --stage7
./run.sh --engine firefox --stage7
./run.sh --engine webkit --stage7  # recorded best-effort WebKit evidence, not Safari
```

Use `run.ps1` for the corresponding Windows CI leg. Pin new `RTCB PASS` names and exact Chromium/Firefox floors in CI rather than relying on nonzero totals.

Before calling the implementation ready, run the repository-prescribed gates for every touched Rust target and the real browser matrix. Do not report a fake-WASM Vitest pass as proof of Web Lock, IndexedDB, WebCrypto, or `RTCPeerConnection` behavior.

Final hygiene:

```bash
git diff --check
git status --short
git diff --stat
```

Expected:

- no whitespace errors;
- only planned files changed;
- all declared acceptance rows have a named, executed witness;
- test totals match CI output;
- exact-head CI is green before merge.

---

## 10. Implementation order and stop gates

1. Land the type/API contract and negative tests.
2. Land source normalization and redaction.
3. **Stop gate:** independent review confirms a deferred credential is acquired only after leader ownership and never crosses follower IPC.
4. Land `up()` and status projection.
5. Land `down()` and cancellation semantics.
6. **Stop gate:** real two/three-tab browser evidence proves promotion, per-handle shutdown, and no half-live lock holder.
7. Land explicit identity erasure.
8. Run secret-retention and cross-browser adversarial evidence.
9. **Stop gate:** pin the portable authority boundary. The leaf must remain outside the native core workspace; prefer opaque credential carriage and native enforcement over copied verification.
10. Land the shared invitation envelope and durable relation ledger.
11. Land mesh and organization adapters, then prove membership-only denial before continuing.
12. Land channel lifecycle, including canonical-identity collision and trusted-root inverses.
13. Land subnet lifecycle, including live admission and least-authority inverses.
14. Run multi-domain partial-result, offline leave, restart and stale-callback evidence.
15. Update public docs only after the lifecycle and relation APIs are stable.

Do not merge Tasks 3–8 or Tasks 10–14 into one broad browser rewrite. The existing WebRTC, session, error, stream, channel, organization, subnet and store surfaces are working substrate; this plan adds lifecycle ownership and adapters around them rather than replacing them. A shared invitation UX does not authorize a shared ambient membership credential.
