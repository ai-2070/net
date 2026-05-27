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

describe('watchTools (polling)', () => {
  // The mesh stub holds a mutable descriptor list — tests poke it
  // to simulate registrations / removals / node-count drift, then
  // assert the watcher diffs them out as Added / Removed /
  // NodeCountChanged events.
  function stubMesh(initial: ToolDescriptor[]): {
    listTools(): ToolDescriptor[]
    set(next: ToolDescriptor[]): void
  } {
    let snapshot = [...initial]
    return {
      listTools: () => [...snapshot],
      set: (next) => {
        snapshot = [...next]
      },
    }
  }

  function makeDesc(toolId: string, nodeCount = 1): ToolDescriptor {
    return descriptorFrom({ name: toolId, description: '' }) as ToolDescriptor &
      Record<string, unknown> as ToolDescriptor & { nodeCount: number } as ToolDescriptor
  }

  // descriptorFrom's nodeCount defaults to 0; override for the
  // stub so we exercise the NodeCountChanged path cleanly.
  function desc(toolId: string, nodeCount: number): ToolDescriptor {
    return { ...descriptorFrom({ name: toolId }), nodeCount }
  }

  it('emits Added when a tool appears after the baseline', async () => {
    const mesh = stubMesh([])
    const ctrl = new AbortController()
    const events: ToolListChange[] = []
    const iter = watchTools(mesh, { intervalMs: 25, signal: ctrl.signal })

    // Add a tool *before* we start consuming so the first tick
    // catches the diff.
    mesh.set([desc('web_search', 1)])

    void (async () => {
      for await (const change of iter) {
        events.push(change)
        if (events.length >= 1) {
          ctrl.abort()
          break
        }
      }
    })()

    await new Promise((r) => setTimeout(r, 100))
    expect(events.length).toBe(1)
    expect(events[0]?.type).toBe('added')
    if (events[0]?.type === 'added') {
      expect(events[0].descriptor.toolId).toBe('web_search')
    }
  })

  it('emits Removed when a tool disappears', async () => {
    const mesh = stubMesh([desc('temp', 1)])
    const ctrl = new AbortController()
    const events: ToolListChange[] = []
    const iter = watchTools(mesh, { intervalMs: 25, signal: ctrl.signal })

    mesh.set([])

    void (async () => {
      for await (const change of iter) {
        events.push(change)
        if (events.length >= 1) {
          ctrl.abort()
          break
        }
      }
    })()

    await new Promise((r) => setTimeout(r, 100))
    expect(events.length).toBe(1)
    expect(events[0]?.type).toBe('removed')
    if (events[0]?.type === 'removed') {
      expect(events[0].descriptor.toolId).toBe('temp')
    }
  })

  it('AbortSignal cancels the polling iterator on the next tick', async () => {
    const mesh = stubMesh([])
    const ctrl = new AbortController()
    const events: ToolListChange[] = []
    let iterationCompleted = false

    const iter = watchTools(mesh, { intervalMs: 25, signal: ctrl.signal })

    const consumeTask = (async () => {
      for await (const change of iter) {
        events.push(change)
      }
      iterationCompleted = true
    })()

    // Let one diff tick pass with no changes — should not produce events.
    await new Promise((r) => setTimeout(r, 60))
    expect(events.length).toBe(0)
    expect(iterationCompleted).toBe(false)

    // Abort. The polling loop sees the signal on its current sleep,
    // rejects the timer Promise, and the iterator exits cleanly.
    ctrl.abort()
    await consumeTask
    expect(iterationCompleted).toBe(true)
    // The for-await terminated without throwing — that's the
    // documented cancellation contract.
  })

  it('pre-aborted signal exits the iterator on the first tick without yielding', async () => {
    const mesh = stubMesh([desc('preexisting', 1)])
    const ctrl = new AbortController()
    ctrl.abort() // Abort BEFORE consuming.
    const events: ToolListChange[] = []
    const iter = watchTools(mesh, { intervalMs: 50, signal: ctrl.signal })

    for await (const change of iter) {
      events.push(change)
    }
    // Mutating the snapshot after the abort must not surface — the
    // iterator has already exited.
    mesh.set([])
    await new Promise((r) => setTimeout(r, 80))
    expect(events.length).toBe(0)
  })

  it('emits NodeCountChanged when publisher count drifts', async () => {
    const mesh = stubMesh([desc('shared_tool', 1)])
    const ctrl = new AbortController()
    const events: ToolListChange[] = []
    const iter = watchTools(mesh, { intervalMs: 25, signal: ctrl.signal })

    mesh.set([desc('shared_tool', 2)])

    void (async () => {
      for await (const change of iter) {
        events.push(change)
        if (events.length >= 1) {
          ctrl.abort()
          break
        }
      }
    })()

    await new Promise((r) => setTimeout(r, 100))
    expect(events.length).toBe(1)
    expect(events[0]?.type).toBe('node_count_changed')
    if (events[0]?.type === 'node_count_changed') {
      expect(events[0].descriptor.toolId).toBe('shared_tool')
      expect(events[0].prevNodeCount).toBe(1)
      expect(events[0].descriptor.nodeCount).toBe(2)
    }
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
