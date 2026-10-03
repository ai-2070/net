// Live tests for **paid** agent-to-agent tasks over the Node binding
// (`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md`) — the twin of
// bindings/python/tests/test_a2a_paid.py.
//
// This file grows slice by slice. WS-B (this part) covers the provider half:
// `PaymentProvider.serveA2aConfigured`, the catalog it validates (every bound
// and duration a checked `bigint`), the preflight bridge, journal ownership,
// and the operator queue. The caller half (prepare / purchase / submit) lands
// with WS-C/WS-D; until then the requester here is the existing `submitTask`,
// which reaches every free catalog entry and is refused by every paid one.
//
// No sleeps as synchronisation: every wait polls the observable the assertion
// is about, with a deadline and a message naming what it last saw.

import { execFileSync } from 'node:child_process'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, PaymentProvider, a2aDocument, a2aU64 } = binding
const { classifyError, PaymentRefusedError, A2aInvalidArgumentError } = await import('../errors')
const HAS_PAID_A2A = typeof PaymentProvider?.prototype?.serveA2aConfigured === 'function'

const PSK = 'a7'.repeat(32)
const PAID = 'summarize'
const FREE = 'echo'
const REVISION = 'r1'

const MOCK_REQS = [
  {
    scheme: 'mock',
    network: 'mock:net',
    amount: '2500',
    asset: 'musd',
    payTo: 'mock-provider-settle-addr',
    maxTimeoutSeconds: 60,
  },
]

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function tmp(name: string): string {
  return join(mkdtempSync(join(tmpdir(), 'net-a2a-paid-')), name)
}

async function meshUnstarted(): Promise<Any> {
  return NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
}

async function handshake(connector: Any, acceptor: Any): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId())
  await sleep(50)
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
  await accepted
}

function devProvider(mesh: Any, statePath: string): Any {
  return new PaymentProvider(mesh, statePath, undefined, undefined, undefined, true)
}

function offer(pricingTerms?: string, overrides: Record<string, unknown> = {}): Any {
  return {
    revision: REVISION,
    pricingTerms,
    bounds: {
      maxPromptBytes: 1024n,
      maxContextRefs: 8n,
      maxTags: 8n,
      maxTagBytes: 64n,
      maxInFlight: 4n,
    },
    reservationTtlSecs: 600n,
    reservationRetentionSecs: 604800n,
    retentionSecs: 3600n,
    description: 'test service',
    ...overrides,
  }
}

type Ran = { taskId: string; service?: string; revision?: string }

function executor(ran: Ran[]) {
  return async (brief: Any) => {
    ran.push({ taskId: brief.taskId, service: brief.service, revision: brief.revision })
    return `blob://${brief.service}/${brief.taskId}`
  }
}

async function waitState(requester: Any, execId: bigint, taskId: string, want: string): Promise<Any> {
  const deadline = Date.now() + 8000
  let last: string | undefined
  while (Date.now() < deadline) {
    const raw = await requester.taskStatus(execId, taskId)
    if (raw !== null) {
      const rec = JSON.parse(raw)
      last = rec.state.state
      if (last === want) return rec
    }
    await sleep(50)
  }
  throw new Error(`task ${taskId} never reached '${want}' (last='${last}')`)
}

/** One started provider node, no peers: for serve-time refusals. */
async function withLone(fn: (mesh: Any, provider: Any) => Promise<void>): Promise<void> {
  const mesh = await meshUnstarted()
  await mesh.start()
  const provider = devProvider(mesh, tmp('engine.json'))
  try {
    await fn(mesh, provider)
  } finally {
    provider.close()
    await mesh.shutdown()
  }
}

/** A provider node and a requester node, handshaken and started. */
async function withPair(fn: (p: Any, provider: Any, requester: Any) => Promise<void>): Promise<void> {
  const p = await meshUnstarted()
  const r = await meshUnstarted()
  await handshake(r, p)
  await p.start()
  await r.start()
  const provider = devProvider(p, tmp('engine.json'))
  try {
    await fn(p, provider, r)
  } finally {
    provider.close()
    await r.shutdown()
    await p.shutdown()
  }
}

async function paidTerms(mesh: Any, provider: Any): Promise<string> {
  return provider.pricingTerms(`${mesh.nodeId()}/net.a2a.task/${PAID}`, JSON.stringify(MOCK_REQS))
}

