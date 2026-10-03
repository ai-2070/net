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
import { mkdtempSync, readFileSync } from 'node:fs'
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
 * Run a child `node` script and return its stdout. A child that exits
 * non-zero or outlives `timeout` makes `execFileSync` throw an error that,
 * on its own, says only ETIMEDOUT / the exit status — so rethrow it carrying
 * the child's stdout and stderr (both populated even on a timeout), which is
 * where a stalled scenario shows how far it got.
 */
function runChild(args: string[], timeout: number): string {
  try {
    return execFileSync(process.execPath, args, { encoding: 'utf8', timeout })
  } catch (e) {
    const err = e as { message?: string; stdout?: string | Buffer; stderr?: string | Buffer }
    throw new Error(
      `child scenario failed: ${err.message ?? String(e)}\n` +
        `--- child stdout ---\n${String(err.stdout ?? '')}\n` +
        `--- child stderr ---\n${String(err.stderr ?? '')}`,
    )
  }
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
      try { const h = await p.serveA2aConfigured(async () => 'x', svc, ${JSON.stringify(journalPath)}); require('node:fs').writeSync(1, 'SERVED\\n'); h.stop(); }
      catch (e) { require('node:fs').writeSync(1, 'REFUSED:' + String(e.message).split(' ')[0] + '\\n'); }
      p.close(); await mesh.shutdown(); process.exit(0);
    })();`
  return runChild(['-e', script], 60000).trim().split('\n').pop()!
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

  it('owns its journal: a second owner of the same path is refused until the first stops', async () => {
    await withLone(async (mesh, provider) => {
      const path = tmp('journal.json')
      const services = { [FREE]: offer() }
      const first = await provider.serveA2aConfigured(executor([]), services, path)
      // The rival needs a node of its own: a node serves the payments wire
      // once, so a second PaymentProvider cannot share this one. Acquired
      // inside the `try`, so a setup failure still releases it.
      let rivalMesh: Any
      let rival: Any
      try {
        rivalMesh = await meshUnstarted()
        await rivalMesh.start()
        rival = devProvider(rivalMesh, tmp('engine-rival.json'))
        expect(await rejection(rival.serveA2aConfigured(executor([]), services, path))).toMatch(
          /^a2a:journal_owned_elsewhere: /,
        )
      } finally {
        first.stop()
        rival?.close()
        await rivalMesh?.shutdown()
      }
      // Idle case: nothing was launched, so retiring the registration is
      // what releases the journal (the under-work case is a later witness).
      const second = await provider.serveA2aConfigured(executor([]), services, path)
      expect(second.serving).toBe(true)
      second.stop()
    })
  }, 30000)

  // PR review (cubic): one provider, one configured catalog. A second one
  // would replace the journal the operator verbs read and hide the first's
  // queue, so it is refused while the first serves — on any journal path.
  it('serves one configured catalog per provider at a time', async () => {
    await withLone(async (_mesh, provider) => {
      const first = await provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j1.json'))
      try {
        expect(
          await rejection(provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j2.json'))),
        ).toMatch(/^a2a: this PaymentProvider already serves a configured A2A catalog/)
      } finally {
        first.stop()
      }
      const second = await provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j3.json'))
      expect(second.serving).toBe(true)
      second.stop()
      // Stopped handles the caller still holds are not live registrations:
      // stop/serve cycles keep working, and each stopped handle stays
      // stopped (the provider prunes them rather than accumulating them).
      const held: Any[] = [first, second]
      for (let i = 0; i < 10; i++) {
        const h = await provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp(`cycle-${i}.json`))
        expect(h.serving).toBe(true)
        h.stop()
        held.push(h)
      }
      expect(held.every((h) => h.serving === false)).toBe(true)
    })
  }, 30000)

  // Code review: a node already serving the A2A services (here the legacy
  // `mesh.serveA2a`) is a lifecycle conflict, not bad input — the unchanged
  // call succeeds once that handle stops — so it must not classify as
  // `A2aInvalidArgumentError` ("retrying unchanged cannot succeed").
  it('a registration conflict on the node is a lifecycle refusal, not an invalid argument', async () => {
    await withLone(async (mesh, provider) => {
      const legacy = await mesh.serveA2a(async () => 'blob://legacy')
      let refusal: string
      try {
        refusal = await rejection(provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j.json')))
      } finally {
        legacy.stop()
      }
      expect(refusal).toMatch(/^a2a: serveA2aConfigured refused to start: already serving/)
      expect(refusal).not.toMatch(/^a2a:invalid_argument:/)
      const handle = await provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('j2.json'))
      expect(handle.serving).toBe(true)
      handle.stop()
    })
  }, 30000)

  // PR review (cubic): a serve still opening its journal when close() lands
  // must not leave the provider serving a node it was told to release. The
  // race goes either way, so either outcome is accepted — the serve rejects
  // (closed before it registered), or it resolved first and close() retired
  // it — and in both the mesh then shuts down, which is the property.
  it('a close() racing an in-flight serve retires it, so the mesh can shut down', async () => {
    for (let round = 0; round < 5; round++) {
      const mesh = await meshUnstarted()
      let shutDown = false
      let provider: Any
      try {
        await mesh.start()
        provider = devProvider(mesh, tmp('engine.json'))
        const pending = provider.serveA2aConfigured(executor([]), { [FREE]: offer() }, tmp('journal.json'))
        provider.close()
        const outcome = await pending.then(
          (h: Any) => (h.serving ? 'still serving' : 'retired'),
          (e: Error) => e.message,
        )
        expect(outcome).toMatch(/^retired$|has been closed/)
        // The property itself: nothing the race left behind holds the node.
        await mesh.shutdown()
        shutDown = true
      } finally {
        if (!shutDown) {
          provider?.close()
          await mesh.shutdown().catch(() => {})
        }
      }
    }
  }, 60000)

  it('answers the operator queue only while a journal is live', async () => {
    await withLone(async (mesh, provider) => {
      expect(await rejection(provider.a2aUnresolved())).toMatch(
        /^a2a: no A2A admission journal is live/,
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
        const hangMsg = await rejection(submit('pf-hang'))
        // A never-settling Promise is refused either at the 5 s budget or —
        // when nothing references it and V8 collects it first, which a busy
        // run makes likely — as a Promise that was dropped before it settled (the
        // abandoned-Promise witness below forces that path). Both fail closed
        // and neither launches.
        expect(hangMsg).toMatch(
          /preflight refused: the preflight (did not answer within 5000 ms|returned a Promise that was dropped before it settled)/,
        )
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

// ---------------------------------------------------------------------------
// WS-D — the caller flow on CapabilityGateway
// ---------------------------------------------------------------------------

const { CapabilityGateway } = binding

/** A live paid provider: node + engine + billing log + journal + catalog. */
class Provider {
  readonly ran: Ran[] = []
  readonly dir = mkdtempSync(join(tmpdir(), 'net-a2a-provider-'))
  provider: Any
  handle: Any
  constructor(
    readonly mesh: Any,
    readonly preflight?: (args: Any) => Promise<string | null>,
  ) {}

  async serve(): Promise<void> {
    this.provider = new PaymentProvider(
      this.mesh,
      join(this.dir, 'engine.json'),
      join(this.dir, 'billing.jsonl'),
      undefined,
      undefined,
      true,
    )
    const terms = await paidTerms(this.mesh, this.provider)
    this.handle = await this.provider.serveA2aConfigured(
      executor(this.ran),
      { [PAID]: offer(terms), [FREE]: offer() },
      join(this.dir, 'journal.json'),
      undefined,
      this.preflight,
    )
  }

  /** Full restart over the same engine + journal paths. */
  async restart(): Promise<void> {
    this.handle.stop()
    this.provider.close()
    // The journal is released once the stopped registration's holders let
    // go; poll the re-serve rather than assume an instant.
    const deadline = Date.now() + 5000
    for (;;) {
      try {
        await this.serve()
        return
      } catch (e) {
        if (Date.now() > deadline) throw e
        await sleep(50)
      }
    }
  }

  async billing(): Promise<Any[]> {
    return (await this.provider.readBilling()).map((e: string) => JSON.parse(e))
  }

  async waitBilling(n: number): Promise<void> {
    const deadline = Date.now() + 8000
    let seen = 0
    while (Date.now() < deadline) {
      seen = (await this.billing()).length
      if (seen >= n) return
      await sleep(50)
    }
    throw new Error(`billing never reached ${n} events (saw ${seen})`)
  }

  close(): void {
    this.handle?.stop()
    this.provider?.close()
  }
}

/** A live paid caller: node + spend policy + purchase store + gateway. */
class Caller {
  readonly dir = mkdtempSync(join(tmpdir(), 'net-a2a-caller-'))
  gateway: Any
  constructor(
    readonly mesh: Any,
    readonly providerNode: bigint,
    readonly profile = 'dev_test',
  ) {
    this.gateway = this.makeGateway()
  }

  makeGateway(): Any {
    return new CapabilityGateway(
      this.mesh,
      undefined, // pinStorePath
      join(this.dir, 'spend-policy.json'),
      this.profile,
      undefined, // paymentUnsafeMockAutoAllow
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      join(this.dir, 'a2a-purchases.json'),
    )
  }

  /** `prepareTask`, retrying only `busy` (nothing reserved, nothing quoted). */
  async prepare(prompt: string, taskId?: string, service = PAID): Promise<string> {
    let last = ''
    for (let i = 0; i < 8; i++) {
      last = await this.gateway.prepareTask(this.providerNode, service, prompt, [], [], taskId)
      if (JSON.parse(last).status !== 'busy') return last
      await sleep(100)
    }
    return last
  }

  /** The `prepared` handle, read losslessly (D2a). */
  static prepared(envelope: string): string {
    return a2aDocument(envelope, '/prepared')
  }

  async purchase(prepared: string): Promise<Any> {
    return JSON.parse(await this.gateway.purchaseTask(prepared))
  }

  async submit(prepared: string): Promise<Any> {
    return JSON.parse(await this.gateway.submitTask(prepared))
  }

  close(): void {
    this.gateway?.close()
  }
}

/** One provider and N callers, handshaken while unstarted, then started. */
async function withTopology(
  opts: { profiles?: string[]; preflight?: (args: Any) => Promise<string | null> },
  fn: (provider: Provider, callers: Caller[]) => Promise<void>,
): Promise<void> {
  const pmesh = await meshUnstarted()
  const cmeshes: Any[] = []
  for (const _ of opts.profiles ?? ['dev_test']) {
    const c = await meshUnstarted()
    await handshake(c, pmesh)
    cmeshes.push(c)
  }
  await pmesh.start()
  for (const c of cmeshes) await c.start()
  const provider = new Provider(pmesh, opts.preflight)
  const callers: Caller[] = []
  try {
    await provider.serve()
    ;(opts.profiles ?? ['dev_test']).forEach((profile, i) =>
      callers.push(new Caller(cmeshes[i], pmesh.nodeId(), profile)),
    )
    await fn(provider, callers)
  } finally {
    for (const c of callers) c.close()
    provider.close()
    for (const c of cmeshes) await c.shutdown()
    await pmesh.shutdown()
  }
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — caller flow (WS-D)', () => {
  it('prepare is a complete handle and moves no money', async () => {
    await withTopology({}, async (provider, [caller]) => {
      const env = await caller.prepare('summarize this', 'prep-1')
      const parsed = JSON.parse(env)
      expect(parsed.status, env).toBe('ok')
      expect(parsed.quote.network).toBe('mock:net')
      expect(parsed.quote.amount).toBe('2500')
      expect(a2aU64(env, '/quote/expires_at_ns')).toBeGreaterThan(0n)
      const prepared = Caller.prepared(env)
      expect(a2aU64(prepared, '/provider_node')).toBe(caller.providerNode)
      expect(JSON.parse(prepared).brief.task_id).toBe('prep-1')
      expect(await provider.billing()).toEqual([])
      expect(provider.ran).toEqual([])
    })
  }, 60000)

  // Review R1, the happy-path leg: the provider's node id is (almost always)
  // not representable as a double, and the handoff below never parses it.
  it('a paid task is purchased once and runs exactly once, handed off losslessly', async () => {
    await withTopology({}, async (provider, [caller]) => {
      const env = await caller.prepare('summarize this', 'paid-1')
      const prepared = Caller.prepared(env)
      if (BigInt(Number(caller.providerNode)) !== caller.providerNode) {
        // The negative control: the ordinary JS round trip names another
        // node in `provider_node` (the copy inside the `capability` string
        // survives, which is why the field itself is compared).
        const roundTripped = JSON.stringify(JSON.parse(prepared))
        expect(a2aU64(roundTripped, '/provider_node')).not.toBe(caller.providerNode)
      }
      const bought = await caller.purchase(prepared)
      expect(bought.status, JSON.stringify(bought)).toBe('paid')
      await provider.waitBilling(1)
      const sent = await caller.submit(prepared)
      expect(sent.status, JSON.stringify(sent)).toBe('accepted')
      await waitState(caller.mesh, caller.providerNode, 'paid-1', 'completed')
      expect(provider.ran).toEqual([{ taskId: 'paid-1', service: PAID, revision: REVISION }])

      // An identical resubmit converges on the original admission: no second
      // run, no second charge.
      expect((await caller.submit(prepared)).status).toBe('accepted')
      expect(provider.ran.length).toBe(1)
      expect((await provider.billing()).length).toBe(1)
    })
  }, 60000)

  it('a retained-id retry converges on one admission and one quote; an altered brief is refused', async () => {
    await withTopology({}, async (_provider, [caller]) => {
      const first = JSON.parse(await caller.prepare('summarize this', 'retain-1'))
      const again = JSON.parse(await caller.prepare('summarize this', 'retain-1'))
      expect(first.status).toBe('ok')
      expect(again.status).toBe('ok')
      expect(again.quote.quote_id).toBe(first.quote.quote_id)
      expect(again.prepared.reservation.admission_id).toBe(first.prepared.reservation.admission_id)
      const altered = JSON.parse(await caller.prepare('something else', 'retain-1'))
      expect(['conflict', 'rejected']).toContain(altered.status)
    })
  }, 60000)

  it('a service the provider does not serve is rejected, not busy', async () => {
    await withTopology({}, async (_provider, [caller]) => {
      const env = JSON.parse(await caller.prepare('x', 'none-1', 'no-such-service'))
      expect(env.status).toBe('rejected')
      expect(env.retryable).toBe(false)
    })
  }, 60000)

  it('production profile holds the purchase until the operator approves', async () => {
    await withTopology({ profiles: ['production'] }, async (provider, [caller]) => {
      const env = await caller.prepare('summarize held', 'hold-1')
      const prepared = Caller.prepared(env)
      const held = await caller.purchase(prepared)
      expect(held.status, JSON.stringify(held)).toBe('requires_payment_approval')
      expect(held.quote_id).toBe(JSON.parse(env).quote.quote_id)
      expect(await provider.billing()).toEqual([])
      const approved = JSON.parse(await caller.gateway.approvePayment(held.quote_id))
      expect(approved.status).toBe('ok')
      const bought = await caller.purchase(prepared)
      expect(bought.status).toBe('paid')
      expect(bought.quote_id).toBe(held.quote_id)
      expect((await caller.submit(prepared)).status).toBe('accepted')
      await waitState(caller.mesh, caller.providerNode, 'hold-1', 'completed')
      await provider.waitBilling(1)
    })
  }, 60000)

  it('a proof bought by one caller is worthless from another', async () => {
    await withTopology({ profiles: ['dev_test', 'dev_test'] }, async (provider, [buyer, thief]) => {
      const prepared = Caller.prepared(await buyer.prepare('summarize mine', 'cross-1'))
      const bought = await buyer.purchase(prepared)
      expect(bought.status).toBe('paid')
      const proof = a2aDocument(JSON.stringify(bought), '/proof')
      const refused = classifyError(await rejected(thief.mesh.submitTaskPaid(prepared, proof)))
      expect(refused, String((refused as Error).message)).toBeInstanceOf(PaymentRefusedError)
      expect(JSON.parse((refused as Any).schematic).handler_executed).toBe(false)
      expect(provider.ran).toEqual([])
      // Positive control: the buyer's own presentation of the same documents.
      expect(await buyer.mesh.submitTaskPaid(prepared, proof)).toBe('cross-1')
      await waitState(buyer.mesh, buyer.providerNode, 'cross-1', 'completed')
      expect(provider.ran.map((r) => r.taskId)).toEqual(['cross-1'])
    })
  }, 60000)

  it('a re-created gateway resumes the purchase from the store; a provider restart keeps the payment', async () => {
    await withTopology({}, async (provider, [caller]) => {
      const prepared = Caller.prepared(await caller.prepare('across restarts', 'restart-1'))
      const bought = await caller.purchase(prepared)
      expect(bought.status).toBe('paid')
      await provider.waitBilling(1)

      caller.close()
      caller.gateway = caller.makeGateway()
      const rows = await caller.gateway.a2aAttempts()
      expect(JSON.parse(rows)[0].state.state).toBe('paid')
      expect(a2aU64(rows, '/0/key/provider_node')).toBe(caller.providerNode)
      const again = await caller.purchase(prepared)
      expect(again.status).toBe('paid')
      expect(again.proof).toEqual(bought.proof)

      await provider.restart()
      expect((await caller.submit(prepared)).status).toBe('accepted')
      await waitState(caller.mesh, caller.providerNode, 'restart-1', 'completed')
      expect(provider.ran.map((r) => r.taskId)).toEqual(['restart-1'])
      expect((await provider.billing()).length).toBe(1)
    })
  }, 90000)

  // The unresolved-financial class end to end, with the R1 recovery leg on
  // the frozen paths: the provider's row is resolved by its `owner` document
  // and scalar `generation`; the caller's live row (`retained: false`) by
  // `key.provider_node` with NO generation.
  it('a post-payment revocation is unresolved on both sides until each operator resolves it', async () => {
    let calls = 0
    const preflight = async () => (++calls === 1 ? null : 'authority revoked before execution')
    await withTopology({ preflight }, async (provider, [caller]) => {
      const prepared = Caller.prepared(await caller.prepare('summarize then revoke', 'revoke-1'))
      expect((await caller.purchase(prepared)).status).toBe('paid')
      await provider.waitBilling(1)
      const sent = await caller.submit(prepared)
      expect(sent.status, JSON.stringify(sent)).toBe('unexecutable')
      expect(sent.message).toMatch(/revoked/)
      expect(provider.ran).toEqual([])

      const queue = await provider.provider.a2aUnresolved()
      expect(JSON.parse(queue)[0].state.admission).toBe('reconcile')
      await provider.provider.a2aResolve(
        a2aDocument(queue, '/0/owner'),
        'revoke-1',
        '{"state":"failed","error":"refunded out of band"}',
        a2aU64(queue, '/0/generation'),
      )
      expect(JSON.parse(await provider.provider.a2aUnresolved())).toEqual([])

      const rows = await caller.gateway.a2aAttempts()
      expect(JSON.parse(rows)[0].state.state).toBe('paid_unexecutable')
      expect(JSON.parse(rows)[0].retained).toBe(false)
      await caller.gateway.a2aResolveAttempt(
        'revoke-1',
        '{"resolution":"closed","outcome":"refunded","evidence":{"ticket":"OPS-1"}}',
        a2aU64(rows, '/0/key/provider_node'),
      )
      const closed = JSON.parse(await caller.gateway.a2aAttempts())
      expect(closed[0].state.state).toBe('resolved')
      expect(closed[0].state.outcome).toBe('refunded')
    })
  }, 90000)

  // Review R3, the after-payment leg: a preflight that never settles at
  // submit is a refusal inside the budget — no launch, and the paid evidence
  // is kept as an unresolved record rather than dropped.
  it('a never-settling preflight at submit after payment launches nothing and leaves it unresolved', async () => {
    let calls = 0
    const preflight = async () => {
      if (++calls === 1) return null
      return new Promise<string | null>(() => {})
    }
    await withTopology({ preflight }, async (provider, [caller]) => {
      const prepared = Caller.prepared(await caller.prepare('summarize then hang', 'hang-1'))
      expect((await caller.purchase(prepared)).status).toBe('paid')
      await provider.waitBilling(1)
      const t0 = Date.now()
      const sent = await caller.submit(prepared)
      expect(Date.now() - t0).toBeLessThan(20000)
      expect(sent.status, JSON.stringify(sent)).toBe('unexecutable')
      // After payment a preflight refusal — here, its timeout — reaches the
      // caller as the provider's admission revocation (parent plan: a
      // post-payment refusal is reconciliation, never an unpaid rejection).
      expect(sent.message).toMatch(/revoked/)
      expect(sent.schematic?.reason).toBe('admission_revoked')
      expect(provider.ran).toEqual([])
      expect(JSON.parse(await provider.provider.a2aUnresolved())[0].state.admission).toBe(
        'reconcile',
      )
      expect(JSON.parse(await caller.gateway.a2aAttempts())[0].state.state).toBe(
        'paid_unexecutable',
      )
    })
  }, 90000)

  it('the paid verbs refuse a gateway without a purchase store, and a closed gateway', async () => {
    const mesh = await meshUnstarted()
    await mesh.start()
    const dir = mkdtempSync(join(tmpdir(), 'net-a2a-gw-'))
    try {
      // A purchase path with no spend policy is refused at construction.
      expect(
        () =>
          new CapabilityGateway(
            mesh,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            join(dir, 'p.json'),
          ),
      ).toThrow(/^gateway: a2aPurchasePath requires paymentPolicyPath/)

      const noStore = new CapabilityGateway(mesh, undefined, join(dir, 'policy.json'), 'dev_test')
      expect(await rejection(noStore.prepareTask(mesh.nodeId(), PAID, 'x'))).toMatch(
        /^gateway: paid A2A needs a2aPurchasePath/,
      )
      noStore.close()

      const bare = new CapabilityGateway(mesh)
      expect(await rejection(bare.a2aAttempts())).toMatch(
        /^gateway: paid A2A needs paymentPolicyPath and a2aPurchasePath/,
      )
      bare.close()

      const full = new Caller(mesh, mesh.nodeId())
      full.close()
      expect(JSON.parse(await full.gateway.prepareTask(mesh.nodeId(), PAID, 'x')).status).toBe(
        'closed',
      )
      expect(await rejection(full.gateway.a2aResolveAttempt('t', '{}'))).toMatch(/^gateway: /)
      // A malformed prepared document is the caller's input.
      const open = new Caller(mesh, mesh.nodeId())
      expect(classifyError(await rejected(open.gateway.purchaseTask('{}')))).toBeInstanceOf(
        A2aInvalidArgumentError,
      )
      open.close()
    } finally {
      await mesh.shutdown()
    }
  }, 30000)
})

// ---------------------------------------------------------------------------
// WS-F — the cross-binding shape fixture
// ---------------------------------------------------------------------------

// `tests/cross_lang_a2a_paid/envelopes.json` was captured from the Python
// binding before WS-A moved the projection into shared Rust; Python's
// `test_a2a_paid_cross_lang.py` asserts the same file. Same scenario, same
// masking: every key and list length, booleans and the closed vocabularies
// literally, every per-run value masked to its type. Every integer masks to
// "<int>", so a u64 a JS double would round is pinned by position without any
// reader holding its value — which is why `JSON.parse` is safe *here* and
// nowhere a document is handed back.
const FIXTURE = resolve(__dirname, '..', '..', '..', 'tests', 'cross_lang_a2a_paid', 'envelopes.json')
const STABLE = new Set([
  'status',
  'state',
  'admission',
  'kind',
  'network',
  'asset',
  'amount',
  'scheme',
  'outcome',
  'resolution',
  'reason',
  'service',
  'service_id',
  'revision',
  'task_id',
])

function shape(value: unknown, key?: string): unknown {
  if (Array.isArray(value)) return value.map((v) => shape(v, key))
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value as object)
        .sort()
        .map((k) => [k, shape((value as Record<string, unknown>)[k], k)]),
    )
  }
  if (typeof value === 'boolean' || value === null) return value
  if (typeof value === 'number') return Number.isInteger(value) ? '<int>' : '<float>'
  if (typeof value === 'string') return key !== undefined && STABLE.has(key) ? value : '<str>'
  return `<${typeof value}>`
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — cross-binding fixture (WS-F)', () => {
  it('the paid-A2A documents keep the shape the Python binding pins', async () => {
    const docs: Record<string, unknown> = {}

    await withTopology({}, async (provider, [caller]) => {
      const env = await caller.prepare('summarize the cross-lang fixture', 'xl-ok')
      expect(JSON.parse(env).status).toBe('ok')
      docs.prepare_ok = JSON.parse(env)
      docs.prepare_unknown_service = JSON.parse(await caller.prepare('summarize', 'xl-none', 'no-such-service'))
      const prepared = Caller.prepared(env)
      const bought = await caller.purchase(prepared)
      expect(bought.status).toBe('paid')
      docs.purchase_paid = bought
      await provider.waitBilling(1)
      const sent = await caller.submit(prepared)
      expect(sent.status).toBe('accepted')
      docs.submit_accepted = sent
      docs.attempts_after_submit = JSON.parse(await caller.gateway.a2aAttempts())
      // A launched task is in the unresolved class until the terminal hook
      // writes its row, which lands after the registry reports `completed`:
      // poll the queue itself (the race the Python capture hit).
      await waitState(caller.mesh, caller.providerNode, 'xl-ok', 'completed')
      const deadline = Date.now() + 8000
      let queue = JSON.parse(await provider.provider.a2aUnresolved())
      while (queue.length > 0 && Date.now() < deadline) {
        await sleep(20)
        queue = JSON.parse(await provider.provider.a2aUnresolved())
      }
      docs.unresolved_empty = queue
    })

    let calls = 0
    const preflight = async () => (++calls === 1 ? null : 'authority revoked before execution')
    await withTopology({ preflight }, async (provider, [caller]) => {
      const prepared = Caller.prepared(await caller.prepare('summarize then revoke', 'xl-revoked'))
      expect((await caller.purchase(prepared)).status).toBe('paid')
      await provider.waitBilling(1)
      const sent = await caller.submit(prepared)
      expect(sent.status).toBe('unexecutable')
      docs.submit_unexecutable = sent
      docs.attempts_paid_unexecutable = JSON.parse(await caller.gateway.a2aAttempts())
      docs.purchase_after_unexecutable = await caller.purchase(prepared)
      docs.unresolved_reconcile = JSON.parse(await provider.provider.a2aUnresolved())
    })

    const want = JSON.parse(readFileSync(FIXTURE, 'utf8')) as Record<string, unknown>
    expect(Object.keys(docs).sort()).toEqual(Object.keys(want).sort())
    for (const name of Object.keys(want)) {
      expect(shape(docs[name]), `${name} drifted from the Python-captured shape`).toEqual(want[name])
    }
  }, 120000)
})

// ---------------------------------------------------------------------------
// WS-F — R1's caller recovery leg: live vs retained, over a seeded store
// ---------------------------------------------------------------------------

/**
 * The fixture seeder `bindings/python/tests/test_a2a_history_boundary.py`
 * uses: it archives real mock-rail payment evidence through the production
 * `retain_superseded` API and removes the live entry, so one key holds a
 * retained incarnation beside the live one that replaces it. No binding verb
 * can manufacture that supersession race; this is explicit fixture
 * supersession, not a claim the race was reproduced.
 *
 * `NET_A2A_SEEDER_EXE` is set by CI's build step; locally it is built here.
 * Hard-fails rather than skipping — a skip would make the witness vacuous on
 * exactly the machines that run it.
 */
function seederExecutable(): string {
  const prebuilt = process.env.NET_A2A_SEEDER_EXE
  if (prebuilt) return prebuilt
  const crateRoot = resolve(__dirname, '..', '..', '..')
  const out = execFileSync(
    'cargo',
    ['test', '-p', 'net-payments', '--features', 'mesh', '--tests', '--no-run', '--message-format', 'json'],
    { cwd: crateRoot, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 },
  )
  for (const line of out.split('\n')) {
    try {
      const msg = JSON.parse(line)
      if (msg.reason === 'compiler-artifact' && msg.executable && msg.target?.name === 'review_python_seed') {
        return msg.executable as string
      }
    } catch {
      // not a JSON line
    }
  }
  throw new Error('cargo reported no executable for review_python_seed')
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — retained vs live resolution (WS-F, R1)', () => {
  // Review S1: a caller generation is a dispatch, not a universal selector.
  // The retained row is resolved by passing its generation; the live row is
  // resolved by omitting it; passing the live row's own generation is
  // refused. The live replacement is driven to `paid_unexecutable` first, so
  // it is eligible for the very same `closed` resolution — wrong routing
  // would close it, and this test would see that.
  it('resolving the retained incarnation leaves the live replacement exactly as it was', async () => {
    const exe = seederExecutable()
    let calls = 0
    const preflight = async () => (++calls <= 2 ? null : 'revoked before execution')
    await withTopology({ preflight }, async (provider, [caller]) => {
      const first = Caller.prepared(await caller.prepare('summarize history', 'history-key'))
      expect((await caller.purchase(first)).status).toBe('paid')
      await provider.waitBilling(1)
      const original = await caller.gateway.a2aAttempts()

      execFileSync(exe, ['--exact', 'review_seed_python_history', '--nocapture'], {
        env: { ...process.env, NET_REVIEW_PURCHASE_PATH: join(caller.dir, 'a2a-purchases.json') },
        encoding: 'utf8',
      })

      const second = Caller.prepared(await caller.prepare('summarize history', 'history-key'))
      expect((await caller.purchase(second)).status).toBe('paid')
      expect((await caller.submit(second)).status).toBe('unexecutable')

      const rows = await caller.gateway.a2aAttempts()
      const parsed = JSON.parse(rows) as Any[]
      expect(parsed.length, rows).toBe(2)
      const hi = parsed.findIndex((r) => r.retained === true)
      const li = parsed.findIndex((r) => r.retained === false)
      expect(hi).toBeGreaterThanOrEqual(0)
      expect(li).toBeGreaterThanOrEqual(0)
      // Same complete key; the retained row is the original incarnation.
      expect(a2aDocument(rows, `/${hi}/key`)).toBe(a2aDocument(rows, `/${li}/key`))
      expect(a2aDocument(rows, `/${hi}/generation`)).toBe(a2aDocument(original, '/0/generation'))
      expect(a2aDocument(rows, `/${hi}/generation`)).not.toBe(a2aDocument(rows, `/${li}/generation`))
      expect(parsed[hi].state.state).toBe('paid_unexecutable')
      expect(parsed[li].state.state).toBe('paid_unexecutable')

      const providerNode = a2aU64(rows, `/${li}/key/provider_node`)
      expect(providerNode).toBe(caller.providerNode)
      const liveBefore = a2aDocument(rows, `/${li}`)
      const closed = '{"resolution":"closed","outcome":"history-only","evidence":{"review":true}}'

      // The live row's own generation names no archived incarnation.
      expect(
        await rejection(
          caller.gateway.a2aResolveAttempt('history-key', closed, providerNode, a2aDocument(rows, `/${li}/generation`)),
        ),
      ).toMatch(/resolve_superseded_attempt/)

      // The retained row, by its generation.
      await caller.gateway.a2aResolveAttempt(
        'history-key',
        closed,
        a2aU64(rows, `/${hi}/key/provider_node`),
        a2aDocument(rows, `/${hi}/generation`),
      )
      const after = await caller.gateway.a2aAttempts()
      const afterParsed = JSON.parse(after) as Any[]
      const ai = afterParsed.findIndex((r) => r.retained === true)
      const al = afterParsed.findIndex((r) => r.retained === false)
      expect(afterParsed[ai].state.state).toBe('resolved')
      expect(afterParsed[ai].state.outcome).toBe('history-only')
      expect(a2aDocument(after, `/${al}`), 'the live replacement must be untouched').toBe(liveBefore)

      // The live row, by omitting the generation.
      await caller.gateway.a2aResolveAttempt(
        'history-key',
        '{"resolution":"closed","outcome":"refunded"}',
        providerNode,
      )
      const done = JSON.parse(await caller.gateway.a2aAttempts()) as Any[]
      expect(done.find((r) => r.retained === false).state.state).toBe('resolved')
    })
  }, 900000)
})

// ---------------------------------------------------------------------------
// An abandoned Promise is reported as one (found at PR review)
// ---------------------------------------------------------------------------

/**
 * Run the GC scenario in a child `node --expose-gc`: V8 collects a pending
 * Promise nothing references, and the only way to make that deterministic is
 * to call `gc()` while the bridge awaits it.
 */
function abandonedPromiseScenario(): { preflight: string; preflightMs: number; executor: string } {
  const script = `
    const { NetMesh, PaymentProvider } = require(${JSON.stringify(resolve(__dirname, '..', 'index.js'))});
    const { mkdtempSync } = require('node:fs'); const { tmpdir } = require('node:os'); const { join } = require('node:path');
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    (async () => {
      const dir = mkdtempSync(join(tmpdir(), 'abandoned-'));
      const mk = () => NetMesh.create({ bindAddr: '127.0.0.1:0', psk: '${PSK}', permissiveChannels: true });
      const p = await mk(); const r = await mk();
      const accepted = p.accept(r.nodeId()); await sleep(50);
      await r.connect(p.localAddr(), p.publicKey(), p.nodeId()); await accepted;
      await p.start(); await r.start();
      const provider = new PaymentProvider(p, join(dir, 'e.json'), undefined, undefined, undefined, true);
      const bounds = { maxPromptBytes: 1024n, maxContextRefs: 8n, maxTags: 8n, maxTagBytes: 64n, maxInFlight: 4n };
      const svc = { echo: { revision: 'r1', bounds, reservationTtlSecs: 600n, reservationRetentionSecs: 604800n, retentionSecs: 3600n } };
      const gcLoop = setInterval(() => global.gc(), 20);

      // 1. A preflight returning a Promise nothing else references.
      const handle = await provider.serveA2aConfigured(async () => 'x', svc, join(dir, 'j.json'), undefined,
        () => new Promise(() => {}));
      let preflight = 'ADMITTED'; let preflightMs = -1;
      for (let i = 0; i < 8; i++) {
        const t0 = Date.now();
        try { await r.submitTask(p.nodeId(), 'x', [], [], 'pf-gc', 'echo', 'r1'); break; }
        catch (e) {
          preflight = String(e.message); preflightMs = Date.now() - t0;
          if (preflight.includes('preflight')) break;
          await sleep(100);
        }
      }
      handle.stop();

      // 2. A task executor returning the same kind of Promise.
      const legacy = await p.serveA2a(() => new Promise(() => {}));
      let id;
      for (let i = 0; i < 8 && !id; i++) {
        try { id = await r.submitTask(p.nodeId(), 'x', [], [], 'ex-gc'); } catch { await sleep(100); }
      }
      let executor = 'NEVER_FAILED';
      const deadline = Date.now() + 20000;
      while (Date.now() < deadline) {
        const raw = await r.taskStatus(p.nodeId(), 'ex-gc');
        const st = raw && JSON.parse(raw).state;
        if (st && st.state === 'failed') { executor = st.error; break; }
        await sleep(50);
      }
      clearInterval(gcLoop);
      legacy.stop(); provider.close();
      require('node:fs').writeSync(1, 'RESULT ' + JSON.stringify({ preflight, preflightMs, executor }) + '\\n');
      process.exit(0);
    })().catch((e) => { require('node:fs').writeSync(1, 'ERROR ' + e.message + '\\n'); process.exit(1); });`
  const out = runChild(['--expose-gc', '-e', script], 120000)
  const line = out.split('\n').find((l) => l.startsWith('RESULT '))
  if (!line) throw new Error(`scenario produced no result:\n${out}`)
  return JSON.parse(line.slice('RESULT '.length))
}

describe.skipIf(!HAS_PAID_A2A)('paid a2a — abandoned Promises (PR review)', () => {
  // A pending Promise that nothing references can never settle; V8 may
  // collect it, and napi then reports a dropped channel. Both JS bridges
  // name that for what it is — at once, not after the budget, and not as a
  // rejection — while still refusing (preflight) or failing (executor).
  it('a collected preflight or executor Promise is reported as never settling', () => {
    const r = abandonedPromiseScenario()
    expect(r.preflight).toMatch(/preflight refused: the preflight returned a Promise that was dropped before it settled/)
    // Reported when V8 collected it, well before the 5 s budget.
    expect(r.preflightMs).toBeGreaterThanOrEqual(0)
    expect(r.preflightMs).toBeLessThan(5000)
    expect(r.executor).toMatch(/a2a task handler returned a Promise that was dropped before it settled/)
  }, 180000)
})
