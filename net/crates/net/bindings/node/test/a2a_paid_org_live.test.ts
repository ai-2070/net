// Review R6 (NODE_A2A_PAID_ADMISSION_PLAN.md WS-F): paid A2A under the
// org-admitted principal, live, through the built addon — on the same-org
// scenario `org_live.test.ts` mints (`gen_subnet_scenario`'s org artifacts).
//
// The port wires two distinct org-caller slots, and sharing a core node is
// not sharing a slot, so both are witnessed with real traffic:
//
// - `NetMesh.setA2aOrgCaller` — the raw requester verbs (`describeA2a`,
//   `taskStatus`, `cancelTask`, …), each of which builds a fresh SDK mesh;
// - `CapabilityGateway.setA2aOrgCaller` — the gateway's own persistent mesh,
//   which its prepare → purchase → submit lifecycle composes over.
//
// A provider serving under `principal: "same_org"` registers all five A2A
// services as PROTECTED, so nothing reaches it until an identity is
// installed, and clearing the identity shuts the door again before launch.
// The cross-org `granted` principal stays covered by the SDK's
// `a2a_admission_identity` Rust witness only.

import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { afterAll, beforeAll, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, PaymentProvider, CapabilityGateway, OrgCredentials, OrgClient, installOrgAuthority, a2aDocument } =
  binding

const HAS =
  typeof PaymentProvider?.prototype?.serveA2aConfigured === 'function' &&
  typeof CapabilityGateway?.prototype?.setA2aOrgCaller === 'function' &&
  typeof installOrgAuthority === 'function'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any

type SameOrgManifest = {
  psk_hex: string
  provider: { seed_hex: string; entity_id_hex: string; org_id_hex: string; authority_dir: string }
  caller: {
    seed_hex: string
    entity_id_hex: string
    org_id_hex: string
    authority_dir: string
    membership_path: string
    dispatcher_path: string
  }
}

const here = dirname(fileURLToPath(import.meta.url))
const crateRoot = resolve(here, '..', '..', '..')
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const SERVICE = 'summarize'

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

/** Plain mkdir, not mkdtemp: on Windows an mkdtemp directory's owner-only
 *  ACL is refused by the audience-secret loader (see org_live.test.ts). */
function scenarioDir(): string {
  const dir = join(tmpdir(), `a2a-org-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`)
  mkdirSync(dir, { recursive: true })
  return dir
}

async function meshFromSeed(seedHex: string, pskHex: string): Promise<Any> {
  return NetMesh.create({
    bindAddr: '127.0.0.1:0',
    psk: pskHex,
    identitySeed: Buffer.from(seedHex, 'hex'),
    permissiveChannels: true,
  })
}

async function handshake(connector: Any, acceptor: Any): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId())
  await sleep(50)
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
  await accepted
}

/** Scoped discovery ships on the announce path: announce and retry until
 *  `attempt` succeeds (each retry a fresh call). */
async function converge<T>(provider: Any, caller: Any, attempt: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + 60_000
  let last: unknown
  while (Date.now() < deadline) {
    await Promise.all([provider.announceCapabilities({}), caller.announceCapabilities({})]).catch(() => {})
    try {
      const got = await attempt()
      if (got !== undefined) return got
    } catch (e) {
      last = e
    }
    await sleep(500)
  }
  throw new Error(`never converged (last: ${String((last as Error)?.message ?? last)})`)
}

