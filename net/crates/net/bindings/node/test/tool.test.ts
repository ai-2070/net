// Pure-function tests for the Node TS tool layer.
//
// Covers `descriptorFrom`, `addToolCapabilitiesToAnnounce`, and
// all four provider format translators (both directions).
//
// Live-mesh tests (`serveTool` + `callTool` round-trip) are
// deferred to an integration test once we're ready to spin up a
// two-mesh harness. The cross-language byte-equality fixtures
// pinned by T-1 will eventually feed both this file and the Rust
// `formats` module from the same golden vectors.

import { describe, expect, it } from 'vitest'

import {
  ToolCallParseError,
  ToolDescriptor,
  ToolEvent,
  ToolListChange,
  addToolCapabilitiesToAnnounce,
  anthropic,
  callToolStreaming,
  descriptorFrom,
  gemini,
  isTerminalEvent,
  mcp,
  openai,
  serveToolStreaming,
  watchTools,
} from '../tool'
import { NetMesh } from '../index'

const RUN_INTEGRATION_TESTS = process.env.RUN_INTEGRATION_TESTS === '1'

function sampleDescriptor(): ToolDescriptor {
  return descriptorFrom({
    name: 'web_search',
    description: 'Search the web.',
    inputSchema: {
      type: 'object',
      properties: { query: { type: 'string' } },
      required: ['query'],
    },
  })
}

describe('descriptorFrom', () => {
  it('defaults version + flags + arrays', () => {
    const desc = descriptorFrom({ name: 'x' })
    expect(desc.toolId).toBe('x')
    expect(desc.name).toBe('x')
    expect(desc.version).toBe('1.0.0')
    expect(desc.stateless).toBe(true)
    expect(desc.streaming).toBe(false)
    expect(desc.estimatedTimeMs).toBe(0)
    expect(desc.tags).toEqual([])
    expect(desc.requires).toEqual([])
    expect(desc.nodeCount).toBe(0)
    expect(desc.inputSchema).toBeUndefined()
  })

  it('serializes JSON schemas to strings', () => {
    const desc = sampleDescriptor()
    expect(typeof desc.inputSchema).toBe('string')
    const parsed = JSON.parse(desc.inputSchema!)
    expect(parsed.properties.query).toEqual({ type: 'string' })
  })
})

describe('isTerminalEvent', () => {
  it('flags result / error events as terminal', () => {
    expect(isTerminalEvent({ type: 'result', data: 1 })).toBe(true)
    expect(isTerminalEvent({ type: 'error', code: 'x', message: 'y' })).toBe(true)
  })
  it('flags non-terminal events as non-terminal', () => {
    expect(isTerminalEvent({ type: 'start', toolId: 'x' })).toBe(false)
    expect(isTerminalEvent({ type: 'progress', pct: 50 })).toBe(false)
    expect(isTerminalEvent({ type: 'delta', data: 1 })).toBe(false)
  })
})

describe('addToolCapabilitiesToAnnounce', () => {
  it('merges ai-tool tag + ToolJs entry on a fresh CapabilitySetJs', () => {
    const desc = sampleDescriptor()
    const caps = addToolCapabilitiesToAnnounce({}, [desc])
    expect(caps.tags).toContain('ai-tool:web_search')
    expect(caps.tools?.[0]?.toolId).toBe('web_search')
  })

  it('preserves caller-supplied tags + dedupes', () => {
    const desc = sampleDescriptor()
    const caps = addToolCapabilitiesToAnnounce(
      { tags: ['region.eu', 'ai-tool:web_search'] },
      [desc],
    )
    // ai-tool:web_search must appear exactly once.
    const occurrences = caps.tags!.filter((t) => t === 'ai-tool:web_search').length
    expect(occurrences).toBe(1)
    // Region tag preserved.
    expect(caps.tags).toContain('region.eu')
  })

  it('no-ops on an empty descriptor list', () => {
    const caps = addToolCapabilitiesToAnnounce({ tags: ['x'] }, [])
    expect(caps.tags).toEqual(['x'])
    expect(caps.tools).toBeUndefined()
  })
})