/**
 * Try to own `journalPath` from a separate Node process. Prints `SERVED` or
 * `REFUSED:<prefix>` — the OS-level lock is what is under test, so the rival
 * must not share this process's in-process owner registry.
 */
function rivalOwner(journalPath: string): string {
  const script = `
    const { NetMesh, PaymentProvider } = require(${JSON.stringify(resolve(__dirname, '..', 'index.js'))});
    const { mkdtempSync } = require('node:fs'); const { tmpdir } = require('node:os'); const { join } = require('node:path');
    (async () => {
      const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: '${PSK}', permissiveChannels: true });
      await mesh.start();
      const p = new PaymentProvider(mesh, join(mkdtempSync(join(tmpdir(), 'rival-')), 'e.json'), undefined, undefined, undefined, true);
      const b = { maxPromptBytes: 1024n, maxContextRefs: 8n, maxTags: 8n, maxTagBytes: 64n, maxInFlight: 4n };
      const svc = { echo: { revision: 'r1', bounds: b, reservationTtlSecs: 600n, reservationRetentionSecs: 604800n, retentionSecs: 3600n } };
      try { const h = await p.serveA2aConfigured(async () => 'x', svc, ${JSON.stringify(journalPath)}); console.log('SERVED'); h.stop(); }
      catch (e) { console.log('REFUSED:' + String(e.message).split(' ')[0]); }
      p.close(); await mesh.shutdown(); process.exit(0);
    })();`
  return execFileSync(process.execPath, ['-e', script], { encoding: 'utf8', timeout: 60000 }).trim().split('\n').pop()!
}

