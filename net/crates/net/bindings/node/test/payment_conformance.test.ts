// Two-node paid-lifecycle conformance (PAYMENTS_PY_TS_SDK_GAP_PLAN Part C). The
// runtime twin of the golden vectors: a Node provider PRICES + CHARGES for a
// tool, and a Node caller PAYS through the consent gate — end to end across two
// nodes, entirely in Node, asserting the same status sequence the Rust
// `flow_end_to_end.rs` drives:
//
//   quote -> requires_payment_approval -> approve -> (retry) served
//   a caller with no payment flow -> denied, carrying `failure.reason`
//
// The engine + settlement + spend policy is single-sourced in `net-payments`
// (this is the marshaling twin); `Production` profile holds every mock spend for
// approval (net-payments `production_profile_holds_every_mock_spend_for_approval`),
// and approving the held quote unblocks the redeem
// (`over_cap_surfaces_structured_approval_and_approval_unblocks`).
//
// Present iff the .node was built with `payments` + `publish` (the vitest CI job
// is); the suite skips cleanly otherwise. Timing across two freshly-connected
// nodes (capability propagation, the fresh-handler reply-subscription race) is
// absorbed with the same retry idiom as the Python `test_publish.py`.

import { tmpdir } from 'node:os'

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const NetMesh = binding.NetMesh
const CapabilityGateway = binding.CapabilityGateway
const PaymentProvider = binding.PaymentProvider
const buildPricingTerms = binding.buildPricingTerms

const HAS_ALL = !!(NetMesh && CapabilityGateway && PaymentProvider && buildPricingTerms)

const PSK = '5b'.repeat(32)
const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms))
const tmp = (n: string): string =>
  `${tmpdir()}/net-conf-${n}-${Date.now()}-${Math.random().toString(36).slice(2)}`

// One acceptable x402 requirement on the mock network (2500 musd).
const MOCK_REQS = JSON.stringify([
  {
    scheme: 'mock',
    network: 'mock:net',
    amount: '2500',
    asset: 'musd',
    payTo: 'mock-provider-settle-addr',
    maxTimeoutSeconds: 60,
  },
])

const ECHO = {
  name: 'echo',
  description: 'a priced echo',
  inputSchema: JSON.stringify({ type: 'object', properties: { message: { type: 'string' } } }),
}

// `connector` dials `acceptor` concurrently with the acceptor accepting — the
// SDK cross-node idiom (accept/connect BEFORE start, so a started node's
// receive-loop auto-accept doesn't race the manual accept). Mirrors the Python
// `_handshake`.
async function handshake(connector: any, acceptor: any): Promise<void> {
  await Promise.all([
    acceptor.accept(connector.nodeId()),
    (async () => {
      await sleep(50) // let the accept register before the connect lands
      await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
    })(),
  ])
}

// Poll the caller's index until the named capability propagates from the
// provider; return its cap id.
async function discover(gw: any, name: string, attempts = 40): Promise<string> {
  for (let i = 0; i < attempts; i++) {
    const res = JSON.parse(await gw.search(name))
    if (res.status === 'ok' && res.capabilities.length > 0) return res.capabilities[0].cap_id
    await sleep(150)
  }
  throw new Error(`capability ${name} never propagated to the caller`)
}

// Invoke until the status is one of `wanted` (absorbing transient transport
// races on a freshly-served handler); returns the last parsed result.
async function invokeUntil(
  gw: any,
  capId: string,
  args: unknown,
  wanted: string[],
  attempts = 20,
): Promise<any> {
  let last: any
  for (let i = 0; i < attempts; i++) {
    last = JSON.parse(await gw.invoke(capId, JSON.stringify(args)))
    if (wanted.includes(last.status)) return last
    await sleep(200)
  }
  return last
}

describe.skipIf(!HAS_ALL)('paid lifecycle conformance (two-node)', () => {
  it('quote -> requires_payment_approval -> approve -> served; no-flow -> denied+failure', async () => {
    const provider = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    const caller = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    let pubHandle: any
    let pp: any
    let gw: any
    let gwNoPay: any
    try {
      // Handshake (caller dials provider) BEFORE start, then start both (async
      // NAPI methods — await so both nodes are up before publishing/invoking).
      await handshake(caller, provider)
      await Promise.all([provider.start(), caller.start()])

      // Provider prices + serves an echo tool, admitting remote callers.
      pp = new PaymentProvider(provider, tmp('prov.state'))
      const terms = buildPricingTerms(pp.providerEntityId, 'prov/echo', MOCK_REQS)
      pubHandle = await pp.publishPaidTools(
        [ECHO],
        async ({ argumentsJson }: { toolName: string; argumentsJson: string }) => ({
          text: `echo: ${JSON.parse(argumentsJson).message ?? ''}`,
        }),
        { echo: terms },
        { allowAnyCaller: true },
      )
      expect(pubHandle.serving).toBe(true)

      // Caller discovers the priced capability over the mesh.
      gw = new CapabilityGateway(caller, null, tmp('caller.policy'), 'production')
      const capId = await discover(gw, 'echo')

      // 1) First invoke — Production holds the mock spend for operator approval.
      const first = await invokeUntil(gw, capId, { message: 'hi' }, ['requires_payment_approval'])
      expect(first.status).toBe('requires_payment_approval')
      expect(first.quote_id).toBeDefined()

      // 2) Operator approves the held quote on the shared spend-policy store.
      const approved = JSON.parse(await gw.approvePayment(first.quote_id))
      expect(approved.status).toBe('ok')
      expect(approved.changed).toBe(true)

      // 3) Retry — the approved quote is paid + redeemed, the tool serves once.
      const served = await invokeUntil(gw, capId, { message: 'hi' }, ['ok'])
      expect(served.status).toBe('ok')
      expect(served.is_error).toBe(false)
      expect(served.text).toContain('hi')

      // 4) A caller with NO payment flow: the paid tool is a structured denial
      //    carrying the provider's `net.payment.failure@1` schematic (reason),
      //    never a throw.
      gwNoPay = new CapabilityGateway(caller, null)
      const denied = await invokeUntil(gwNoPay, capId, { message: 'hi' }, ['denied'])
      expect(denied.status).toBe('denied')
      expect(denied.failure?.reason).toBeDefined()
    } finally {
      // Release every retained node reference (a napi class is GC-finalized,
      // not scope-dropped) so both shutdowns can run deterministically.
      if (pubHandle) pubHandle.stop()
      if (gw) gw.close()
      if (gwNoPay) gwNoPay.close()
      if (pp) pp.close()
      await provider.shutdown().catch(() => {})
      await caller.shutdown().catch(() => {})
    }
  }, 90000)
})