describe('openai format', () => {
  it('to_openai_tool emits function envelope + strict when schema present', () => {
    const tool = openai.toOpenaiTool(sampleDescriptor()) as {
      type: string
      function: { name: string; description: string; parameters: object; strict: boolean }
    }
    expect(tool.type).toBe('function')
    expect(tool.function.name).toBe('web_search')
    expect(tool.function.description).toBe('Search the web.')
    expect(tool.function.strict).toBe(true)
    expect((tool.function.parameters as Record<string, unknown>).type).toBe('object')
  })

  it('lower_openai_tool_call extracts name + arguments + id', () => {
    const spec = openai.lowerOpenaiToolCall({
      id: 'call_abc',
      type: 'function',
      function: { name: 'web_search', arguments: '{"query":"mesh"}' },
    })
    expect(spec.name).toBe('web_search')
    expect(spec.argumentsJson).toBe('{"query":"mesh"}')
    expect(spec.providerCallId).toBe('call_abc')
  })

  it('lower_openai_tool_call rejects malformed arguments string', () => {
    expect(() =>
      openai.lowerOpenaiToolCall({
        function: { name: 'x', arguments: 'not valid json {' },
      }),
    ).toThrow(ToolCallParseError)
  })
})

describe('anthropic format', () => {
  it('to_anthropic_tool uses snake_case input_schema', () => {
    const tool = anthropic.toAnthropicTool(sampleDescriptor()) as {
      name: string
      description: string
      input_schema: Record<string, unknown>
    }
    expect(tool.name).toBe('web_search')
    expect(tool.description).toBe('Search the web.')
    expect(tool.input_schema.type).toBe('object')
    expect('strict' in tool).toBe(false)
  })

  it('lower_anthropic_tool_use serializes input + carries id', () => {
    const spec = anthropic.lowerAnthropicToolUse({
      type: 'tool_use',
      id: 'toolu_xyz',
      name: 'web_search',
      input: { query: 'mesh', max_results: 5 },
    })
    expect(spec.name).toBe('web_search')
    const parsed = JSON.parse(spec.argumentsJson)
    expect(parsed.query).toBe('mesh')
    expect(parsed.max_results).toBe(5)
    expect(spec.providerCallId).toBe('toolu_xyz')
  })
})

describe('mcp format', () => {
  it('to_mcp_tool uses camelCase inputSchema', () => {
    const tool = mcp.toMcpTool(sampleDescriptor()) as {
      name: string
      description: string
      inputSchema: Record<string, unknown>
    }
    expect(tool.name).toBe('web_search')
    expect(tool.inputSchema.type).toBe('object')
  })

  it('lower_mcp_tools_call leaves providerCallId undefined', () => {
    const spec = mcp.lowerMcpToolsCall({
      name: 'web_search',
      arguments: { query: 'mesh' },
    })
    expect(spec.name).toBe('web_search')
    const parsed = JSON.parse(spec.argumentsJson)
    expect(parsed.query).toBe('mesh')
    expect(spec.providerCallId).toBeUndefined()
  })
})

describe('gemini format', () => {
  it('to_gemini_function_declaration uses parameters field', () => {
    const decl = gemini.toGeminiFunctionDeclaration(sampleDescriptor()) as {
      name: string
      description: string
      parameters: Record<string, unknown>
    }
    expect(decl.name).toBe('web_search')
    expect(decl.parameters.type).toBe('object')
  })

  it('lower_gemini_function_call reads args field', () => {
    const spec = gemini.lowerGeminiFunctionCall({
      name: 'web_search',
      args: { query: 'mesh' },
    })
    expect(spec.name).toBe('web_search')
    const parsed = JSON.parse(spec.argumentsJson)
    expect(parsed.query).toBe('mesh')
    expect(spec.providerCallId).toBeUndefined()
  })
})

