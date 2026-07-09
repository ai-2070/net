# Documentation Strategy — Net Payments (x402-native commercial facts)

**Status:** APPROVED WITH AMENDMENTS (Kyra, 2026-07-09) — not started. The six
required amendments are folded in: the bounded "commercial facts" definition +
PII boundary (Doctrines 9), terms-acceptance = signed-evidence-only (Doctrine
10), the "no HTTP endpoint required" differentiator (Doctrine 11), the
payments-scoped per-language wording incl. Node-has-no-delegation/A2A (Doctrine
8), and XRPL demoted from the shipped ladder to enablement-gated (Phase 0 +
`payments/networks.md`). Sub-plan of
[`DOCS_STRATEGY_PLAN.md`](DOCS_STRATEGY_PLAN.md), which deliberately **deferred**
payments docs on 2026-07-04 ("`payments-later.md` … Defer as stubs — Speculative;
must not imply shipped features"). That deferral is now unblocked: payments has
shipped — the `net-payments` crate (P0 mock + P1 real-network ladder), the
five-envelope object model, tiered verification, spend policy + approvals, the
`net.payment.failure@1` schematic, and demand+supply SDK surfaces in Rust,
Python, and Node. This plan governs the public docs under
`web/src/content/docs/` for the money layer, and reconciles with the one payments
asset already live in worldview (`guides/submitted-is-not-completed.md`, the
payment ladder).

**Goal:** Make Net Payments *legible as a category* (what it is, and — more
importantly — what it is **not**) and *usable as an implementation surface*
(price a capability, pay to invoke, approve/budget, enable a network), without
ever reading like a payment processor. The single source-of-truth asset is the
`net-payments` skill (`.claude/skills/net-payments/`, 16 reference files); this
plan lands its content as public docs, pinned to shipped behavior.

**Scope of this plan (decided):** ONE payments docs plan. Phase 1 (the concept
spine) is specified to executable depth; Phases 2–5 are sketched here and can
spin off sub-plans if a phase grows. The `net-payments` skill stays the living
agent-facing reference; these docs are the human/agent public surface drawn from
it. **The code is ground truth; the skill is the draft; neither ships a claim the
crate doesn't cash (Phase 0).**

---

## Positioning decision (frozen)

**Net standardizes the commercial facts around capability invocation; it does
not intermediate the money.** This is the category line from the skill's
`concepts.md`, and it is the whole positioning — the docs succeed or fail on
whether a reader stops thinking "payment processor."

- **Lead line:** *x402 moves the money; Net signs the commercial facts around it
  — provider identity, discovery-time pricing, tiered verification, immutable
  billing, and spend policy.*
- **"Commercial facts" is a bounded term (frozen).** Commercial facts are
  **references, commitments, signatures, quotes, verification results, policy
  decisions, and billing events.** They are **not** customer PII, tax records,
  KYB files, invoices, shipping data, or provider account records. Every page
  that uses the phrase carries this boundary — otherwise a reader takes
  "commercial facts" as permission to route billing-profile data through Net.
- **The negative space is the positioning.** Net Payments does **not** custody
  funds, process payments, issue invoices, determine taxes, or clear
  transactions. Every concept page states which of those it is *not*, up front.
  A reader who leaves thinking "Net is a Stripe/PSP" is a documentation failure,
  not a simplification.
- **Why layered onto the mesh story:** payments are a capability of the agentic
  mesh (`DOCS_STRATEGY_PLAN.md`'s frozen "wedge now, substrate underneath"), not
  a separate product. The payment ladder is already the worked example in
  `worldview/… submitted-is-not-completed.md` ("`200 OK` is not work done; facts,
  not acknowledgements") — payments docs deepen that, never contradict it.

**Reconciliation obligation:** `guides/submitted-is-not-completed.md` and
`worldview/right-and-wrong-use-cases.md` already reference payments. The new
`payments/` section must tell the same story at greater depth; the worldview
pages stay the funnel, `payments/` is the destination. Do not let a new payments
page re-open a positioning the worldview already froze.

---

## Doctrine (non-negotiable constraints)

Lifted from the skill's eight doctrines + `gotchas.md`, plus the data-boundary
and scope constraints below; these gate every page.

1. **x402 is the wire; Net signs around it.** Net envelopes wrap x402
   structures; they never replace, translate, or re-encode them. Chain specifics
   live in x402 schemes + facilitator config, never in Net core. No page shows a
   "Net payment format" that parallels x402.
2. **Byte-preservation is law.** x402 documents ride as base64 of their *original
   bytes* (`X402Carry`), never re-serialized. Every code sample that carries an
   x402 doc carries it opaquely; a sample that re-parses one is wrong.
3. **Non-custodial by construction.** Identity keys ≠ settlement keys. The
   `SchemeSigner` seam takes *typed operations* and returns signatures; there is
   no raw-bytes signing method. No page implies Net holds or moves a key.
4. **Verification is a tier, not a boolean:** `observed | confirmed(n) | final`.
   A facilitator receipt is `observed`, full stop — `confirmed(n)`/`final` come
   only from the independent on-chain `ChainChecker`. Reorg is a first-class
   outcome that freezes the quote. No page says "the facilitator confirmed
   finality."
5. **The policy engine decides, not the model.** Spend policy runs caller-side
   before anything leaves; provider policy runs at quote issuance *and* before
   the handler. Handlers never see unpaid calls. Approvals render in UX; the
   decision lives in shared policy state. Real networks deny by default.
6. **Enabling a network is config, not code.** Facilitator pack + registry
   entries + a conformance run — no new envelope types, no per-network branches
   outside `src/x402/`. The network guide is a config recipe, not a code port.
7. **Honesty pinned to shipped behavior (Phase 0 gate).** No page ships a claim
   the crate doesn't cash. The reserved/deferred surfaces below are named as
   reserved, or omitted — never implied shipped.
8. **Per-language availability is stated, not hidden — and scoped to payments.**
   Rust, Python, and Node expose the **paid-capability demand/supply surfaces
   described here**; Go is a golden-vector verifier only; there is no C payment
   flow; and the Rust `net-payments` crate is **not on crates.io**. State it the
   way `DOCS_STRATEGY_PLAN.md` states the Go/C mesh asymmetry — plainly. Do not
   let a reader infer *every* agent feature exists everywhere: **Python
   additionally exposes delegation/A2A defaults; Node delegation/A2A are not
   exposed yet and are intentionally out of scope for this payments release**
   (see [`NODE_DELEGATION_A2A_SDK_PLAN.md`](NODE_DELEGATION_A2A_SDK_PLAN.md)).
9. **No plaintext PII in Net envelopes.** Net Payments does not carry plaintext
   customer PII in invocation, billing, lifecycle, or failure envelopes.
   Provider / customer / KYB / billing records live in provider or partner
   systems. Net may carry **opaque profile references, commitments, and signed
   acceptance evidence** — never the underlying records. Every envelope example
   respects this; no page shows a "billing profile" field with plaintext
   customer data.
10. **Terms acceptance is signed evidence, not a terms service.** Where terms
    support is documented, it means **signed terms-acceptance evidence and terms
    hashes/IDs** (the `terms_hash` already on the quote envelope). It does **not**
    mean Net hosts terms text, validates legal authority, stores customer
    identity, or adjudicates enforceability. Anything beyond signed evidence +
    hashes/IDs is reserved.
11. **Net-native payments do not require an HTTP endpoint.** Net-native paid
    capabilities are discovered through capability announcements and invoked over
    nRPC; x402 payment material rides as opaque preserved bytes in the
    invocation/admission envelope. **HTTP 402 is an adapter path for web APIs
    (`http402.md`), not a requirement for Net providers.** No page implies a Net
    provider must run a web server.

---

## Reader journeys (payments serves four+ readers)

| Reader | Needs | Served by |
|---|---|---|
| **Provider** (charges) | Price a capability at discovery, run the quote→verify→settle→serve→bill lifecycle, gate the handler | `payments/` (lifecycle, verification) + `guides/price-a-capability.md` + `sdk/<lang>/payments` |
| **Caller** (pays) | Discover a price, apply spend policy, clear or request approval, pay, branch on a denial | `payments/` (spend policy, failure schematic) + `guides/pay-to-invoke.md` |
| **Operator** (approves/budgets) | Approve held quotes, set budgets, read the billing stream | `guides/approve-and-budget.md` + `payments/spend-policy…` + `payments/billing` |
| **Network integrator** | Turn on Base/Solana/xrpl via config | `guides/enable-a-network.md` + `payments/networks` |
| **Agent / Claude Code** (Reader 3) | Branch on `net.payment.failure@1`; buildable tasks | `payments/failure-schematic` + `agent-briefs/` + the `net-payments` skill |

Reader 3 is again category-defining: the machine-actionable **failure schematic**
(reason → recovery, safe-to-retry / safe-to-requote) is the payments feature most
directly built *for* an agent, and gets its own concept + reference page.

---

## Information architecture (additive)

Current sections (keep all): `worldview, start, guides, concepts, sdk,
agent-briefs, reference, tutorials, releases`. **New** section: `payments` (the
money-layer concept spine). Payments content also threads into the existing
`guides/`, `sdk/<lang>/`, `reference/`, and `agent-briefs/`.

Proposed sidebar order in `web/src/docs.order.ts` → `sections` (payments after
`concepts` — the mesh mental model comes first — and before `sdk`):

```
worldview, start, guides, concepts, payments, sdk, agent-briefs, reference, tutorials, releases
```

### New/changed pages (source skill asset each draws on)

**`payments/`** (Phase 1 — the concept spine)
- `README.md` — landing: the category line + the bounded "commercial facts" definition + the "no HTTP endpoint required" differentiator + the object model at a glance + links (`SKILL.md`, `concepts.md`).
- `what-net-payments-is.md` — the mental model + the eight doctrines + the explicit "what it is NOT" + the PII boundary (`concepts.md`, `gotchas.md`).
- `x402-and-net.md` — envelopes wrap x402; byte-preservation; the two-way door. **Leads with the differentiator (Doctrine 11):** Net-native paid capabilities are announced + invoked over nRPC with x402 material carried as opaque preserved bytes in the invocation/admission envelope — HTTP 402 is an adapter path for web APIs, not a requirement for Net providers (`x402.md`, `object-model.md`).
- `the-lifecycle.md` — quote → verify → settle → serve → bill (provider) and pricing → spend policy → pay → invoke (caller) (`provider.md`, `caller.md`).
- `verification-tiers.md` — `observed | confirmed(n) | final`; the independent `ChainChecker`; reorg freeze; the facilitator is not in the trust root (`verification.md`).
- `spend-policy-and-approvals.md` — the policy engine decides; budgets, delegation inheritance, the operator approval surface; fail-closed default (`spend-policy.md`).
- `non-custodial-signing.md` — identity keys ≠ settlement keys; `SchemeSigner`; eip155 / svm / xrpl; no raw-bytes path (`signer.md`).
- `networks.md` — config-not-code; CAIP-2/CAIP-19; the signed asset registry; the network-enablement ladder **stated by go/no-go state, not as one flat "shipped" list**: mock (P0) is fully active; Base Sepolia / Base / Solana are active *as applicable per their pinned enablement state* (seams landed, live conformance/checker gated per rung); **XRPL is built Mode-A (XRP-only) but enablement-gated — do NOT list it as shipped-active** pending the pinned upstream `scheme_exact_xrpl` + live t54 conformance (`networks.md`, [`PAYMENTS_P1_NETWORK_LADDER.md`](PAYMENTS_P1_NETWORK_LADDER.md), [`PAYMENTS_XRPL_ENABLEMENT_PLAN.md`](PAYMENTS_XRPL_ENABLEMENT_PLAN.md)). Phase 0 records the exact per-rung go/no-go.
- `failure-schematic.md` — `net.payment.failure@1` beside the human error; reason→recovery mapping; the tolerant predicate (`failure-schematic.md`).
- `billing.md` — immutable billing events + the stream; what billing is NOT (`billing.md`).

**`guides/`** (Phase 2 — task recipes)
- `price-a-capability.md` — `build_pricing_terms` + publish paid tools + the engine (`provider.md`, `bindings.md`).
- `pay-to-invoke.md` — the gateway + spend policy + approval loop + branch on `failure` (`caller.md`, `failure-schematic.md`).
- `approve-and-budget.md` — operator approval verbs, budgets, `spent_today`, the billing stream (`spend-policy.md`, `billing.md`).
- `enable-a-network.md` — facilitator pack + registry entry + conformance run; testnet (`networks.md`, `facilitator.md`).
- `pay-an-http-402-api.md` — the outbound `X402HttpFlow` / `PaymentHttpClient` (`http402.md`).

**`sdk/<lang>/payments.md`** (Phase 3) — for `rust`, `typescript`, `python`
(full demand+supply) and `go` (verifier-only note). Rides the **existing
per-language gating** (`languages` in `docs.order.ts`; taxonomy in
`web/src/lib/docs-language.ts`). Each states the availability matrix (`bindings.md`).

**`reference/`** (Phase 4) — `payments-envelopes.md` (the five envelopes + the
canonical signing regime + idempotency/versioning; `object-model.md`),
`payment-failure-schematic.md` (the full `@1` contract + tolerance predicate;
`failure-schematic.md`), `x402-carry.md` (`X402Carry`, requirements/payload/
settlement views, CAIP; `x402.md`), `payments-status-vocabulary.md` (gateway/HTTP
status discriminants + `ERR_PAYMENT`), and **`terms-acceptance.md`** — the
signed-acceptance-evidence boundary (Doctrine 10): `terms_hash` / terms IDs on
the quote envelope + signed acceptance evidence, with an explicit "not: hosting
terms text, validating authority, storing identity, adjudicating enforceability"
note (may instead be a section of `payments-envelopes.md` if it stays short).
**No reference page shows a plaintext "billing profile" field.**

**`agent-briefs/`** (Phase 4) — `price-and-charge`, `pay-to-invoke`,
`enable-a-network`: executable-by-agent (Goal / Files / Commands / Expected /
Test / Acceptance / Pitfalls), cross-linking the published `net-payments` skill
and `testing.md`.

### Nav / homepage wiring (mechanics)

Same as `DOCS_STRATEGY_PLAN.md`: the site is **Next.js**; docs are a file tree
under `web/src/content/docs/` discovered by `web/src/lib/docs.ts`; ordering,
labels, hide, and language gating live in `web/src/docs.order.ts`. Adding the
`payments` section = create the folder + `README.md`, then add it to `sections`,
`folders`, and `labels`. The `sdk/<lang>/payments` pages append to each existing
`folders."sdk/<lang>"` list and ride the existing `languages` map. No page moves;
no `concepts/`/`reference/` deletions.

---

## Phase plan

### Phase 0 — Payments claims audit (blocking gate)

Output: `docs/misc/PAYMENTS_DOCS_CLAIMS_AUDIT.md`. Payments has the sharpest
shipped-vs-reserved boundary of any Net surface, so this gate is load-bearing.
Verify **against code, not the skill**:

- **Shipped (may be documented):** the quote → verify → settle → serve → bill
  lifecycle (`payments/src/engine/`, `flow/`); tiered verification
  `observed|confirmed(n)|final` + `ChainChecker` + reorg freeze (`checker/`,
  `verification.md` claims); spend policy + approval verbs + delegation
  inheritance (`policy/spend.rs`); `net.payment.failure@1` (`sdk/src/tool_payment.rs`);
  signers eip155/svm/xrpl via `ExternalSigner` (no raw keys); billing log + stream
  (`billing/`); outbound HTTP-402 (`flow/http402.rs`); SDK surfaces — Rust/Python/
  Node demand+supply, Go verifier-only.
- **Network ladder — record go/no-go per rung, never a flat "shipped" list.**
  Mock (P0) is fully active. For each real rung (Base Sepolia / Base / Solana /
  XRPL) record the *pinned enablement state* from
  [`PAYMENTS_P1_NETWORK_LADDER.md`](PAYMENTS_P1_NETWORK_LADDER.md) +
  [`PAYMENTS_XRPL_ENABLEMENT_PLAN.md`](PAYMENTS_XRPL_ENABLEMENT_PLAN.md): which
  have live conformance vs. env-gated pending, which lack a chain checker
  (serve-tier ceiling), and — specifically — **XRPL is built Mode-A (XRP-only)
  but enablement-gated (live t54 conformance open; upstream `scheme_exact_xrpl`
  not yet pinned)**, so it is **not** documented as shipped-active. `payments-live.yml`
  is the env-gated live path, not proof of default enablement.
- **Reserved / deferred (MUST NOT be implied shipped):** disputes/refunds
  (`net.payment.dispute@1` is *reserved*; no semantics pre-P5); RFQ / dynamic
  pricing (deferred — no counter-offer object; that absence is the rule);
  accounts / postpaid / prepaid (Mode E — deferred); inbound HTTP-402 *serving*
  (deferred); **terms handling beyond signed acceptance evidence + hashes/IDs**
  (Doctrine 10 — reserved). Each is named "reserved/deferred" or omitted.
- **Data boundary (audit the envelope shapes, not just prose):** confirm no
  invocation / billing / lifecycle / failure envelope carries plaintext customer
  PII — only opaque references, commitments, signed acceptance evidence, and the
  `commercial facts` set (Doctrine 9). Any field that could carry a "billing
  profile" / customer record is a finding: downgrade the copy or file it.
- **Availability caveats (must be stated on the SDK pages):** `net-payments` is
  **not published to crates.io** (a Rust consumer needs a git/path dep — the
  `crates-v*` release ships only `net-mesh`, `net-mesh-sdk`, `net-mesh-mcp`, and
  the macros); the **seam** (`ToolPaymentGate`, `FailureSchematic`) *does* ship in
  `net-mesh-sdk` (ungated), but the engine does not; `payments-http` is **opt-in**
  (kept out of the default wheel/.node — it pulls reqwest/rustls); **delegation /
  A2A are Python-only today** (Node out of scope for this release).

**Acceptance:** every payments claim is backed by a named primitive/test or
downgraded/removed; no later page asserts an unbacked or reserved capability; the
per-language availability matrix, the per-rung network go/no-go, and the PII/terms
boundaries are written and correct.

**Exit gate:** if any headline claim ("Net settles your payment", "the
facilitator confirmed finality", "pay from any language", "XRPL is live", "carry
your billing profile through Net") is materially wrong, fix the copy (or
downgrade the claim) before Phase 1 ships that page.

### Phase 1 — Concept spine (`payments/`) (~2–3 days)

The ten `payments/` pages + the `docs.order.ts` wiring (new `payments` section
after `concepts`, `folders.payments` order, `labels`) + the worldview
reconciliation pointer.

**Acceptance (all must hold):**
- Every page opens with the reader's commercial problem, and every concept page
  states the relevant "it is NOT" (custody / processing / invoicing / clearing).
- `README.md` / `what-net-payments-is.md` carry the bounded "commercial facts"
  definition and the PII boundary (Doctrines 9); `x402-and-net.md` leads with the
  "no HTTP endpoint required" differentiator (Doctrine 11).
- `verification-tiers.md` never conflates a facilitator receipt with finality;
  `non-custodial-signing.md` shows the typed-intent seam, no raw key.
- `failure-schematic.md` documents the tolerant predicate exactly as the four
  golden-vector verifiers enforce it (known-field reject, unknown-key collapse,
  optional type-checks — mirrors the fixture `_note`).
- Reconciles with `submitted-is-not-completed.md` (no contradiction); the
  worldview stays the funnel.
- `cd web && npm run build` passes; internal links resolve; `payments` renders
  first-after-concepts in the sidebar with correct labels.

### Phase 2 — Provider + caller + operator guides (~3 days)

The five `guides/` pages. **Acceptance:** a provider prices + charges a mock
capability end-to-end and a caller pays through the approval loop and branches on
the `failure` object — every command copy-runnable against the `MockFacilitator`;
fail-closed (paid capability with no flow → `denied`) shown, not hand-waved.

### Phase 3 — SDK payments pages (per language)

`sdk/rust|typescript|python/payments.md` (full surface) + `sdk/go/payments.md`
(verifier-only). Rides the existing SDK spine + language gating.
**Acceptance:** each language's snippets match the shipped surface (verified
against `sdk-ts` / `sdk-py` / the bindings / `net-payments`); the availability
matrix (Rust/Python/Node full; Go verifier-only; C absent; crates.io caveat;
`payments-http` opt-in) is stated on the page, not faked.

### Phase 4 — Reference + agent briefs

The four `reference/` pages (envelopes, failure schematic, x402-carry, status
vocabulary) + the payments `agent-briefs/`. **Acceptance:** the envelope + failure
schematic reference matches the golden vectors byte-for-byte; briefs are
executable-by-agent and cross-link the skill.

### Phase 5 — Testnet runbook / live payments (gated, optional)

The env-gated live-network walkthrough (the `payments-live.yml` path; Base
Sepolia). Gated on real-network access + a maintained testnet facilitator;
ship as a runbook, not a quickstart. Deferred unless a live demo is prioritized.

---

## Skill → docs reconciliation

The `net-payments` skill is the draft; the docs are the public cut. One-to-one
where possible so both stay in sync:

| Skill file | Docs page |
|---|---|
| `concepts.md`, `SKILL.md` | `payments/README.md`, `payments/what-net-payments-is.md` |
| `object-model.md` | `payments/x402-and-net.md`, `reference/payments-envelopes.md` |
| `x402.md` | `payments/x402-and-net.md`, `reference/x402-carry.md` |
| `provider.md`, `caller.md` | `payments/the-lifecycle.md`, `guides/price-a-capability.md`, `guides/pay-to-invoke.md` |
| `verification.md` | `payments/verification-tiers.md` |
| `spend-policy.md` | `payments/spend-policy-and-approvals.md`, `guides/approve-and-budget.md` |
| `signer.md` | `payments/non-custodial-signing.md` |
| `networks.md` | `payments/networks.md`, `guides/enable-a-network.md` |
| `failure-schematic.md` | `payments/failure-schematic.md`, `reference/payment-failure-schematic.md` |
| `billing.md` | `payments/billing.md` |
| `facilitator.md` | `guides/enable-a-network.md` |
| `http402.md` | `guides/pay-an-http-402-api.md` |
| `bindings.md` | `sdk/<lang>/payments.md` |
| `testing.md` | `agent-briefs/*`, Phase 0 audit |
| `gotchas.md` | the "what it is NOT" blocks + Risks below |

---

## Risks & non-goals

- **Reading like a payment processor (the #1 risk).** Mitigated by the frozen
  category line + the mandatory "it is NOT" block on every concept page +
  Phase 0. If a reader thinks Net custodies funds, the docs failed.
- **"Commercial facts" read as a data license (PII leak).** The phrase could be
  taken as permission to route customer/billing-profile/KYB data through Net.
  Mitigated by the bounded definition (Positioning) + the PII boundary (Doctrine
  9) + the Phase 0 envelope-shape audit. Provider/customer/KYB records stay in
  provider/partner systems; Net carries only opaque references + signed evidence.
- **Terms read as a terms service.** "Terms support" could imply Net hosts /
  validates / adjudicates terms. Mitigated by Doctrine 10 (signed evidence +
  hashes/IDs only) + the reserved-beyond-that note.
- **Overstating the network ladder (esp. XRPL).** Listing XRPL as shipped-active
  when it is built-but-enablement-gated (or a rung as live when its conformance
  is env-gated) would write a check the code doesn't cash. Mitigated by the
  per-rung go/no-go audit in Phase 0 and the ladder wording in `payments/networks.md`.
- **Implying reserved features.** Disputes/refunds, RFQ/dynamic pricing,
  accounts/postpaid, inbound-402 serving are reserved/deferred — Phase 0 pins
  them; pages name them "reserved", never demo them.
- **Per-language availability confusion.** A Rust user could `cargo add`
  expecting the payments engine and not find it (net-payments isn't on
  crates.io); a caller could expect the HTTP-402 client in the default package
  (it's opt-in); a reader could assume Node has delegation/A2A because Python
  does (it doesn't yet). Mitigated by the Phase 0 matrix surfaced on every SDK
  page + the payments-scoped wording in Doctrine 8.
- **Docs drifting from the skill / code.** The skill moves fast; the code is
  ground truth. Mitigated by the reconciliation table + re-running Phase 0's
  spot-checks before each phase ships.
- **Non-goals:** building payments (shipped; see `PAYMENTS_IMPLEMENTATION_PLAN.md`
  / `PAYMENTS_P1_IMPLEMENTATION_PLAN.md`), an interactive payments demo,
  documenting disputes/RFQ/accounts (reserved), and any change to the
  `net-payments` skill or to `concepts/`/`reference/` content beyond additive
  new pages.

---

## Immediate next step

On approval, execute **Phase 0** (the payments claims audit →
`PAYMENTS_DOCS_CLAIMS_AUDIT.md`, incl. the reserved-feature list + the
per-language availability matrix), then **Phase 1** (the ten `payments/` pages +
`docs.order.ts` wiring + worldview reconciliation), and stop for review before
Phase 2.