async function rejection(p: Promise<unknown>): Promise<string> {
  try {
    await p
  } catch (e) {
    return (e as Error).message
  }
  throw new Error('expected a rejection')
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — provider (WS-B)', () => {
  it('serves a catalog: the free entry runs through submitTask, the paid one is refused unpaid', async () => {
    await withPair(async (p, provider, r) => {
      const ran: Ran[] = []
      const terms = await paidTerms(p, provider)
      const handle = await provider.serveA2aConfigured(
        executor(ran),
        { [PAID]: offer(terms), [FREE]: offer() },
        tmp('journal.json'),
      )
      try {
        expect(handle.serving).toBe(true)
        expect(handle.services).toBe(5) // task/status/cancel + prepare/describe

        // Free: no prepare, no quote, no proof — and the executor sees the
        // catalog pair on this path.
        let taskId: string | undefined
        for (let i = 0; i < 8 && taskId === undefined; i++) {
          try {
            taskId = await r.submitTask(p.nodeId(), 'echo this', [], [], undefined, FREE, REVISION)
          } catch {
            await sleep(100)
          }
        }
        expect(taskId).toBeDefined()
        const rec = await waitState(r, p.nodeId(), taskId!, 'completed')
        expect(rec.state.result_ref).toBe(`blob://${FREE}/${taskId}`)
        expect(ran).toEqual([{ taskId, service: FREE, revision: REVISION }])

        // Paid, submitted without a purchase: refused before the executor.
        const msg = await rejection(
          r.submitTask(p.nodeId(), 'summarize this', [], [], 'unpaid-1', PAID, REVISION),
        )
        // The provider answers the payment application status (0x8006) — no
        // reservation, because nothing was prepared. The free verb surfaces
        // it as a status-bearing error; the schematic-carrying refusal is
        // `submitTaskPaid`'s (WS-C).
        expect(msg).toMatch(/status 0x8006: no admission reservation exists/)
        expect(ran.map((x) => x.taskId)).not.toContain('unpaid-1')
      } finally {
        handle.stop()
      }
      expect(handle.serving).toBe(false)
      expect(handle.services).toBe(0)
    })
  }, 30000)

  it('refuses catalogs it cannot serve, before opening any journal', async () => {
    await withLone(async (mesh, provider) => {
      const ex = executor([])
      const terms = await paidTerms(mesh, provider)
      const serve = (services: Any, opts?: Any) =>
        rejection(provider.serveA2aConfigured(ex, services, tmp('j.json'), opts))

      expect(await serve({})).toMatch(/^a2a:invalid_argument: .*at least one service/)
      expect(await serve({ [PAID]: offer(terms) }, { principal: 'anyone' })).toMatch(
        /^a2a:invalid_argument: .*"session_peer"/,
      )
      // A bound the wire cannot carry is a promise core refuses to publish.
      expect(
        await serve({
          [PAID]: offer(terms, {
            bounds: { ...offer().bounds, maxPromptBytes: 1n << 20n },
          }),
        }),
      ).toMatch(/^a2a:invalid_argument: serveA2aConfigured refused to start/)
    })
  }, 30000)

  // Review R4: every bound and duration is a checked u64 `bigint`. Nothing is
  // coerced, nothing wraps, and the refusal names the field.
  it('checks every catalog number as a u64 bigint', async () => {
    await withLone(async (mesh, provider) => {
      const ex = executor([])
      const serve = (entry: Any) =>
        rejection(provider.serveA2aConfigured(ex, { [FREE]: entry }, tmp('j.json')))

      for (const bad of [-1n, 1n << 64n, (1n << 64n) + 5n]) {
        expect(await serve(offer(undefined, { retentionSecs: bad }))).toMatch(
          /^a2a:invalid_argument: services\["echo"\]\.retentionSecs: /,
        )
        expect(
          await serve(offer(undefined, { bounds: { ...offer().bounds, maxTags: bad } })),
        ).toMatch(/^a2a:invalid_argument: services\["echo"\]\.bounds\.maxTags: /)
      }
      // A JS number (or anything else) where a bigint is declared is a type
      // refusal at the boundary — never coerced, so 1.5 / NaN cannot truncate.
      for (const bad of [600, 1.5, Number.NaN, Number.POSITIVE_INFINITY, '600']) {
        let threw = false
        try {
          await provider.serveA2aConfigured(
            ex,
            { [FREE]: offer(undefined, { reservationTtlSecs: bad }) },
            tmp('j.json'),
          )
        } catch {
          threw = true
        }
        expect(threw, `reservationTtlSecs=${String(bad)} must be refused`).toBe(true)
      }
    })
  }, 30000)

  it('owns its journal: a second serve on the same path is refused until the first stops', async () => {
    await withLone(async (mesh, provider) => {
      const path = tmp('journal.json')
      const services = { [FREE]: offer() }
      const first = await provider.serveA2aConfigured(executor([]), services, path)
      try {
        expect(await rejection(provider.serveA2aConfigured(executor([]), services, path))).toMatch(
          /^a2a:journal_owned_elsewhere: /,
        )
      } finally {
        first.stop()
      }
      // Idle case: nothing was launched, so retiring the registration is
      // what releases the journal (the under-work case is a later witness).
      const second = await provider.serveA2aConfigured(executor([]), services, path)
      expect(second.serving).toBe(true)
      second.stop()
    })
  }, 30000)

  it('answers the operator queue only while a journal is live', async () => {
    await withLone(async (mesh, provider) => {
      expect(await rejection(provider.a2aUnresolved())).toMatch(
        /^a2a:invalid_argument: no A2A admission journal is live/,
      )
      const handle = await provider.serveA2aConfigured(
        executor([]),
        { [FREE]: offer() },
        tmp('journal.json'),
      )
      expect(JSON.parse(await provider.a2aUnresolved())).toEqual([])
      // A malformed owner is the caller's input, refused by the shared parser.
      expect(
        await rejection(provider.a2aResolve('{"kind":"relay"}', 't', '{"state":"failed","error":"x"}')),
      ).toMatch(/^a2a:invalid_argument: owner_json kind "relay"/)
      handle.stop()
      // Idle: no launched task holds the store, so the queue goes with the
      // registration — once the operator calls above have let go of the
      // clone each took, which lags their resolution by a moment. Poll the
      // observable rather than assert at an instant (plan D6: operator
      // access follows store lifetime).
      const deadline = Date.now() + 2000
      let last = ''
      while (Date.now() < deadline) {
        try {
          await provider.a2aUnresolved()
          last = 'still answering'
        } catch (e) {
          last = (e as Error).message
          break
        }
        await sleep(20)
      }
      expect(last).toMatch(/no A2A admission journal is live/)
    })
  }, 30000)

  it('close() retires the registration it created, so the mesh can shut down', async () => {
    const mesh = await meshUnstarted()
    await mesh.start()
    const provider = devProvider(mesh, tmp('engine.json'))
    const handle = await provider.serveA2aConfigured(
      executor([]),
      { [FREE]: offer() },
      tmp('journal.json'),
    )
    expect(handle.serving).toBe(true)
    provider.close()
    expect(handle.serving).toBe(false)
    await expect(
      provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j2.json')),
    ).rejects.toThrow(/has been closed/)
    await mesh.shutdown()
  }, 30000)

  it('is owned across processes, not only within one', async () => {
    await withLone(async (_mesh, provider) => {
      const path = tmp('journal.json')
      const handle = await provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, path)
      try {
        expect(rivalOwner(path)).toBe('REFUSED:a2a:journal_owned_elsewhere:')
      } finally {
        handle.stop()
      }
      expect(rivalOwner(path)).toBe('SERVED')
    })
  }, 120000)

  // Review R2 / plan D6: stop() and close() retire the registration; they do
  // NOT release the journal while a launched task can still record its
  // outcome. Only when that task and its terminal write finish does a second
  // owner get in.
  it('keeps the journal owned while a launched task runs, even after stop() and close()', async () => {
    await withPair(async (p, provider, r) => {
      const path = tmp('journal.json')
      let release!: () => void
      const parked = new Promise<void>((done) => (release = done))
      let started = false
      const handle = await provider.serveA2aConfigured(
        async (brief: Any) => {
          started = true
          await parked
          return `blob://${brief.taskId}`
        },
        { [FREE]: offer() },
        path,
      )
      let taskId: string | undefined
      for (let i = 0; i < 8 && taskId === undefined; i++) {
        try {
          taskId = await r.submitTask(p.nodeId(), 'park', [], [], 'parked-1', FREE, REVISION)
        } catch {
          await sleep(100)
        }
      }
      expect(taskId).toBe('parked-1')
      const deadline = Date.now() + 5000
      while (!started && Date.now() < deadline) await sleep(20)
      expect(started, 'the executor must be running before the stop').toBe(true)

      handle.stop()
      provider.close()
      expect(handle.serving).toBe(false)

      // A launched task still holds the journal: a second owner — in this
      // process and in another — is refused, and the operator queue answers.
      const rival = devProvider(p, tmp('engine2.json'))
      try {
        expect(
          await rejection(rival.serveA2aConfigured(executor([]), { [FREE]: offer() }, path)),
        ).toMatch(/^a2a:journal_owned_elsewhere: /)
        expect(rivalOwner(path)).toBe('REFUSED:a2a:journal_owned_elsewhere:')
        expect(JSON.parse(await provider.a2aUnresolved()).length).toBeGreaterThanOrEqual(0)

        // Let the task finish; its terminal write is the last holder.
        release()
        const until = Date.now() + 8000
        let last = ''
        while (Date.now() < until) {
          try {
            const h = await rival.serveA2aConfigured(executor([]), { [FREE]: offer() }, path)
            h.stop()
            last = 'SERVED'
            break
          } catch (e) {
            last = (e as Error).message
          }
          await sleep(50)
        }
        expect(last).toBe('SERVED')
      } finally {
        release()
        rival.close()
      }
    })
  }, 120000)

  // Plan D3: the preflight runs before any work, fails closed on anything
  // but `null`, and is bounded so a wedged callback cannot hold an admission.
  it('runs the preflight on a free direct submit and fails closed', async () => {
    await withPair(async (p, provider, r) => {
      const ran: Ran[] = []
      const seen: Any[] = []
      let mode: 'admit' | 'refuse' | 'throw' | 'hang' = 'admit'
      const handle = await provider.serveA2aConfigured(
        executor(ran),
        { [FREE]: offer() },
        tmp('journal.json'),
        undefined,
        async (args: Any) => {
          seen.push(args)
          if (mode === 'refuse') return 'not this one'
          if (mode === 'throw') throw new Error('preflight blew up')
          if (mode === 'hang') return new Promise(() => {})
          return null
        },
      )
      const submit = (id: string) =>
        r.submitTask(p.nodeId(), 'echo', [], [], id, FREE, REVISION)
      try {
        let ok: string | undefined
        for (let i = 0; i < 8 && ok === undefined; i++) {
          try {
            ok = await submit('pf-admit')
          } catch {
            await sleep(100)
          }
        }
        expect(ok).toBe('pf-admit')
        await waitState(r, p.nodeId(), 'pf-admit', 'completed')
        // The owner is the requester's session peer, carried exactly: read the
        // u64 out of the raw document, never through a JS double.
        const owner = seen[0].ownerJson as string
        expect(owner).toMatch(/"kind":"peer"/)
        expect(BigInt(/"node":(\d+)/.exec(owner)![1])).toBe(r.nodeId())
        expect(JSON.parse(seen[0].offerJson).service_id).toBe(FREE)
        expect(JSON.parse(seen[0].briefJson).task_id).toBe('pf-admit')

        mode = 'refuse'
        expect(await rejection(submit('pf-refuse'))).toMatch(/not this one/)
        mode = 'throw'
        expect(await rejection(submit('pf-throw'))).toMatch(/preflight refused: the preflight (threw|rejected)/)
        mode = 'hang'
        const t0 = Date.now()
        expect(await rejection(submit('pf-hang'))).toMatch(/did not answer within 5000 ms/)
        // A received refusal, inside the caller's 30 s budget (review R3).
        expect(Date.now() - t0).toBeLessThan(15000)
        expect(ran.map((x) => x.taskId)).toEqual(['pf-admit'])
      } finally {
        handle.stop()
      }
    })
  }, 60000)
})