describe('watchTools (event-driven)', () => {
  // The fake mesh's watchTools() returns a native iter that yields the
  // pre-seeded JSON changes (exactly as the napi `ToolWatchIter` would),
  // then ends with `null`. Records the intervalMs the wrapper passes
  // through and whether `close()` fired. Mirror of the Python E-4
  // fake-mesh tests — the live substrate delivery is validated
  // substrate-side + cross-language.
  function desc(toolId: string, nodeCount: number): ToolDescriptor {
    return { ...descriptorFrom({ name: toolId }), nodeCount }
  }

  function fakeMesh(changes: ToolListChange[]) {
    const state = {
      lastIntervalMs: undefined as number | null | undefined,
      closed: false,
    }
    const queue = changes.map((c) => JSON.stringify(c))
    const native = {
      async next(): Promise<string | null> {
        return queue.length ? (queue.shift() as string) : null
      },
      close() {
        state.closed = true
      },
    }
    return {
      state,
      mesh: {
        async watchTools(intervalMs?: number | null) {
          state.lastIntervalMs = intervalMs
          return native
        },
      },
    }
  }

  // A native iter whose `next()` pends until `close()` — models a
  // ceiling-less watch parked on the fold change with nothing pending.
  function blockingFakeMesh() {
    const state = { closed: false }
    let resolvePending: ((v: string | null) => void) | null = null
    const native = {
      async next(): Promise<string | null> {
        if (state.closed) return null
        return new Promise<string | null>((resolve) => {
          resolvePending = resolve
        })
      },
      close() {
        state.closed = true
        if (resolvePending) {
          resolvePending(null)
          resolvePending = null
        }
      },
    }
    return {
      state,
      mesh: {
        async watchTools(_intervalMs?: number | null) {
          return native
        },
      },
    }
  }

  it('parses each change variant and closes the native iter on completion', async () => {
    const { state, mesh } = fakeMesh([
      { type: 'added', descriptor: desc('web_search', 1) },
      { type: 'removed', descriptor: desc('old_tool', 1) },
      {
        type: 'node_count_changed',
        descriptor: desc('web_search', 3),
        prevNodeCount: 1,
      },
    ])

    const events: ToolListChange[] = []
    for await (const change of watchTools(mesh)) {
      events.push(change)
    }

    expect(events.map((e) => e.type)).toEqual([
      'added',
      'removed',
      'node_count_changed',
    ])
    expect(events[0]?.type === 'added' && events[0].descriptor.toolId).toBe(
      'web_search',
    )
    expect(events[1]?.type === 'removed' && events[1].descriptor.toolId).toBe(
      'old_tool',
    )
    if (events[2]?.type === 'node_count_changed') {
      expect(events[2].prevNodeCount).toBe(1)
      expect(events[2].descriptor.nodeCount).toBe(3)
    }
    // The wrapper ALWAYS closes the native iter in its `finally`.
    expect(state.closed).toBe(true)
  })

  it('omitted interval maps to pure event-driven (null), positive to a ceiling', async () => {
    const a = fakeMesh([])
    for await (const _ of watchTools(a.mesh)) {
      // drains immediately (empty)
    }
    expect(a.state.lastIntervalMs).toBeNull()

    const b = fakeMesh([])
    for await (const _ of watchTools(b.mesh, { intervalMs: 500 })) {
      // drains immediately (empty)
    }
    expect(b.state.lastIntervalMs).toBe(500)
  })

  it('subscribes eagerly at call time, not on the first iteration', async () => {
    // The substrate baseline is taken when the native watch is created.
    // The wrapper must call `mesh.watchTools(...)` when `watchTools` is
    // CALLED — not defer it to the first `for await` — so a change
    // published before iteration begins is still observed. The fake's
    // `watchTools` sets `lastIntervalMs` synchronously; if subscription
    // were lazy it would still be `undefined` here.
    const { state, mesh } = fakeMesh([])
    const iterable = watchTools(mesh)
    expect(state.lastIntervalMs).toBeNull()

    // Drain to run the generator's `finally` and close the native iter.
    for await (const _ of iterable) {
      // empty
    }
    expect(state.closed).toBe(true)
  })

  it('AbortSignal closes the native iter and ends iteration', async () => {
    const { state, mesh } = blockingFakeMesh()
    const ctrl = new AbortController()
    let iterationCompleted = false

    const consumeTask = (async () => {
      for await (const _ of watchTools(mesh, { signal: ctrl.signal })) {
        // never yields — next() pends until close()
      }
      iterationCompleted = true
    })()

    // Give the generator a tick to subscribe + park on next().
    await new Promise((r) => setTimeout(r, 20))
    expect(iterationCompleted).toBe(false)

    ctrl.abort()
    await consumeTask
    expect(iterationCompleted).toBe(true)
    expect(state.closed).toBe(true)
  })

  it('pre-aborted signal closes immediately without yielding', async () => {
    const { state, mesh } = fakeMesh([
      { type: 'added', descriptor: desc('preexisting', 1) },
    ])
    const ctrl = new AbortController()
    ctrl.abort()

    const events: ToolListChange[] = []
    for await (const change of watchTools(mesh, { signal: ctrl.signal })) {
      events.push(change)
    }
    expect(events.length).toBe(0)
    expect(state.closed).toBe(true)
  })
})

