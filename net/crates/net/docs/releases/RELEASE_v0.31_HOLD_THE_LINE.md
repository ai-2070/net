# Net v0.31 — "Hold The Line"

*Named after Toto's 1978 debut single — David Paich's piano hook, Steve Lukather's guitar, Bobby Kimball's belt, the AOR staple whose chorus insists "hold the line, love isn't always on time."*

v0.31 is a **boundaries** release. Money, tools, and machine identity all start crossing between machines this cycle — a capability priced and paid across the mesh, a host's tools bridged onto it, a device enrolled into someone's fleet — and every one of those crossings defines a line something must *not* cross back over: a signing key, a credential, an unpaid invocation, a revoked consent, a settlement counted twice, a reorg'd block that already served. The codename is the design brief. Every default here is chosen so the line holds when the other side is hostile, lossy, reorging, or simply — *love isn't always on time* — slow.

Four tracks land:

- **Non-custodial payments across three networks** — Base, Solana, and XRPL, a money path that signs without ever holding a key, verification that survives reorgs, and the whole demand-and-supply surface now in Python and Node at parity with Rust.
- **The MCP bridge** — any MCP host reaches tools across a trusted mesh, any local stdio server publishes onto it, and credentials never leave the machine that holds them.
- **Hermes-native identity** — device enrollment, delegation chains, and agent-to-agent task handoff, with consent and pinning graduated into the SDK itself.
- **Docs rebuilt to lead with the worldview**, plus a cross-language SDK spine.

The organizing observation is the same one that has shaped every release since the substrate stopped being a prototype: **the hard parts already existed — the work was an adapter over them, not new infrastructure.** Pricing is a capability announcement. Settlement verification reads a chain the transport never touches. Owner-scope is the AEAD-verified caller origin the transport already stamps on every request. A2A is an nRPC call with a task id. Nothing below adds a routing concept or a fold — it adds edges that ride the surface the core already exposes.

---

## Non-custodial payments, now across Base, Solana, and XRPL

Net has priced and settled capability calls since v0.24. v0.31 is the release where the money path stops being one-network and one-language: three settlement networks behind a config-not-code ladder, verification that holds a payment at arm's length until the chain agrees, a signer seam that never sees a private key, and the entire caller-and-provider surface brought to Python and Node.

### The network ladder — add a network with data, not a branch

The enablement surface is **data, not code**. A signed `AssetRegistry` (`net.payment.asset_registry@1`) maps CAIP-2 chain ids and CAIP-19 asset ids to their decimals; the shipped `default_registry_v1` carries Base and Base-Sepolia USDC, Solana SPL-USDC, and native XRP, all six-decimal. A quote binds the registry's `{version, hash}` and is verified under exactly the revision that issued it — an unregistered asset is a hard reject, and a registered asset whose decimals don't match is a hard reject, so a decimals mismatch can never silently misprice a transfer. Facilitators are built from config packs (`x402_org_base_sepolia`, `cdp_base_mainnet`, `cdp_solana_mainnet`, `t54_xrpl_mainnet`) that return data and branch on nothing; `HttpFacilitator::from_config` re-verifies the facilitator's live `GET /supported` at load, so a pack is "the map, not the permission."

**XRPL ships off by default** — and not by omission. XRP is in the registry; the gate is the caller's spend policy, whose `allowed_networks` allowlist starts empty, so a wallet holding an XRPL key still gets a clean "not enabled" denial at policy time, before anything is selected, signed, or sent (`xrpl_stays_off_by_default_with_a_wallet_until_the_network_is_allowed`). Turning a network on is three deliberate acts — allowlist it, configure a facilitator, provide a namespace signer — never one.

### Verification that holds until the chain agrees — *love isn't always on time*

A facilitator receipt says a payment was *observed*. It does not say the payment is *final*, and on a chain that reorgs those are different facts. v0.31 makes the difference a type. `VerificationTier` is `Observed | Confirmed(n) | Final`, strictly ordered, and a facilitator receipt justifies only `Observed` — every higher tier comes from an **independent `ChainChecker`** that reads the chain directly rather than from the party being paid. The checkers (`Eip155Checker` on confirmation depth, `SvmChecker` on the processed→confirmed→finalized commitment ladder, `XrplChecker` on validated ledgers) cross-check the amount **delivered**, not the amount claimed sent, bound to the payer. A reorg is first-class: `InvalidationReason::Reorg` **freezes further serving** on that verification chain rather than quietly serving against a settlement that evaporated; an on-chain revert reads as `Reverted`; overpayment surfaces as an exception, never an auto-credit (there are no automatic refunds in v1). Settlement isn't always on time — so a provider that wants finality *waits* for it, tier by tier, instead of trusting the fast answer.