/** The native message of a rejection, wrapped back into an Error. */
async function rejected(p: Promise<unknown>): Promise<Error> {
  return new Error(await rejection(p))
}

/**
 * A `prepared` document naming a reservation the provider never minted.
 * Built as text so the u64 node id is written exactly.
 */
function fakePrepared(providerNode: bigint, taskId: string, prompt: string, service = PAID): string {
  return (
    `{"provider_node":${providerNode},` +
    `"brief":{"task_id":"${taskId}","prompt":"${prompt}","context_refs":[],"tags":[],` +
    `"service":"${service}","revision":"${REVISION}"},` +
    `"offer_hash":"00",` +
    `"reservation":{"task_id":"${taskId}","admission_id":"00",` +
    `"capability":"${providerNode}/net.a2a.task/${service}","commitment":"00",` +
    `"purchase_hash":"00","pricing_terms":null,"expires_at":0}}`
  )
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — requester raw verbs (WS-C)', () => {
  // Review R1 / plan D2a: the documents carry u64 integers a JS double
  // cannot hold. The readers keep them exact; JSON.parse does not.
  it('reads nested documents and u64 fields losslessly', () => {
    const big = 9007199254740993n // 2^53 + 1: not representable as a double
    expect(BigInt(Number(big))).not.toBe(big)
    const env =
      `{"status":"ok","prepared":{"provider_node":${big},"brief":{"task_id":"t"}},` +
      `"quote":{"expires_at_ns":18446744073709551615}}`

    const prepared = a2aDocument(env, '/prepared')
    expect(a2aU64(prepared, '/provider_node')).toBe(big)
    expect(a2aU64(env, '/quote/expires_at_ns')).toBe(18446744073709551615n)
    expect(a2aDocument(env, '')).toContain(`"provider_node":${big}`)
    // The negative control: the ordinary JS round trip changes the id.
    expect(JSON.stringify(JSON.parse(prepared))).not.toContain(String(big))

    const refusals: Array<() => unknown> = [
      () => a2aDocument(env, '/nope'),
      () => a2aDocument('not json', ''),
      () => a2aU64(env, '/status'),
      () => a2aU64('{"n":-1}', '/n'),
      () => a2aU64('{"n":1.5}', '/n'),
    ]
    for (const call of refusals) {
      let caught: unknown
      try {
        call()
      } catch (e) {
        caught = e
      }
      expect(classifyError(caught)).toBeInstanceOf(A2aInvalidArgumentError)
    }
  })

  it('describes a configured catalog, and a free-path provider has nothing to describe', async () => {
    await withPair(async (p, provider, r) => {
      const terms = await paidTerms(p, provider)
      const handle = await provider.serveA2aConfigured(
        executor([]),
        { [PAID]: offer(terms), [FREE]: offer() },
        tmp('journal.json'),
      )
      try {
        let raw: string | undefined
        for (let i = 0; i < 8 && raw === undefined; i++) {
          try {
            raw = await r.describeA2a(p.nodeId())
          } catch {
            await sleep(100)
          }
        }
        expect(raw).toBeDefined()
        const offers = Object.fromEntries(JSON.parse(raw!).map((o: Any) => [o.service_id, o]))
        expect(Object.keys(offers).sort()).toEqual([FREE, PAID].sort())
        expect(offers[PAID].pricing_terms).toBe(terms)
        expect(offers[FREE].pricing_terms ?? null).toBeNull()
        expect(a2aU64(raw!, '/0/bounds/max_in_flight')).toBe(4n)
      } finally {
        handle.stop()
      }
      // The legacy free path serves no describe service.
      const legacy = await p.serveA2a(async () => 'blob://x')
      try {
        expect(await rejection(r.describeA2a(p.nodeId()))).toMatch(/^a2a: describeA2a: /)
      } finally {
        legacy.stop()
      }
    })
  }, 60000)

  it('submitTaskPaid: an unpaid proof is a PaymentRefusedError with its schematic, before the executor', async () => {
    await withPair(async (p, provider, r) => {
      const ran: Ran[] = []
      const terms = await paidTerms(p, provider)
      const handle = await provider.serveA2aConfigured(
        executor(ran),
        { [PAID]: offer(terms) },
        tmp('journal.json'),
      )
      const prepared = fakePrepared(p.nodeId(), 'raw-unpaid', 'summarize')
      const proof = '{"quote_id":"","binding_sig":[]}'
      try {
        // Retry only transport noise from the freshly connected pair.
        let caught: unknown
        for (let i = 0; i < 8; i++) {
          try {
            await r.submitTaskPaid(prepared, proof)
            caught = undefined
            break
          } catch (e) {
            caught = e
            if (!(e as Error).message.startsWith('a2a: ')) break
          }
          await sleep(100)
        }
        const typed = classifyError(caught) as InstanceType<typeof PaymentRefusedError>
        expect(typed, String((caught as Error)?.message)).toBeInstanceOf(PaymentRefusedError)
        expect(typed.message.length).toBeGreaterThan(0)
        expect(typed.schematic).toBeDefined()
        const schematic = JSON.parse(typed.schematic!)
        expect(schematic.object).toBe('net.payment.failure@1')
        expect(schematic.handler_executed).toBe(false)
        expect(schematic.stage).toBe('admission')
        expect(schematic.reason).toBe('no_reservation')
        // No reservation means the provider cannot rule out a payment made
        // elsewhere, so it does not claim one did not happen: `unknown`, the
        // conservative row (Python's twin submits after a real prepare and
        // so sees `no`).
        expect(schematic.funds_moved).toBe('unknown')
        expect(ran).toEqual([])
      } finally {
        handle.stop()
      }
    })
  }, 60000)

  it('submitTaskPaid refuses malformed documents and an undeliverable brief locally', async () => {
    const mesh = await meshUnstarted()
    await mesh.start()
    try {
      const proof = '{"quote_id":"q","binding_sig":[]}'
      for (const bad of ['{}', 'not json']) {
        expect(classifyError(await rejected(mesh.submitTaskPaid(bad, proof)))).toBeInstanceOf(
          A2aInvalidArgumentError,
        )
      }
      expect(
        classifyError(await rejected(mesh.submitTaskPaid(fakePrepared(mesh.nodeId(), 'p', 'x'), 'nope'))),
      ).toBeInstanceOf(A2aInvalidArgumentError)
      // Past the encoded-brief limit: refused before any packet.
      const huge = fakePrepared(mesh.nodeId(), 'big', 'x'.repeat(256 * 1024))
      expect(await rejection(mesh.submitTaskPaid(huge, proof))).toMatch(
        /^a2a:invalid_argument: submitTaskPaid: /,
      )
    } finally {
      await mesh.shutdown()
    }
  }, 30000)

  it('setA2aOrgCaller(null) is accepted and the requester verbs keep working', async () => {
    await withPair(async (p, provider, r) => {
      const handle = await provider.serveA2aConfigured(
        executor([]),
        { [FREE]: offer() },
        tmp('journal.json'),
      )
      try {
        r.setA2aOrgCaller(null)
        let taskId: string | undefined
        for (let i = 0; i < 8 && taskId === undefined; i++) {
          try {
            taskId = await r.submitTask(p.nodeId(), 'echo', [], [], 'org-null', FREE, REVISION)
          } catch {
            await sleep(100)
          }
        }
        expect(taskId).toBe('org-null')
        await waitState(r, p.nodeId(), 'org-null', 'completed')
      } finally {
        handle.stop()
      }
    })
  }, 30000)
})
