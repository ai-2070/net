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
  addToolCapabilitiesToAnnounce,
  anthropic,
  descriptorFrom,
  gemini,
  isTerminalEvent,
  mcp,
  openai,
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
