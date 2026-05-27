// Cross-language tool-format compatibility fixture test (plan T-1).
//
// Loads `crates/net/tests/cross_lang_tool_formats/golden_vectors.json`
// — the canonical fixture pinning byte-equality across all four
// tool-format translators (Rust / Node TS / Python / Go). Failure of
// any case here signals cross-binding wire-format drift.
//
// Matches the Rust verifier at
// `sdk/tests/tool_format_golden_vectors.rs`.

import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import {
  anthropic,
  gemini,
  mcp,
  openai,
  ToolCallParseError,
  ToolDescriptor,
} from '../tool'

const FIXTURE_PATH = join(
  __dirname,
  '..',
  '..',
  '..',
  'tests',
  'cross_lang_tool_formats',
  'golden_vectors.json',
)

interface FixtureDescriptorCase {
  name: string
  input: {
    tool_id: string
    name: string
    version: string
    description: string | null
    input_schema_object: object | null
    output_schema_object: object | null
    requires: string[]
    estimated_time_ms: number
    stateless: boolean
    streaming: boolean
    tags: string[]
    node_count: number
  }
  lowered_openai: object
  lowered_anthropic: object
  lowered_mcp: object
  lowered_gemini: object
}

interface FixtureLowerCase {
  name: string
  reply_json: Record<string, unknown>
  expected_spec: {
    name: string
    arguments_json?: string
    arguments_parsed?: unknown
    provider_call_id?: string | null
  }
}

interface FixtureErrorCase {
  name: string
  provider: 'openai' | 'anthropic' | 'mcp' | 'gemini'
  reply_json: Record<string, unknown>
}

interface Fixture {
  descriptors: FixtureDescriptorCase[]
  lower_openai_cases: FixtureLowerCase[]
  lower_anthropic_cases: FixtureLowerCase[]
  lower_mcp_cases: FixtureLowerCase[]
  lower_gemini_cases: FixtureLowerCase[]
  error_cases: FixtureErrorCase[]
}

const fixture: Fixture = JSON.parse(readFileSync(FIXTURE_PATH, 'utf8'))

function descriptorFromFixture(input: FixtureDescriptorCase['input']): ToolDescriptor {
  return {
    toolId: input.tool_id,
    name: input.name,
    version: input.version,
    description: input.description ?? undefined,
    inputSchema: input.input_schema_object ? JSON.stringify(input.input_schema_object) : undefined,
    outputSchema: input.output_schema_object ? JSON.stringify(input.output_schema_object) : undefined,
    requires: input.requires,
    estimatedTimeMs: input.estimated_time_ms,
    stateless: input.stateless,
    streaming: input.streaming,
    tags: input.tags,
    nodeCount: input.node_count,
  }
}

describe('descriptor lowerings match golden vectors', () => {
  for (const c of fixture.descriptors) {
    it(c.name, () => {
      const desc = descriptorFromFixture(c.input)
      expect(openai.toOpenaiTool(desc)).toEqual(c.lowered_openai)
      expect(anthropic.toAnthropicTool(desc)).toEqual(c.lowered_anthropic)
      expect(mcp.toMcpTool(desc)).toEqual(c.lowered_mcp)
      expect(gemini.toGeminiFunctionDeclaration(desc)).toEqual(c.lowered_gemini)
    })
  }
})

function assertLowerSpec(
  got: { name: string; argumentsJson: string; providerCallId?: string },
  expected: FixtureLowerCase['expected_spec'],
): void {
  expect(got.name).toBe(expected.name)
  if (typeof expected.arguments_json === 'string') {
    expect(got.argumentsJson).toBe(expected.arguments_json)
  }
  if (expected.arguments_parsed !== undefined) {
    expect(JSON.parse(got.argumentsJson)).toEqual(expected.arguments_parsed)
  }
  if (expected.provider_call_id === null || expected.provider_call_id === undefined) {
    expect(got.providerCallId).toBeUndefined()
  } else {
    expect(got.providerCallId).toBe(expected.provider_call_id)
  }
}

describe('lower_openai matches golden vectors', () => {
  for (const c of fixture.lower_openai_cases) {
    it(c.name, () => {
      const spec = openai.lowerOpenaiToolCall(c.reply_json)
      assertLowerSpec(spec, c.expected_spec)
    })
  }
})

describe('lower_anthropic matches golden vectors', () => {
  for (const c of fixture.lower_anthropic_cases) {
    it(c.name, () => {
      const spec = anthropic.lowerAnthropicToolUse(c.reply_json)
      assertLowerSpec(spec, c.expected_spec)
    })
  }
})

describe('lower_mcp matches golden vectors', () => {
  for (const c of fixture.lower_mcp_cases) {
    it(c.name, () => {
      const spec = mcp.lowerMcpToolsCall(c.reply_json)
      assertLowerSpec(spec, c.expected_spec)
    })
  }
})

describe('lower_gemini matches golden vectors', () => {
  for (const c of fixture.lower_gemini_cases) {
    it(c.name, () => {
      const spec = gemini.lowerGeminiFunctionCall(c.reply_json)
      assertLowerSpec(spec, c.expected_spec)
    })
  }
})

describe('error cases all reject', () => {
  for (const c of fixture.error_cases) {
    it(c.name, () => {
      const dispatch = () => {
        switch (c.provider) {
          case 'openai':
            return openai.lowerOpenaiToolCall(c.reply_json)
          case 'anthropic':
            return anthropic.lowerAnthropicToolUse(c.reply_json)
          case 'mcp':
            return mcp.lowerMcpToolsCall(c.reply_json)
          case 'gemini':
            return gemini.lowerGeminiFunctionCall(c.reply_json)
          default: {
            const _exhaust: never = c.provider
            throw new Error(`unknown provider ${_exhaust as string}`)
          }
        }
      }
      expect(dispatch).toThrow(ToolCallParseError)
    })
  }
})