### Keys never cross the line — the non-custodial signer seam

The money path signs, but it never holds a key. `SchemeSigner` exposes exactly the signatures a settlement needs — EIP-712 typed data for EVM's EIP-3009 `TransferWithAuthorization`, `sign_svm_transfer` for an SPL `TransferChecked`, `sign_xrpl_payment` for a presigned XRPL Payment — and pointedly **no raw-bytes signing method at all**. That absence is the invariant: there is no arbitrary signing oracle, so a compromised caller can't coax a signature over bytes the scheme didn't build. Intents (`SvmTransferIntent`, `XrplPaymentIntent`) are derived from the *quoted* requirements, never from caller-supplied fields; the XRPL path requires an `invoiceId`, binds it, and makes partial-payment / `SendMax` unrepresentable by construction. Implementations (`ExternalSigner`, `ExternalSvmSigner`, `ExternalXrplSigner`) call out to a KMS, wallet, or MPC signer, and a wrong-namespace signer fails closed rather than mis-signing. Net learns an address and the signatures it asked for; the private key stays wherever it already lived.

### Demand and supply, now in Python and Node

The whole payment surface reaches two more languages at parity — both fronting the one Rust `PaymentEngine`, so the caller and provider state machines cannot drift between languages.

- **Callers** get a consent-gated `CapabilityGateway` whose `invoke` returns a status discriminant (`ok`, `requires_approval`, `requires_payment_approval`, `denied`, …) rather than paying silently; pricing rides `describe` / `list_tools`, and operator approval verbs (`approve_payment` / `approvePayment`, plus the spend-today accounting) gate every charge. With no flow configured, a paid capability fails **closed** — denied, never served free.
- **Providers** author `net.pricing.terms@1` with `build_pricing_terms`, publish priced tools through the same free `publish_tools` scaffolding, and read immutable `net.billing.event@1` records back through `read_billing`.
- **Outbound**, `PaymentHttpClient` walks an external HTTP 402 in both languages (the two-way door), with the caller's spend policy as the whole gate.
- All three **signer seams** (`eip155` / `solana` / `xrpl`) are wired in both languages with no raw-bytes path — Python over `spawn_blocking`, Node over a `ThreadsafeFunction` with a bounded timeout.

Go and C stay golden-vector verifiers for now — they check the wire vocabulary, they don't yet front the gateway. That asymmetry is stated, not papered over.

### A denial an agent can act on — the failure schematic

When a paid call is refused, the human gets an error string and an agent gets a **machine-actionable schematic**. `net.payment.failure@1` rides as a `net-failure-schematic` reply header *beside* — never instead of — the unchanged human error body, carrying the fields a caller branches on: the `stage` (admission vs redeem), a `recovery` block (`safe_to_retry`, `safe_to_requote`, `next_action`), whether `funds_moved`, and what a `prior_payment` did. It is deliberately scoped: `code` is `"payment"` only, free-form freeze prose stays off the schematic and on the human body, and identity / tax / KYB / shipping are explicit non-goals — the schematic tells an agent what to *do next*, not what went wrong in prose.

---

## The MCP bridge — any host, tools across a trusted mesh

v0.31 ships the **MCP bridge**. An existing MCP host (Claude Code, Cursor) can discover and use tools that live on *other machines* across a user's trusted mesh, and any local stdio MCP server can be published *onto* that mesh — in both cases without the host learning the mesh exists and without a credential ever leaving the machine that holds it. It arrives as a new `net-mesh-mcp` adapter crate plus three CLI commands: `net wrap`, `net mcp serve`, and `net mcp pin`. Discovery is the capability fold; invoke and describe are nRPC calls; owner-scoping is the AEAD-verified caller origin. The bridge rides `net-mesh-sdk` only — never the core crate — the same public surface the Python/TS bindings wrap, and a CI test fails the build if it ever reaches for core directly.

### `net wrap` — publish a local server's tools onto the mesh

```bash
net wrap github --identity ~/.net/id.toml -- npx -y @modelcontextprotocol/server-github
```