// Live end-to-end (single node, no handshake): a self-served tool
// announced on a node fires an `Added` to a local `watchTools` watcher
// off the substrate's capability-fold change signal — proving the napi
// `ToolWatchIter` + the wrapper consume the real event stream, and that
// the async-fn `watch_tools` spawns its diff task inside the napi tokio
// runtime. Gated behind RUN_INTEGRATION_TESTS (needs the built binary).
describe.skipIf(!RUN_INTEGRATION_TESTS)('watchTools (live single-node)', () => {
  it('delivers an Added for a self-served tool', async () => {
    const mesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: '42'.repeat(32),
    })
    // `watchTools` subscribes eagerly (kicks off `mesh.watchTools()` at
    // call time); `it.next()` then drives consumption. The sleep lets the
    // napi diff task settle before we announce.
    const it = watchTools(mesh as never)[Symbol.asyncIterator]()
    const firstP = it.next()
    await new Promise((r) => setTimeout(r, 200))

    const caps = addToolCapabilitiesToAnnounce({}, [
      descriptorFrom({ name: 'web_search', description: 'Search the web.' }),
    ])
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await (mesh as any).announceCapabilities(caps)

    const res = await Promise.race([
      firstP,
      new Promise<never>((_, rej) =>
        setTimeout(() => rej(new Error('timeout: no change in 4s')), 4000),
      ),
    ])
    // Closing the iterator drops the substrate watch's receiver, which
    // ends the diff task and releases its node ref — so shutdown sees no
    // outstanding references. Give the task a beat to unwind first.
    await it.return?.()
    await new Promise((r) => setTimeout(r, 100))
    expect(res.done).toBe(false)
    expect(res.value?.type).toBe('added')
    if (res.value?.type === 'added') {
      expect(res.value.descriptor.toolId).toBe('web_search')
    }
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await (mesh as any).shutdown()
  })
})

describe('callToolStreaming', () => {
  // Mock the minimal TypedMeshRpc surface that callToolStreaming
  // consumes: a `callServiceStreaming` that resolves to an object
  // that's both AsyncIterable<ToolEvent> and has a `close()` method.
  function mockRpcYielding(events: ToolEvent[]): any {
    let closed = false
    return {
      callServiceStreaming: async () => {
        let i = 0
        return {
          async next(): Promise<ToolEvent | null> {
            if (closed || i >= events.length) return null
            return events[i++]!
          },
          async *[Symbol.asyncIterator]() {
            while (true) {
              const v = await this.next()
              if (v === null) return
              yield v
            }
          },
          close: async () => {
            closed = true
          },
        }
      },
    }
  }

  it('passes through terminal events without synthesis', async () => {
    const rpc = mockRpcYielding([
      { type: 'start', toolId: 'web_search', callId: 1 },
      { type: 'delta', data: { partial: 'a' } },
      { type: 'result', data: { final: 'ok' } },
    ])
    const collected: ToolEvent[] = []
    for await (const event of callToolStreaming(rpc, 'web_search', {})) {
      collected.push(event)
    }
    expect(collected.length).toBe(3)
    expect(collected[collected.length - 1]?.type).toBe('result')
    // No `missing_terminal` event should appear after the terminal.
    expect(collected.find((e) => e.type === 'error' && (e as any).code === 'missing_terminal'))
      .toBeUndefined()
  })

  it('synthesizes missing_terminal when stream ends without result/error', async () => {
    const rpc = mockRpcYielding([
      { type: 'start', toolId: 'web_search', callId: 2 },
      { type: 'delta', data: { partial: 'partial-only' } },
      // No result/error — stream just ends.
    ])
    const collected: ToolEvent[] = []
    for await (const event of callToolStreaming(rpc, 'web_search', {})) {
      collected.push(event)
    }
    expect(collected.length).toBe(3) // start + delta + synthesized error
    const last = collected[collected.length - 1]!
    expect(last.type).toBe('error')
    if (last.type === 'error') {
      // Exact byte shape pinned by T-2 fixture; the wrapper MUST emit
      // `code: "missing_terminal"` so downstream adapters can match
      // reliably across all four languages.
      expect(last.code).toBe('missing_terminal')
      expect(last.message).toMatch(/terminal result or error/i)
    }
  })

  it('empty stream synthesizes a single missing_terminal event', async () => {
    const rpc = mockRpcYielding([])
    const collected: ToolEvent[] = []
    for await (const event of callToolStreaming(rpc, 'noop_tool', {})) {
      collected.push(event)
    }
    expect(collected.length).toBe(1)
    expect(collected[0]?.type).toBe('error')
    if (collected[0]?.type === 'error') {
      expect(collected[0].code).toBe('missing_terminal')
    }
  })

  it('error-terminal also suppresses the missing_terminal synthesis', async () => {
    const rpc = mockRpcYielding([
      { type: 'start', toolId: 'web_search', callId: 3 },
      { type: 'error', code: 'handler_panicked', message: 'boom' },
    ])
    const collected: ToolEvent[] = []
    for await (const event of callToolStreaming(rpc, 'web_search', {})) {
      collected.push(event)
    }
    expect(collected.length).toBe(2)
    const last = collected[collected.length - 1]!
    expect(last.type).toBe('error')
    if (last.type === 'error') {
      // The handler's `boom` error survives — we don't paper over it
      // with a synthesized envelope.
      expect(last.code).toBe('handler_panicked')
    }
  })
})

