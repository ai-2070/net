// Cross-language MCP bridge helper-parity fixture test
// (`MCP_BRIDGE_SDK_PLAN.md` P1-P3 conformance).
//
// Loads `crates/net/tests/cross_lang_mcp/helper_vectors.json` — the
// canonical fixture the Rust source-of-truth verifier
// (`adapters/mcp/tests/helper_golden_vectors.rs`) validates — and asserts
// the Node classifyMcpServer / lowerMcpTool bindings produce the same
// results. A failure here means the Node binding drifted from the core.

import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { classifyMcpServer, lowerMcpTool } from '../index'

const FIXTURE_PATH = join(
  __dirname,
  '..',
  '..',
  '..',
  'tests',
  'cross_lang_mcp',
  'helper_vectors.json',
)

interface ClassifyCase {
  name: string
  program: string
  args: string[]
  envs: Record<string, string>
  credential_override: string | null
  force: boolean
  expected_status: string
}

interface LowerCase {
  name: string
  tool: unknown
  server_version: string
  credential_status: string
  substitutability: string
  expected: unknown
}

const FIXTURE = JSON.parse(readFileSync(FIXTURE_PATH, 'utf8')) as {
  classify: ClassifyCase[]
  lower: LowerCase[]
}

// Reshape a lowered DTO into the fixture's comparison shape: descriptor is a
// JSON string here (the napi convention), and inside it input_schema /
// output_schema are JSON strings — parse them all so the comparison is by
// value. Field names map from camelCase (napi) to the fixture's snake_case.
function normalize(result: {
  toolId: string
  mcpName: string
  descriptor: string
  bridgeMetadata: Record<string, string>
}): unknown {
  const desc = JSON.parse(result.descriptor) as Record<string, unknown>
  const normDesc: Record<string, unknown> = { ...desc }
  normDesc.input_schema_object = desc.input_schema
    ? JSON.parse(desc.input_schema as string)
    : null
  normDesc.output_schema_object = desc.output_schema
    ? JSON.parse(desc.output_schema as string)
    : null
  delete normDesc.input_schema
  delete normDesc.output_schema
  return {
    tool_id: result.toolId,
    mcp_name: result.mcpName,
    bridge_metadata: result.bridgeMetadata,
    descriptor: normDesc,
  }
}

// napi-rs exports are `undefined` when the backing `mcp` Cargo feature isn't
// compiled. Skip (not fail opaquely with "not a function") when that's the
// case, so a slimmed build variant reads as skipped rather than broken.
const mcpBindingAvailable =
  typeof classifyMcpServer === 'function' && typeof lowerMcpTool === 'function'

describe.skipIf(!mcpBindingAvailable)('MCP helper golden vectors', () => {
  for (const c of FIXTURE.classify) {
    it(`classify: ${c.name}`, () => {
      const envs = Object.entries(c.envs).map(([key, value]) => ({ key, value }))
      const got = classifyMcpServer(
        c.program,
        c.args,
        envs,
        c.credential_override ?? undefined,
        c.force,
      )
      expect(got).toBe(c.expected_status)
    })
  }

  for (const c of FIXTURE.lower) {
    it(`lower: ${c.name}`, () => {
      const lowered = lowerMcpTool(
        JSON.stringify(c.tool),
        c.server_version,
        c.credential_status,
        c.substitutability,
      )
      expect(normalize(lowered)).toEqual(c.expected)
    })
  }
})