`net wrap` spawns a stdio MCP server, reads `tools/list`, and lowers each tool to an **owner-scoped mesh capability** with an nRPC handler per tool plus a `describe` service. It is long-running: it streams a `wrapped` report, then a lifecycle event per `tools/list_changed` or server exit, reconciling the announced set live.

- **Credentials never transit.** Env vars and tokens (`--env KEY=VALUE`) are set on the wrapped server's *child process* on the owning machine. Only tool *arguments* and *results* cross the wire — never the secret. A permanent CI regression test (`a_credential_env_never_appears_in_a_tool_result`) threads a sentinel token through a cross-machine invoke and asserts it appears nowhere on the wire, in logs, or in errors.
- **Owner-only by default.** A wrapped tool is describable and invocable only by the root identity that wrapped it, checked against the transport-verified caller origin — not a self-claimed field. Widen deliberately with `--allow <origin>`. Credential detection is fail-safe: an unknown status is treated as credentialed until proven boring, and the downward `--no-credentials` override needs `--force`.

Tools whose MCP name isn't a valid channel id (`createIssue`, spaced or punctuated names) are **sanitized to a stable channel-safe id and still bridged** — the original name kept for invocation — so wrapping real-world servers doesn't silently drop half their surface.

### `net mcp serve` — front the mesh to a local MCP host

```json
{ "mcpServers": { "net": { "command": "net", "args": ["mcp", "serve"] } } }
```

One line of host config and the model can reach the whole mesh. `net mcp serve` is a stdio MCP server that fronts the running `net` daemon as a **thin client** — N hosts on one machine are N shims sharing one daemon and one identity, never N embedded nodes. Its default surface is five **meta-tools** (search / describe / invoke / list-pinned / request-pin), which keeps the host's tool list small and the per-call schema accurate. Discovery and description never imply invocation: a capability that carries credentials — or reports *none*, since the demand side does not trust a wire-declared status — is search/describe-only until the operator allowlists it or approves a pin.

### Pinning as promotion — and consent

The model calls `net_request_pin(cap_id)`, which writes a **pending** request and grants nothing. A human approves out of band:

```bash
net mcp pin approve <cap_id>     # or: reject / list
```

An approved pin is **promoted to a first-class typed MCP tool** in the host's list, with its real input schema — restoring per-call schema accuracy and the host's own approval prompt — and it clears the shim's consent gate for that one capability, for that user, on that machine, and nothing wider. Two rules are absolute: **the model cannot approve its own future access** (consent happens outside the model loop), and the pin store is a per-user file, **owner-only `0600`**, written atomically under a cross-process lock so a stale snapshot can never resurrect a revoked approval. Promoted tool names are a pure function of the capability id, so approving or rejecting one pin never remaps a name a host cached onto a different capability.

### Holding the line — the security posture

The codename is the design. A bridge sits at the exact seam a confused-deputy attack wants: a host the user trusts, talking to a mesh that may carry other people's nodes.

- **Consent is fail-closed.** An empty policy gates *everything*. A wire-declared `credential_status` — including `none` — is never trusted across the demand-side boundary; the gate reloads the pin store per invoke, so an out-of-band approval takes effect immediately with no stale-snapshot window.
- **Owner scope gates both surfaces.** Describe and invoke both reject on the AEAD-verified origin, so a node outside the scope sees nothing in search *and* cannot invoke.
- **Invoke is at-most-once for credentialed *and* paid tools.** A timeout does not prove the tool didn't run, so only a provably duplicate-safe (uncredentialed, unpaid) tool retries a timed-out call, expressed as a typed `InvokeSafety` flag; a credentialed, stateful, or *paid* one surfaces the timeout rather than re-running it, so a lost reply never becomes a duplicated issue or a double charge.
- **Cross-provider collapse and failover are opt-in, off by default.** Bridging one tool from several nodes into one logical capability — and failing an invoke over between them — is powerful, but equivalence today is proven only from *wire-declared* attributes a peer controls, with no proof the peer shares your owner identity. So the safe default keeps every provider on its own node id; a single-owner mesh opts in with `net mcp serve --trust-equivalent-providers`. Collapse is additionally impossible across accounts by construction — the equivalence key folds the credential status, so a credentialed tool never merges.

Bridged tools carry `compat_tier: "mcp_bridge"`: request/response only — no streaming, artifacts, or migration. The bridge is the funnel, not the destination.

### Consent, graduated into the SDK — and bound into three languages

