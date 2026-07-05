// Cross-language consent-surface parity fixture test
// (`MCP_BRIDGE_SDK_PLAN.md` conformance).
//
// Loads `crates/net/tests/cross_lang_mcp/consent_vectors.json` — the
// fixture the Rust source-of-truth verifier
// (`sdk/tests/consent_golden_vectors.rs`) validates — and asserts the Node
// consent bindings agree with the core.

import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { CapabilityId, ConsentPolicy, credentialRequiresConsent } from '../index'

const FIXTURE_PATH = join(
  __dirname,
  '..',
  '..',
  '..',
  'tests',
  'cross_lang_mcp',
  'consent_vectors.json',
)

const FIXTURE = JSON.parse(readFileSync(FIXTURE_PATH, 'utf8')) as {
  cap_id_canonicalize: { name: string; input: string; expected: string }[]
  cap_id_invalid: { name: string; input: string }[]
  credential_requires_consent: { name: string; status: string; expected: boolean }[]
  consent_decision: {
    name: string
    ops: { op: 'allow' | 'pin' | 'unpin'; cap_id: string }[]
    cap_id: string
    credential_status: string
    expected: string
  }[]
}

describe('consent golden vectors', () => {
  for (const c of FIXTURE.cap_id_canonicalize) {
    it(`canonicalize: ${c.name}`, () => {
      expect(CapabilityId.parse(c.input).display()).toBe(c.expected)
    })
  }

  for (const c of FIXTURE.cap_id_invalid) {
    it(`invalid: ${c.name}`, () => {
      expect(() => CapabilityId.parse(c.input)).toThrow()
    })
  }

  for (const c of FIXTURE.credential_requires_consent) {
    it(`requiresConsent: ${c.name}`, () => {
      expect(credentialRequiresConsent(c.status)).toBe(c.expected)
    })
  }

  for (const c of FIXTURE.consent_decision) {
    it(`decision: ${c.name}`, () => {
      const policy = new ConsentPolicy()
      for (const op of c.ops) {
        if (op.op === 'allow') policy.allow(op.cap_id)
        else if (op.op === 'pin') policy.pin(op.cap_id)
        else policy.unpin(op.cap_id)
      }
      expect(policy.decide(c.cap_id, c.credential_status)).toBe(c.expected)
    })
  }
})
