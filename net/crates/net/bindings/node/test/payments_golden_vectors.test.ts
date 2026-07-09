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
  failure_schematic_vectors: {
    tag: string
    header_name: string
    cases: {
      name: string
      header_utf8?: string
      header_base64?: string
      accepted: boolean
      expect?: {
        stage: string
        reason: string
        retryable: boolean
        funds_moved: string
        prior_payment: string
        recovery: {
          class: string
          actor: string
          safe_to_retry: boolean
          safe_to_requote: boolean
        }
      }
      expect_extra_keys?: string[]
    }[]
  }
}

const FAILURE = FIXTURE.failure_schematic_vectors

function failureHeaderBytes(c: (typeof FAILURE.cases)[number]): Buffer {
  return c.header_utf8 !== undefined
    ? Buffer.from(c.header_utf8, 'utf8')
    : Buffer.from(c.header_base64!, 'base64')
}

// The required-field shape a `FailureSchematic` deserializes into (its
// non-optional fields). `quote_id` / `tool_id` / `recovery.next_action` are
// `Option<String>` and extra keys ride `#[serde(flatten)]`.
const REQUIRED_STR = ['object', 'code', 'stage', 'reason', 'message', 'funds_moved', 'prior_payment']
const REQUIRED_BOOL = ['retryable', 'handler_executed']

// An `Option<String>` field: absent or JSON `null` deserializes to `None`; any
// other present type (a number, bool, array, object) fails the typed serde
// deserialize, so a *present* optional is still type-checked.
function optionalStrOk(obj: Record<string, unknown>, key: string): boolean {
  const v = obj[key]
  return v === undefined || v === null || typeof v === 'string'
}

// Presence + JSON type of every required field, plus the type of every present
// optional — the structural half of `from_header_bytes` (a full typed serde
// deserialize). A tag-only, mistyped-required, or mistyped-optional object does
// not deserialize, so it is not accepted.
function hasSchematicShape(obj: Record<string, unknown>): boolean {
  if (!REQUIRED_STR.every((k) => typeof obj[k] === 'string')) return false
  if (!REQUIRED_BOOL.every((k) => typeof obj[k] === 'boolean')) return false
  if (!['quote_id', 'tool_id'].every((k) => optionalStrOk(obj, k))) return false
  const rec = obj.recovery
  if (typeof rec !== 'object' || rec === null || Array.isArray(rec)) return false
  const r = rec as Record<string, unknown>
  return (
    typeof r.class === 'string' &&
    typeof r.actor === 'string' &&
    typeof r.safe_to_retry === 'boolean' &&
    typeof r.safe_to_requote === 'boolean' &&
    optionalStrOk(r, 'next_action')
  )
}

// Mirror `FailureSchematic::from_header_bytes`: decode the header bytes as
// strict UTF-8 JSON (JS `JSON.parse` already rejects Infinity/NaN) and accept
// iff the value deserializes to the full schematic shape AND carries the tag —
// else `null` (fall back to the human error body).
function tolerantParse(raw: Buffer): Record<string, unknown> | null {
  let text: string
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(raw)
  } catch {
    return null
  }
  let obj: unknown
  try {
    obj = JSON.parse(text)
  } catch {
    return null
  }
  if (
    typeof obj === 'object' &&
    obj !== null &&
    !Array.isArray(obj) &&
    (obj as Record<string, unknown>).object === FAILURE.tag &&
    hasSchematicShape(obj as Record<string, unknown>)
  ) {
    return obj as Record<string, unknown>
  }
  return null
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

  for (const c of FAILURE.cases) {
    it(`${c.name}: failure-schematic tolerance verdict + field access`, () => {
      const parsed = tolerantParse(failureHeaderBytes(c))
      expect(parsed !== null).toBe(c.accepted)
      if (parsed === null) return
      expect(parsed.object).toBe(FAILURE.tag)
      if (c.expect) {
        expect(parsed.stage).toBe(c.expect.stage)
        expect(parsed.reason).toBe(c.expect.reason)
        expect(parsed.retryable).toBe(c.expect.retryable)
        expect(parsed.funds_moved).toBe(c.expect.funds_moved)
        expect(parsed.prior_payment).toBe(c.expect.prior_payment)
        const rec = parsed.recovery as Record<string, unknown>
        expect(rec.class).toBe(c.expect.recovery.class)
        expect(rec.actor).toBe(c.expect.recovery.actor)
        expect(rec.safe_to_retry).toBe(c.expect.recovery.safe_to_retry)
        expect(rec.safe_to_requote).toBe(c.expect.recovery.safe_to_requote)
      }
      for (const k of c.expect_extra_keys ?? []) {
        expect(Object.prototype.hasOwnProperty.call(parsed, k)).toBe(true)
      }
    })
  }
})