The consent vocabulary (`CapabilityId`, `CredentialStatus`, `ConsentPolicy`, `ConsentDecision`) and the persistent `PinStore` **graduated out of the adapter and into `net-mesh-sdk` itself**, where the adapter now re-exports them (with compile-time proofs that each re-export *is* the SDK type). From that one home they are bound into **Python** (PyO3), **Node** (napi), and **Go/C** (a new `net-mcp-ffi` crate exporting `net_mcp_*` C symbols over `classify` / `lower_tool` and the consent/pin gate), all pinned by cross-language golden vectors. This is the structural move the rest of the release stands on: one consent gate, one pin store, shared by every binding instead of reinvented per language. The full spawn/wrap round-trip stays Rust-side for now; the bindings expose the pure helpers and the gate.

### Building the boundary before it exists — credential forwarding

v0.31 also lands the deny-by-default machinery for a *future* credential-forwarding path: sealed-context crypto (`X25519SealedBoxSealer`/`Opener`, an anonymous X25519 sealed box over XChaCha20-Poly1305, key derived from the ed25519 identity seed), a two-ended policy where the caller must allow *sending* and the destination must allow *accepting* (deny wins), secret values that live only in a `SecretBackend` and materialize as a redacted, unserializable, zeroize-on-drop handle, and a `net forwarding` CLI. But **no live path forwards anything this cycle** — forwarding needs an HTTP-facing capability the stdio-only adapter doesn't have, and the never-for-stdio doctrine means a wrapped stdio server's credentials stay in its child process, permanently. The machinery is built hostile-by-default and left inert until the HTTP subsystem it's for arrives.

---

## Hermes-native — enrollment, delegation, agent-to-agent

Where the MCP bridge federates *tools*, this track federates *identity and work*. It lands this cycle in Rust and Python (with a Hermes plugin), each piece tested in-process and over a two-node loopback mesh:

- **Device enrollment** — an invite → join → approve handshake brings a new device into a root identity's fleet, with a device registry, an operator facade, silent auto-renewal, and an immediate revoke that survives a restart.
- **Delegation chains** — a capability invoke is gated along a `root → machine → gateway` chain against a revocation store, so a delegated node acts only within the scope its parent actually granted (`net wrap --owner-root`).
- **Agent-to-agent (A2A)** — `serve_a2a` / submit / status / cancel hand a task from one agent to another over the mesh, with cancellation and artifact-ref results.
- **Tool federation** — a provider publishes local tools with fail-closed approval routing (`approval_unreachable` denies rather than leaks), and a consumer surfaces them machine-namespaced and deduped.

Because consent and the pin store now live in `net-mesh-sdk` (above), this track builds on the same SDK-resident gate rather than a Hermes-local copy — the "consent behind the SDK" refactor, delivered here rather than promised. Node/TS parity for enrollment, delegation, and A2A is the one piece that stays on the deferred list.

---

## The docs, rebuilt to lead with the worldview

v0.31 lands a documentation rebuild. The public docs now lead with what Net *is for* before how it works: **Net is a discovery mesh for agentic capability** — agents find live capabilities across a trusted mesh, invoke them safely, observe what happened, and recover when work fails — with the latency-first encrypted mesh kept as the identity one layer down, not discarded. New `worldview/` pages sell the belief system (including the flagship "submitted is not completed" explainer — a `200 OK` is not work done — and an explicit "do NOT use Net when…" page); real-command `guides/` walk both bridge directions end to end; a nine-page **payments concept spine** (`what-net-payments-is`, `x402-and-net`, `the-lifecycle`, `verification-tiers`, `spend-policy-and-approvals`, `non-custodial-signing`, `networks`, `failure-schematic`, `billing`) draws the money model 1:1 from the shipped primitives; and a cross-language **SDK spine** (`quickstart`, `announce`, `discover`, `invoke`, `watch`, `artifacts`, `errors`) runs seven pages across Rust / TypeScript / Python / Go, with C an honest three-file spine because its ABI is bus-only and the Go binding's poll-based-not-async asymmetry stated on the page rather than faked.

The load-bearing part is a **pre-flight claims audit**: no worldview or payments claim shipped until the code cashed it. It corrected copy to the real command names, confirmed multi-hop discovery is genuinely shipped (bounded, not infinite), pinned the `mcp_bridge` request/response tier on every page that mentions bridged tools — and pulled one overclaim outright: per-delegation-chain **budget inheritance** ("a child's budget ≤ the parent's remaining") has no code behind it, so it was downgraded from documented behavior to roadmap. No existing `concepts/` or `reference/` page was moved or dropped — additive only.

