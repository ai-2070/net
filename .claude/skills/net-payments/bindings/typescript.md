# Node / TypeScript — payments

Read `../bindings.md` first. This page is only what is TypeScript-specific.

## Package and import — `@net-mesh/core`, not the wrapper

```ts
import { CapabilityGateway, PaymentProvider, PaymentHttpClient, buildPricingTerms } from '@net-mesh/core'
```

**`@net-mesh/sdk` contains no payments code.** `net/crates/net/sdk-ts/src/` has
none of it. There is also no `@net-mesh/payments` package — the name is
reserved and unpublished, and everything ships inside `@net-mesh/core`.

Behind `payments`, `publish` and `payments-http` — all three in the
default build. `payments-http` carries `HttpFacilitator`, so without it the
only reachable settlement backend is the mock.

## Demand — pay to invoke

Source: `net/crates/net/bindings/node/src/capability_gateway.rs`.

```ts
const gw = new CapabilityGateway(
  mesh, pinStorePath ?? null,
  paymentPolicyPath ?? null, paymentProfile ?? null,   // "production" | "dev_test"
  paymentUnsafeMockAutoAllow ?? false,
  paymentSignerAddress, paymentSigner,                 // eip155: async (typedDataJson) => "0x..." (65-byte sig)
  paymentSignerSvmAddress, paymentSignerSvm,           // solana: async (intentJson) => base64 tx
  paymentSignerXrplAddress, paymentSignerXrpl,         // xrpl:   async (intentJson) => hex Payment blob
)
await gw.search(query)                    // JSON string; capabilities[] each with requires_approval
await gw.describe(capId)                  // JSON string; includes pricing_terms (null = free)
await gw.invoke(capId, argumentsJson)     // JSON string; status discriminant (+ failure on denials)
await gw.approvePayment(quoteId)          // operator verbs — resolve a requires_payment_approval
await gw.rejectPayment(quoteId); await gw.pendingPayments(); await gw.spentToday(network, asset)
gw.close()                                // RELEASE the node clone before mesh.shutdown()
```

**Every gate method resolves to a JSON string — never a throw for a gate
outcome.** The `invoke` status vocabulary is `ok | requires_approval |
requires_payment_approval | validation_error | denied | not_found |
transport_error | no_daemon | error`.

A denial carries a `failure` object (the `net.payment.failure@1` schematic)
**only when the provider attached one** — branch on `failure.reason` /
`failure.recovery` when present, else the prose (`failure-schematic.md`).

## Supply — price and charge

Source: `net/crates/net/bindings/node/src/payment_provider.rs`.

```ts
// A settlement backend is MANDATORY — there is no default. Either settle for
// real (needs the `payments-http` build feature):
const provider = new PaymentProvider(
  mesh, statePath, billingLogPath /* ? */,
  'https://facilitator.example.com',       // facilitatorUrl
  undefined,                               // facilitatorAuthToken, where wanted
)
// ...or opt explicitly into the in-process mock, which MOVES NO VALUE:
const dev = new PaymentProvider(mesh, statePath, undefined, undefined, undefined, true)
// The caller's possession proof is required by DEFAULT — `provider` above
// already requires it. Pass false only for a deployment whose callers predate
// the binding: without it the quote id alone redeems, and it is not a secret.
const legacy = new PaymentProvider(
  mesh, statePath, undefined,
  'https://facilitator.example.com', undefined, undefined,
  false,  // requireInvocationBinding — the opt-out, not the opt-in
)
// Terms must be authored under the SAME registry revision the provider quotes
// under — a real facilitator means the production registry:
const terms = buildPricingTerms(
  provider.providerEntityId, capability, requirementsJson,
  true,  // productionRegistry
)
// Passing neither throws; passing both throws. The mock lets a provider sign
// quotes, emit billing events, and serve while settling nothing, so choosing it
// is a decision the operator makes out loud.
provider.providerEntityId                 // Buffer (32B); the identity that issues quotes
await provider.readBilling()              // string[] of net.billing.event@1

const pub = await provider.publishPaidTools(
  tools,                                  // [{ name, description?, inputSchema }]
  handler,                                // async ({ toolName, argumentsJson }) => ({ text, isError? })
  pricing,                                // { [toolName]: termsJson } — every tool must be priced
  { version?, ownerOrigin?, allowAnyCaller?, handlerTimeoutMs? },
)
provider.close()                          // tears down the quote/pay wire + releases the node
```

