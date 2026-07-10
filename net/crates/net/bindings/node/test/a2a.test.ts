// Live tests for agent-to-agent task handoff (`HERMES_INTEGRATION_PLAN_V2.md`
// Phase 3): `NetMesh.serveA2a` serves the task lifecycle backed by a JS async
// task executor, and a second node submits / polls / cancels it over the wire
// — the Node twin of bindings/python/tests/test_a2a.py.
//
// Mirrors the Rust `mesh_a2a` tests at the binding layer — the couch test:
// hand off a long job, watch it run, cancel mid-run, and the task's state
// demonstrably flips to `cancelled` with the result discarded; plus a
// completed path where the result comes back as an artifact ref.
//
// One deliberate divergence from the Python suite: Python cancels the
// executor's *coroutine* (asyncio.CancelledError inside its await), so its
// tests assert the handler stopped. A JS Promise cannot be aborted from
// outside — the binding's cancel discards the handler's eventual result and
// records `Cancelled` (see `src/a2a.rs` module docs) — so these tests assert
// the wire-visible contract instead: the state stays `cancelled` even after
// the handler's natural completion point, and no result is ever served.
//
// Present iff the .node was built with the `a2a` feature (the vitest CI job
// is; the suite skips cleanly otherwise).

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh } = binding
const HAS_A2A = typeof NetMesh?.prototype?.serveA2a === 'function'

const PSK = '8e'.repeat(32)

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Mesh = any

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

async function meshUnstarted(): Promise<Mesh> {
  return NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
}

// The Python suite's `_handshake`: acceptor waits for the connector's routed
// handshake while the connector dials — both sides before `start()`.
async function handshake(connector: Mesh, acceptor: Mesh): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId())
  await sleep(50)
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
  await accepted
}

// The first call can lose its reply while the freshly-connected pair settles;
// retry a few times like the Python `_submit_retry`.
async function submitRetry(
  requester: Mesh,
  execId: bigint,
  prompt: string,
  refs: string[],
  attempts = 5,
): Promise<string> {
  let last: unknown
  for (let i = 0; i < attempts; i++) {
    try {
      return await requester.submitTask(execId, prompt, refs)
    } catch (e) {
      last = e
      await sleep(100)
    }
  }
  throw last
}

// Poll the executor's status for `taskId` until it reaches `want`.
async function waitState(
  requester: Mesh,
  execId: bigint,
  taskId: string,
  want: string,
  timeoutMs = 6000,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
): Promise<any> {
  const deadline = Date.now() + timeoutMs
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

describe.skipIf(!HAS_A2A)('a2a', () => {
  it('submits a task, watches it run, and cancels it over the wire', async () => {
    const executor = await meshUnstarted()
    const requester = await meshUnstarted()
    let handle: { stop(): void } | undefined
    try {
      await handshake(requester, executor)
      await executor.start()
      await requester.start()

      // A long job — context rides as a Datafort ref.
      handle = await executor.serveA2a(async (_brief: { taskId: string }) => {
        await sleep(30_000)
        return 'blob://done'
      })
      const execId = executor.nodeId()

      const taskId = await submitRetry(requester, execId, 'grind a long job', ['blob://ctx'])
      expect(typeof taskId).toBe('string')
      expect(taskId.length).toBeGreaterThan(0)

      await waitState(requester, execId, taskId, 'running')

      // Cancel mid-run → the wire-visible state flips and stays cancelled.
      expect(await requester.cancelTask(execId, taskId)).toBe(true)
      await waitState(requester, execId, taskId, 'cancelled')
    } finally {
      handle?.stop()
      await requester.shutdown().catch(() => {})
      await executor.shutdown().catch(() => {})
    }
  }, 25_000)

  it('a cancel racing the dispatch never serves the discarded result', async () => {
    // The guard-armed-at-dispatch window: a cancel accepted right after
    // submit must win — the state reads `cancelled` and stays `cancelled`
    // past the handler's natural completion point, so the handler's (JS-side
    // unabortable) result is provably discarded rather than served.
    const executor = await meshUnstarted()
    const requester = await meshUnstarted()
    const completed: string[] = []
    let handle: { stop(): void } | undefined
    try {
      await handshake(requester, executor)
      await executor.start()
      await requester.start()

      handle = await executor.serveA2a(async (brief: { taskId: string }) => {
        await sleep(1000)
        completed.push(brief.taskId) // the handler itself cannot be aborted
        return 'blob://discarded'
      })
      const execId = executor.nodeId()

      const taskId = await submitRetry(requester, execId, 'job to kill instantly', [])
      // No wait for "running": cancel as close to the dispatch as the wire
      // allows, so the token can trip before the executor's select polls.
      expect(await requester.cancelTask(execId, taskId)).toBe(true)
      await waitState(requester, execId, taskId, 'cancelled')

      // Past the handler's natural completion point: the state must still be
      // `cancelled` — a resolved-after-cancel result never rewrites it.
      await sleep(1500)
      const rec = JSON.parse(await requester.taskStatus(execId, taskId))
      expect(rec.state.state).toBe('cancelled')
    } finally {
      handle?.stop()
      await requester.shutdown().catch(() => {})
      await executor.shutdown().catch(() => {})
    }
  }, 25_000)

  it('completes a task with an artifact ref', async () => {
    const executor = await meshUnstarted()
    const requester = await meshUnstarted()
    let handle: { stop(): void; serving: boolean } | undefined
    try {
      await handshake(requester, executor)
      await executor.start()
      await requester.start()

      // The result is promoted home as an artifact ref, not inlined.
      const h: { stop(): void; serving: boolean } = await executor.serveA2a(
        async () => 'blob://summary-99',
      )
      handle = h
      expect(h.serving).toBe(true)
      const execId = executor.nodeId()

      const taskId = await submitRetry(requester, execId, 'summarize', [])
      const rec = await waitState(requester, execId, taskId, 'completed')
      expect(rec.state.result_ref).toBe('blob://summary-99')
      expect(rec.brief.prompt).toBe('summarize')

      // A finished task cancels to false; an unknown task is null.
      expect(await requester.cancelTask(execId, taskId)).toBe(false)
      expect(await requester.taskStatus(execId, 'nope')).toBeNull()

      h.stop()
      expect(h.serving).toBe(false)
    } finally {
      handle?.stop()
      await requester.shutdown().catch(() => {})
      await executor.shutdown().catch(() => {})
    }
  }, 25_000)
})