---

## The hardening pass — what the reviews forced

Each landed track got a dedicated code review that ran independent finder angles and verified every candidate by reading the implicated path. The secure cores held — credentials stay in the child, keys never reach the signer seam, consent is genuinely fail-closed, the tiered-verification arithmetic checked out — and the findings clustered exactly where these boundaries meet an unfriendly network. Every fix landed with a regression test; the crate suite, clippy, and `cargo doc -D warnings` are green.

**The bridge.** The headline was the retry path: a same-node retry that covered a lost reply also re-ran credentialed, non-idempotent tools, so a timeout on `create_issue` could duplicate the issue — now an invoke is at-most-once unless the tool is provably duplicate-safe (`InvokeSafety`), extended so a *paid* tool with `credential_status: none` is at-most-once too. The wrapped server's reply reader was moved off the single stdout drainer so a full pipe can't wedge both directions and capped so a flood can't exhaust memory; `wrap::session::refresh()` now captures and reverts prior state on a `tools/list_changed` failure instead of silently misrouting a cross-publication; the pin store is owner-only from the first byte and its save is durable; and on the new Go/C surface, an empty-error consent decide that failed *open* was made fail-closed, plus cgo thread-migration, use-after-free, and a data race were closed with `-race` tests.

**The money path.** The SVM checker's payer-bind failed *open* when the facilitator named no payer — now a terminal fail-closed error; the XRPL checker was pinned to rippled `api_version: 1` and taught to resolve Clio/v2 nested `tx_json`, so a v2-shaped response no longer invalidates every settlement; an untagged quote now rejects a *tagged* payment and the engine hard-refuses a non-u32 destination tag; EVM delivery is bound to the EIP-3009 nonce (accepting a bare-hex nonce that previously disabled the bind), and a caller-injected `authorization.nonce` can no longer override the provider `invoiceId` off-EVM; and engine internals are redacted out of the caller-facing gate message.

**The crypto.** The sealed-box sealer now rejects a low-order or identity recipient key — otherwise a contributory-check gap disclosed full plaintext — and credential classification was widened past the three-name `authorization` / `cookie` / `set-cookie` list so a less-obvious secret header isn't announced in the clear.

---

## What's deferred (honestly)

- **XRPL IOUs (RLUSD and any issued asset).** Only native XRP (Mode A) ships; an IOU asset or an `extra.issuer` is a structured refusal, pending the issued-currency amount-domain review.
- **Reorg-*out* detection.** A reorg observed while verifying freezes the chain today, but a settlement that was `Confirmed` and *later* reorged needs a stateful checker; out of scope, it degrades to `Pending` and the engine holds its last tier.
- **Automatic refunds / overpayment auto-satisfy.** None in v1 — overpayment is surfaced as an exception, not credited.
- **Node/TS parity for enrollment, delegation, and A2A.** The mechanism ships in Rust and Python; only the napi/TypeScript surface is missing, and it's the one Hermes piece left on the list.
- **Owner-identity verification for MCP failover.** `--trust-equivalent-providers` is the interim guard; proving a substitutable peer shares your root identity (not merely a public contract) waits on the permission system, and collapse/failover is inert until you turn it on.
- **A live credential-forwarding path.** The deny-by-default machinery landed but nothing forwards a secret this cycle; stdio wrapping keeps credentials in the child permanently, and distribution/injection wait on the HTTP subsystem.
- **Remote / HTTP MCP, OAuth passthrough, `--public` exposure.** Stdio-only by design for v0.31 — it dodges the bulk of the spec churn — and public mesh exposure stays behind the owner-only default.
- **Streaming / artifacts / migration over a bridged tool.** Structurally out of the `mcp_bridge` compat tier; these are native Net capabilities, and the docs say so on every page.
- **Hermes real-machine acceptance.** The enrollment / federation / A2A code passes in-process and over a two-node loopback, but validation against a real running Hermes and the two-machine stopwatch acceptance remain infra-gated.
- **The non-`exact` scheme families** (`upto`, RFQ/dynamic pricing) and the **MCP FFI spawn/wrap round-trip** — shelved behind pinned entry criteria; an unknown scheme fails closed at selection.

---

## Breaking changes