describe.skipIf(!HAS)('paid a2a — org-admitted principal, live (WS-F, R6)', () => {
  let dir = ''
  let m: SameOrgManifest

  beforeAll(() => {
    dir = scenarioDir()
    execFileSync(
      'cargo',
      ['run', '-q', '-p', 'net-mesh-sdk', '--features', 'net,cortex,fixtures', '--example', 'gen_subnet_scenario', '--', dir],
      { cwd: crateRoot, stdio: 'inherit' },
    )
    m = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')) as SameOrgManifest
    // One owner audience per organization (spec §3.4's pre-staging step).
    writeFileSync(
      join(dir, m.caller.authority_dir, 'owner-audience.key'),
      readFileSync(join(dir, m.provider.authority_dir, 'owner-audience.key')),
    )
  }, 600_000)

  afterAll(() => {
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  it('both org-caller slots carry a same-org identity through a paid lifecycle, and clearing them denies before launch', async () => {
    const p = (rel: string) => join(dir, rel)
    // Everything acquired inside the `try`, so a failure anywhere in setup
    // still reaches the cleanup below (PR review).
    let state: string | undefined
    let providerMesh: Any
    let callerMesh: Any
    let provider: Any
    let handle: Any
    let gateway: Any
    let client: Any
    let shutDown = false
    const ran: string[] = []
    const owners: string[] = []
    try {
      state = mkdtempSync(join(tmpdir(), 'a2a-org-state-'))
      providerMesh = await meshFromSeed(m.provider.seed_hex, m.psk_hex)
      callerMesh = await meshFromSeed(m.caller.seed_hex, m.psk_hex)
      installOrgAuthority(providerMesh, p(m.provider.authority_dir))
      installOrgAuthority(callerMesh, p(m.caller.authority_dir))
      await handshake(callerMesh, providerMesh)
      await providerMesh.start()
      await callerMesh.start()

      provider = new PaymentProvider(providerMesh, join(state, 'engine.json'), join(state, 'billing.jsonl'), undefined, undefined, true)
      const terms = await provider.pricingTerms(`${providerMesh.nodeId()}/net.a2a.task/${SERVICE}`, MOCK_REQS)
      handle = await provider.serveA2aConfigured(
        async (brief: Any) => {
          ran.push(brief.taskId)
          return `blob://${brief.taskId}`
        },
        {
          [SERVICE]: {
            revision: 'r1',
            pricingTerms: terms,
            bounds: { maxPromptBytes: 1024n, maxContextRefs: 8n, maxTags: 8n, maxTagBytes: 64n, maxInFlight: 4n },
            reservationTtlSecs: 600n,
            reservationRetentionSecs: 604800n,
            retentionSecs: 3600n,
          },
        },
        join(state, 'journal.json'),
        { principal: 'same_org' },
        async (args: Any) => {
          owners.push(args.ownerJson)
          return null
        },
      )
      gateway = new CapabilityGateway(
        callerMesh,
        undefined,
        join(state, 'spend-policy.json'),
        'dev_test',
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        undefined,
        join(state, 'a2a-purchases.json'),
      )
      const target = providerMesh.nodeId()

      // ---- control: PROTECTED really is protected ----
      await expect(callerMesh.describeA2a(target)).rejects.toThrow()
      const unadmitted = JSON.parse(await gateway.prepareTask(target, SERVICE, 'x', [], [], 'org-denied-0'))
      expect(unadmitted.status, JSON.stringify(unadmitted)).not.toBe('ok')

      // ---- install the identity on BOTH slots ----
      client = OrgClient.bind(
        callerMesh,
        OrgCredentials.create({
          membership: readFileSync(p(m.caller.membership_path)),
          dispatcher: readFileSync(p(m.caller.dispatcher_path)),
          grants: [],
          audienceSecretPaths: [],
        }),
      )
      callerMesh.setA2aOrgCaller(client)
      gateway.setA2aOrgCaller(client)

      // Raw slot: the mesh's own describe reaches the protected catalog.
      const offers = await converge(providerMesh, callerMesh, async () => callerMesh.describeA2a(target))
      expect(JSON.parse(offers)[0].service_id).toBe(SERVICE)

      // Gateway slot: the paid lifecycle.
      const env = await converge(providerMesh, callerMesh, async () => {
        const e = await gateway.prepareTask(target, SERVICE, 'summarize under org', [], [], 'org-task-1')
        return JSON.parse(e).status === 'ok' ? e : undefined
      })
      const prepared = a2aDocument(env, '/prepared')
      const bought = JSON.parse(await gateway.purchaseTask(prepared))
      expect(bought.status, JSON.stringify(bought)).toBe('paid')
      const sent = JSON.parse(await gateway.submitTask(prepared))
      expect(sent.status, JSON.stringify(sent)).toBe('accepted')

      // Raw slot again: status and cancel reach the protected task.
      const done = await converge(providerMesh, callerMesh, async () => {
        const raw = await callerMesh.taskStatus(target, 'org-task-1')
        return raw !== null && JSON.parse(raw).state.state === 'completed' ? raw : undefined
      })
      expect(JSON.parse(done).state.result_ref).toBe('blob://org-task-1')
      expect(await callerMesh.cancelTask(target, 'org-task-1')).toBe(false) // already terminal
      expect(ran).toEqual(['org-task-1'])

      // The preflight saw the admitted ENTITY, not the session peer.
      const owner = owners[owners.length - 1]
      expect(owner).toMatch(/"kind":"entity"/)
      expect(owner).toContain(m.caller.entity_id_hex)

      // ---- clear both slots: denied before launch ----
      gateway.setA2aOrgCaller(null)
      const cleared = JSON.parse(await gateway.prepareTask(target, SERVICE, 'x', [], [], 'org-denied-1'))
      expect(cleared.status, JSON.stringify(cleared)).not.toBe('ok')
      callerMesh.setA2aOrgCaller(null)
      await expect(callerMesh.describeA2a(target)).rejects.toThrow()
      expect(ran).toEqual(['org-task-1'])

      // PR review (cubic): an identity left installed must not block
      // shutdown — the slot holds an SDK org client with its own node
      // reference, and `shutdown()` releases it. Re-install, release the
      // documented handles, and require the shutdown to succeed.
      callerMesh.setA2aOrgCaller(client)

      // Code review: a shutdown REFUSED for a genuine outstanding reference
      // (the open gateway and client) leaves the mesh usable — and must
      // leave its identity installed with it, not silently strip it.
      await expect(callerMesh.shutdown()).rejects.toThrow(/outstanding references/)
      const still = await converge(providerMesh, callerMesh, async () => callerMesh.describeA2a(target))
      expect(JSON.parse(still)[0].service_id).toBe(SERVICE)

      handle.stop()
      provider.close()
      gateway.close()
      client.close()
      handle = provider = gateway = client = undefined
      await callerMesh.shutdown()
      await providerMesh.shutdown()
      shutDown = true
    } finally {
      handle?.stop()
      provider?.close()
      gateway?.close()
      try {
        client?.close()
      } catch {
        /* already closed */
      }
      if (!shutDown) {
        await callerMesh?.shutdown().catch(() => {})
        await providerMesh?.shutdown().catch(() => {})
      }
      if (state) rmSync(state, { recursive: true, force: true })
    }
  }, 300_000)
})