Free publish, behind `publish`:

```ts
const handle = await mesh.publishTools(tools, handler, { allowAnyCaller: true })
handle.serving; handle.tools; await handle.withdraw(); handle.stop()
```

## Outbound HTTP-402 — opt-in `payments-http`

```ts
const client = new PaymentHttpClient(
  paymentPolicyPath, paymentProfile, false, signerAddress, signer,
  'public_only',  // destinationPolicy — the default; see below
)
const [statusJson, body] = await client.fetchPaid(url)   // [string, Buffer]
```

**`destinationPolicy` defaults to `public_only`, and that will refuse a
`localhost` URL.** This is the one door whose URL an agent can choose, so the
SSRF guard is on by default — a permissive default would put every integration
one model-chosen URL away from an unauthenticated service on the same host.
Reaching a local or self-hosted x402 server is done by asking for it:
`'public_or_loopback'` for this machine, `'allow_private'` for an internal
network. An unknown value throws rather than falling back, and there is no
spelling that disables the guard entirely.

## Three Node-specific obligations

**Callbacks are JS async functions returning Promises**, not sync callables. This
is the first thing that differs from Python, where the same seam takes a plain
callable.

**Signers are async** — `(typedIntentJson) => Promise<string>` — bridged to
`ExternalSigner` / `ExternalSvmSigner` / `ExternalXrplSigner` through a
`ThreadsafeFunction`→Promise seam. Both-or-neither per scheme; requires
`paymentPolicyPath`. **The timeout is one-sided:** a signer timeout drops the
Rust wait but does **not** cancel the JS callback, so treat that outcome as
indeterminate rather than failed (`signer.md`).

**`close()` is mandatory before `NetMesh.shutdown()`.** A `#[napi]` class is
GC-finalized, not scope-dropped, so `CapabilityGateway` and `PaymentProvider`
retain a node clone that makes `shutdown()`'s `Arc::try_unwrap` fail with
"outstanding references" until GC happens to run. `close()` drops it
deterministically. Publish handles use `withdraw()` (awaited) or `stop()`.

## Other Node facts worth stating if hit

- **`permissiveChannels: true`** on `NetMesh.create({...})` — the Node twin of
  Python's `permissive_channels` — is required for `publishTools` /
  `publishPaidTools`, because served tools ride dynamically-named channels the
  default channel-config registry rejects. Defaults to `false`.
- **`NetMesh.localAddr()`** returns the OS-assigned `ip:port`, which is what you
  need for a two-node handshake.
- **`listTools()` carries `pricingTerms`**, which was once `watchTools`-only.

## Gaps

`bindings/coverage.md` is authoritative. The one to know: **tiered verification
is Rust-only.** No `ChainChecker` projection — a settled result here is the
facilitator's claim, not an independent confirmation.

## Where to look when this page is not enough

- **Authoritative source:**
  `net/crates/net/bindings/node/src/capability_gateway.rs`,
  `net/crates/net/bindings/node/src/payment_provider.rs`,
  `net/crates/net/bindings/node/src/payment_http.rs`.
- **Conformance:**
  `net/crates/net/bindings/node/test/payments_golden_vectors.test.ts`.

## Never infer from another binding

- Signers and handlers are **async** here and **sync callables** in Python.
- The `close()` obligation is a napi GC artifact and has no Python twin.
- Rust returns typed `GatedOutcome` values; here every gate method resolves to a
  JSON string you must parse.
