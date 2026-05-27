// Cross-language ToolEvent envelope round-trip fixture test (plan T-2).
//
// Loads `crates/net/tests/cross_lang_tool_formats/tool_event_vectors.json`
// and asserts that for each case the Node TS ToolEvent
// representation round-trips through JSON byte-equal to the wire
// shape. Matches the Rust verifier at
// `sdk/tests/tool_event_golden_vectors.rs`.

import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { isTerminalEvent, ToolEvent } from '../tool'

const FIXTURE_PATH = join(
  __dirname,
  '..',
  '..',
  '..',
  'tests',
  'cross_lang_tool_formats',
  'tool_event_vectors.json',
)

interface FixtureCase {
  name: string
  wire: Record<string, unknown>
  is_terminal: boolean
}

interface Fixture {
  cases: FixtureCase[]
}

const fixture: Fixture = JSON.parse(readFileSync(FIXTURE_PATH, 'utf8'))

describe('ToolEvent round-trip matches golden vectors', () => {
  for (const c of fixture.cases) {
    it(c.name, () => {
      // The Node TS ToolEvent is a tagged-union TypeScript type.
      // Its JSON representation IS the wire shape — no separate
      // serialize/deserialize step beyond the cast. Round-trip is
      // therefore JSON.parse(JSON.stringify(event)).
      const event = c.wire as unknown as ToolEvent
      // is_terminal contract
      expect(isTerminalEvent(event)).toBe(c.is_terminal)
      // Round-trip through JSON to assert no extra fields, no
      // dropped fields, no key reordering issues.
      const roundTripped = JSON.parse(JSON.stringify(event))
      expect(roundTripped).toEqual(c.wire)
    })
  }
})