// ---------------------------------------------------------------------------
// serveToolStreaming — server-side missing_terminal synthesis (E-8).
// ---------------------------------------------------------------------------

describe('serveToolStreaming server-side terminal synthesis', () => {
  type CapturedHandler = (req: unknown, sink: { send: (e: unknown) => void }) => Promise<void>

  // Captures the wrapped handler serveToolStreaming passes to the
  // underlying rpc.serveStreaming so tests can drive it directly.
  function mockServeRpc(): {
    rpc: any
    captured: { handler: CapturedHandler | null }
  } {
    const captured: { handler: CapturedHandler | null } = { handler: null }
    const rpc = {
      // Unary serve — covers the auto-installed tool.metadata.fetch.
      serve: () => ({ close: () => {} }),
      // Streaming serve — capture the wrapped handler.
      serveStreaming: <Req, Resp>(
        _service: string,
        handler: (req: Req, sink: { send: (e: Resp) => void }) => Promise<void> | void,
      ) => {
        captured.handler = handler as CapturedHandler
        return { close: () => {} }
      },
    }
    return { rpc, captured }
  }

  it('emits missing_terminal when handler returns without a terminal event', async () => {
    const { rpc, captured } = mockServeRpc()
    serveToolStreaming(rpc, { name: 'web_search' }, async function* () {
      yield { type: 'start', toolId: 'web_search' } as ToolEvent
      yield { type: 'delta', data: { partial: 'no terminal' } } as ToolEvent
    })
    expect(captured.handler).not.toBeNull()
    const sent: any[] = []
    await captured.handler!({}, { send: (e) => sent.push(e) })
    // start + delta + synthesized missing_terminal
    expect(sent.length).toBe(3)
    const last = sent[sent.length - 1]
    expect(last.type).toBe('error')
    expect(last.code).toBe('missing_terminal')
  })

  it('does NOT emit missing_terminal when handler yields a result', async () => {
    const { rpc, captured } = mockServeRpc()
    serveToolStreaming(rpc, { name: 'web_search' }, async function* () {
      yield { type: 'start', toolId: 'web_search' } as ToolEvent
      yield { type: 'result', data: { final: 'ok' } } as ToolEvent
    })
    const sent: any[] = []
    await captured.handler!({}, { send: (e) => sent.push(e) })
    expect(sent.length).toBe(2)
    expect(sent[sent.length - 1].type).toBe('result')
    expect(sent.some((e) => e.type === 'error' && e.code === 'missing_terminal')).toBe(false)
  })

  it('handler exception maps to handler_error (no missing_terminal synth)', async () => {
    const { rpc, captured } = mockServeRpc()
    serveToolStreaming(rpc, { name: 'web_search' }, async function* () {
      yield { type: 'start', toolId: 'web_search' } as ToolEvent
      throw new Error('boom')
    })
    const sent: any[] = []
    await captured.handler!({}, { send: (e) => sent.push(e) })
    expect(sent.length).toBe(2)
    const last = sent[sent.length - 1]
    expect(last.type).toBe('error')
    expect(last.code).toBe('handler_error')
    expect(last.message).toContain('boom')
  })
})

describe('empty-schema fallback', () => {
  // Build a descriptor WITHOUT an input schema; every translator
  // must short-circuit to `{type: "object", properties: {}}` so the
  // provider's strict-mode validator accepts it.
  const desc = descriptorFrom({ name: 'no_args', description: 'Bare.' })

  it('openai falls back to empty object + strict=false', () => {
    const tool = openai.toOpenaiTool(desc) as {
      function: { parameters: Record<string, unknown>; strict: boolean }
    }
    expect(tool.function.parameters.type).toBe('object')
    expect(tool.function.strict).toBe(false)
  })
  it('anthropic falls back to empty object', () => {
    const tool = anthropic.toAnthropicTool(desc) as {
      input_schema: Record<string, unknown>
    }
    expect(tool.input_schema.type).toBe('object')
  })
  it('mcp falls back to empty object', () => {
    const tool = mcp.toMcpTool(desc) as { inputSchema: Record<string, unknown> }
    expect(tool.inputSchema.type).toBe('object')
  })
  it('gemini falls back to empty object', () => {
    const decl = gemini.toGeminiFunctionDeclaration(desc) as {
      parameters: Record<string, unknown>
    }
    expect(decl.parameters.type).toBe('object')
  })
})