v0.31 is **additive on the wire and on every existing transport, fold, reliability, and SDK path** — none of them changed shape. What a downstream feels is new surface and a dependency-major bump, not a behavior change to code it already ships.

- **New crates:** `net-mesh-mcp` (the SDK-only bridge adapter) and `net-mcp-ffi` (the Go/C ABI). Both honor the SDK-only doctrine; a CI test fails the build if the adapter reaches for the core crate directly.
- **New CLI:** `net wrap`, `net mcp serve`, `net mcp pin`, and `net forwarding` (inert this cycle). A node that never runs them is untouched.
- **New public SDK surface:** the `net-mesh-sdk` consent module (`CapabilityId`, `CredentialStatus`, `ConsentPolicy`, `ConsentDecision`) and `PinStore`; the enrollment / delegation / A2A modules; and a node's ability to read its own origin hash (the value the owner-scope gate keys on).
- **New payments surface:** the signed `AssetRegistry` / `RegistryRef`, the `VerificationTier` / `ChainChecker` / `ChainVerdict` verification types, the `SchemeSigner` seam with `ExternalSvmSigner` / `ExternalXrplSigner` and the `SvmTransferIntent` / `XrplPaymentIntent` builders, facilitator config packs — plus the full `CapabilityGateway` / `PaymentProvider` / `PaymentHttpClient` and signer seams in Python and Node.
- **New wire vocabulary, all additive and capability/header-gated:** `net.payment.asset_registry@1`, `net.payment.verification@1`, `net.payment.failure@1`, and `net.invoke.forwarded_context@1`. Old peers drop what they don't understand.
- **Dependency-major bumps.** Unlike v0.30's lockfile-only cycle, the money path and the sealing path moved onto current crypto — `k256` 0.14, `ed25519-dalek` 3, `x25519-dalek` 3, `sha2` 0.11, `sha3` 0.12, and `net-payments`' HTTP client to `reqwest` 0.13 — which a downstream sharing the resolved graph or re-exporting these types will need to align on.

---

## How to upgrade

1. **Pull the release** — nothing changes unless you invoke the new commands or opt into a payment flow. Existing bus, stream, nRPC, and persistence code behaves exactly as before.
2. **To take payments across a network**, add it to the caller's spend-policy `allowed_networks`, configure a facilitator from a config pack, and wire a namespace signer (`eip155` / `solana` / `xrpl`) — the key stays in your KMS/wallet. XRPL is off until you do all three. Providers author terms with `build_pricing_terms` and publish with `publish_paid_tools`; callers gate every charge through the `CapabilityGateway`, in Rust, Python, or Node.
3. **To publish a tool onto the mesh**, run `net wrap <name> --identity <id> -- <stdio MCP server cmd>`. Put the tool's secrets in `--env`; they stay in the child. Use a *stable* identity, or owner-only scoping admits nobody.
4. **To consume the mesh from an MCP host**, run `net mcp serve` and add the one-line host config above. Discover with `net_search_capabilities`, approve a capability out of band with `net mcp pin approve <cap_id>`, and it promotes to a first-class typed tool. Enable cross-provider failover with `--trust-equivalent-providers` only on a mesh where every peer is your own.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.30.0 → 0.31.0`, propagated across the CLI, deck, SDK, and language-binding manifests. This cycle moved first-party dependencies (v0.30 was lockfile-only):

- **Money path:** `net-payments`' HTTP client to `reqwest` 0.13, with the money-path TLS pinned to a `ring`-backed rustls provider (no process-global) over bundled webpki roots.
- **Crypto stack:** `k256` 0.14, `ed25519-dalek` 3, `x25519-dalek` 3, `sha2` 0.11, `sha3` 0.12 — the same primitives the settlement signing and the sealed-context path depend on.
- **New adapters, no new third-party weight:** `net-mesh-mcp` and `net-mcp-ffi` pull only crates already vendored in the workspace (`async-trait`, `bytes`, `fs2`, `futures`, `serde`, `serde_json`, `thiserror`, `tokio`); the SDK-only-dependency CI test guards it.
- **Docs/web (Next.js under `web/`), lockfile and tooling only, no runtime path:** `marked` 18.0.6, `prettier` 3.9.5, `shiki` 4.3.1, plus the routine `posthog-js` / `posthog-node`, `immer`, and Radix refreshes.

---

Released 2026-07-09.

## License

See [LICENSE](../../LICENSE).
