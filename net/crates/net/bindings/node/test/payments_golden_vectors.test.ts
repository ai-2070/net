// Cross-language payments golden vectors (`PAYMENTS_IMPLEMENTATION_PLAN.md`
// Workstream 1).
//
// Loads `crates/net/tests/cross_lang_payments/payment_vectors.json` — the
// fixture the Rust source-of-truth verifier
// (`payments/tests/payments_golden_vectors.rs`) validates — and asserts the
// canonical-encoding regime holds byte-identically from JS:
//
//   - canonical form: one JSON object, all keys sorted bytewise, compact
//     separators, raw UTF-8, integers only (floats are rejected)
//   - signed payload = canonical form with the top-level `signature`
//     key absent; ed25519 over those exact bytes
//   - x402 documents ride as base64 of their preserved original bytes —
//     the captured v2 fixtures must survive untouched
//
// CAIP / amount / decimals grammar tables are enforced by the Rust
// verifier (the grammar lives in the Rust core; no payments binding
// exists yet — logic never lives in bindings, so nothing is re-implemented
// here).

import { createPublicKey, verify as cryptoVerify } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

const FIXTURE_DIR = join(__dirname, '..', '..', '..', 'tests', 'cross_lang_payments')

const FIXTURE = JSON.parse(
  readFileSync(join(FIXTURE_DIR, 'payment_vectors.json'), 'utf8'),
) as {
  envelopes: {
    name: string
    object: string
    canonical: string
    signed_payload: string | null
    signer_hex: string | null
    signature_hex: string | null
  }[]
  x402_byte_preservation: {
    name: string
    file: string
    base64: string
    embedded_in: string | null
    envelope_field: string | null
  }[]
}

// The payments canonical writer, per the regime pinned by the fixture.
// Keys are ASCII in every envelope schema, so JS UTF-16 sort order and
// bytewise UTF-8 order agree; the Rust verifier is the tie-breaker if a
// future vector ever introduces non-ASCII keys.
function canonicalize(value: unknown): string {
  if (value === null) return 'null'
  switch (typeof value) {
    case 'boolean':
      return value ? 'true' : 'false'
    case 'number':
      if (!Number.isSafeInteger(value)) {
        throw new Error(`non-integer or unsafe number in envelope: ${value}`)
      }
      return String(value)
    case 'string':
      return JSON.stringify(value)
    default:
      break
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalize).join(',')}]`
  }
  const obj = value as Record<string, unknown>
  const body = Object.keys(obj)
    .sort()
    .map((k) => `${JSON.stringify(k)}:${canonicalize(obj[k])}`)
    .join(',')
  return `{${body}}`
}

// Raw 32-byte ed25519 public key -> Node KeyObject (DER SPKI wrapping).
const SPKI_ED25519_PREFIX = Buffer.from('302a300506032b6570032100', 'hex')
function verifyEd25519(pubHex: string, sigHex: string, message: Buffer): boolean {
  const key = createPublicKey({
    key: Buffer.concat([SPKI_ED25519_PREFIX, Buffer.from(pubHex, 'hex')]),
    format: 'der',
    type: 'spki',
  })
  return cryptoVerify(null, message, key, Buffer.from(sigHex, 'hex'))
}

describe('payments golden vectors', () => {
  for (const env of FIXTURE.envelopes) {
    it(`${env.name}: canonical emission is a fixed point`, () => {
      const parsed = JSON.parse(env.canonical) as Record<string, unknown>
      expect(canonicalize(parsed)).toBe(env.canonical)
    })

    if (env.signature_hex !== null) {
      it(`${env.name}: signed payload derives and the signature verifies`, () => {
        const parsed = JSON.parse(env.canonical) as Record<string, unknown>
        delete parsed.signature
        const payload = canonicalize(parsed)
        expect(payload).toBe(env.signed_payload)
        expect(
          verifyEd25519(env.signer_hex!, env.signature_hex!, Buffer.from(payload, 'utf8')),
        ).toBe(true)
        // Tampered payload must not verify.
        expect(
          verifyEd25519(
            env.signer_hex!,
            env.signature_hex!,
            Buffer.from(`${payload} `, 'utf8'),
          ),
        ).toBe(false)
      })
    }
  }

  for (const p of FIXTURE.x402_byte_preservation) {
    it(`${p.name}: captured x402 fixture survives untouched`, () => {
      const fileBytes = readFileSync(join(FIXTURE_DIR, ...p.file.split('/')))
      expect(Buffer.from(p.base64, 'base64').equals(fileBytes)).toBe(true)
      // Base64 round-trip is exact.
      expect(fileBytes.toString('base64')).toBe(p.base64)

      if (p.embedded_in !== null && p.envelope_field !== null) {
        const env = FIXTURE.envelopes.find((e) => e.name === p.embedded_in)
        expect(env, `envelope ${p.embedded_in} exists`).toBeDefined()
        const parsed = JSON.parse(env!.canonical) as Record<string, unknown>
        expect(parsed[p.envelope_field]).toBe(p.base64)
      }
    })
  }

  it('floats are rejected by the canonical writer', () => {
    expect(() => canonicalize({ price: 1.5 })).toThrow()
  })
})
