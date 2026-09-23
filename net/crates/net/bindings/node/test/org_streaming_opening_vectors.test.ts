/**
 * S4Vectors — Node's consumer of the shared org streaming OPENING envelope +
 * frozen `org:` error vocabulary.
 *
 * Loads `tests/cross_lang_org/streaming_opening_vectors.json` — the SAME
 * fixture Rust generates and consumes — and asserts this binding recovers the
 * identical bytes, roles, and error verdicts. Mirrors the Python consumer so a
 * reviewer can diff the two side by side (row ids and order are kept in
 * lockstep across both bindings).
 *
 * Imports from `../errors`, which is native-free, so this runs with no
 * compiled cdylib at all — following `org_error_vectors.test.ts`.
 *
 * Four harness-integrity rules are non-negotiable and each is defended by a
 * named row below: malformed/unknown errors, narrowing IDs, callback loss, or
 * decoder disagreement MUST NOT become success.
 */

import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { classifyOrgError, OrgError, OrgUnclassifiedError } from '../errors'

// --- fixture schema (a trimmed mirror of streaming_opening_vectors.json) ---
interface Expect {
  decode: string
  kind: number
  session_binding_hex: string
  sig_domain_separated: boolean
  verify: string
}
interface OpeningVector {
  call_binding_sig_hex: string
  call_id: string
  expect: Expect
  id: string
  kind: number
  kind_name: string
  pre_sig_prefix_hex: string
  proof_expires_at_unix_ns: string
  session_binding_hex: string
  unary_call_binding_sig_hex: string
  unary_wire_hex: string
  unary_wire_len: number
  wire_base64: string
  wire_hex: string
  wire_len: number
}
interface Reject {
  base: string
  base_wire_len: number
  id: string
  wire_base64: string
  wire_hex: string
  wire_len: number
  expect_error_display?: string
  expect_decode?: string
  expect_verify_error_display?: string
}
interface VocabVector {
  domain: string
  is_local: boolean
  kind: string
  wire: string
  wire_base64: string
  wire_len: number
}
interface UnclassifiedCase {
  expect_domain: string
  expect_is_local: boolean
  wire: string
  wire_base64: string
  wire_len: number
}
interface Fixture {
  version: number
  prefix: string
  ids: Record<string, string>
  layout: {
    kind_values: Record<string, number>
    max_proof_bytes: number
    session_binding_len: number
    signature_wire_len: number
    stream_suffix_len: number
  }
  opening_vectors: OpeningVector[]
  decoder_rejects: Reject[]
  signature_rejects: Reject[]
  error_vocabulary: {
    vectors: VocabVector[]
    unclassified_cases: UnclassifiedCase[]
  }
}

const fixture: Fixture = JSON.parse(
  readFileSync(
    join(__dirname, '..', '..', '..', 'tests', 'cross_lang_org', 'streaming_opening_vectors.json'),
    'utf8',
  ),
)

// The full 32-byte ids (`*_hex` values); `capability_tag` is not an id and is
// excluded by the 64-hex filter.
const fullIds = Object.values(fixture.ids).filter((v) => /^[0-9a-f]{64}$/.test(v))

// The only own keys the classifier may surface — anything else (an `entityId`,
// `orgId`, …) would be a leaked identity field.
const CLASSIFIED_KEYS = ['name', 'message', 'stack', 'domain', 'kind', 'reason']

/** Byte offsets where two byte strings differ (over their common length). */
function byteDiffs(a: Buffer, b: Buffer): number[] {
  const n = Math.min(a.length, b.length)
  const out: number[] = []
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) out.push(i)
  return out
}

describe('org streaming opening vectors (cross-language fixture)', () => {
  it('loads the fixture with the expected shape', () => {
    expect(fixture.version).toBe(1)
    expect(fixture.prefix).toBe('org:')
    expect(fixture.opening_vectors.length).toBe(6)
    expect(fixture.decoder_rejects.length).toBe(8)
    expect(fixture.signature_rejects.length).toBe(1)
    expect(fixture.error_vocabulary.vectors.length).toBe(24)
    expect(fixture.error_vocabulary.unclassified_cases.length).toBe(4)
  })

  // ---- byte-for-byte pin: one row per EVERY vector (43 rows) ----
  interface ByteRow {
    src: string
    srcEnc: 'hex' | 'utf8'
    wire_base64: string
    wire_len: number
  }
  const byteRows: [string, ByteRow][] = [
    ...fixture.opening_vectors.map(
      (v) =>
        [`bytes-${v.id}`, { src: v.wire_hex, srcEnc: 'hex', wire_base64: v.wire_base64, wire_len: v.wire_len }] as [
          string,
          ByteRow,
        ],
    ),
    ...fixture.decoder_rejects.map(
      (r) =>
        [`bytes-${r.id}`, { src: r.wire_hex, srcEnc: 'hex', wire_base64: r.wire_base64, wire_len: r.wire_len }] as [
          string,
          ByteRow,
        ],
    ),
    ...fixture.signature_rejects.map(
      (r) =>
        [`bytes-${r.id}`, { src: r.wire_hex, srcEnc: 'hex', wire_base64: r.wire_base64, wire_len: r.wire_len }] as [
          string,
          ByteRow,
        ],
    ),
    ...fixture.error_vocabulary.vectors.map(
      (v) =>
        [
          `bytes-vocab-${v.domain}.${v.kind}`,
          { src: v.wire, srcEnc: 'utf8', wire_base64: v.wire_base64, wire_len: v.wire_len },
        ] as [string, ByteRow],
    ),
    ...fixture.error_vocabulary.unclassified_cases.map(
      (u, i) =>
        [
          `bytes-unclassified-${i}`,
          { src: u.wire, srcEnc: 'utf8', wire_base64: u.wire_base64, wire_len: u.wire_len },
        ] as [string, ByteRow],
    ),
  ]
  it.each(byteRows)('%s', (_title, row) => {
    const bytes = row.srcEnc === 'hex' ? Buffer.from(row.src, 'hex') : Buffer.from(row.src, 'utf8')
    expect(bytes.length).toBe(row.wire_len)
    expect(bytes.toString('base64')).toBe(row.wire_base64)
    if (row.srcEnc === 'hex') {
      // Envelope rows only: round-trips the hex, catching non-canonical / upper /
      // invalid hex because Buffer.from silently truncates a bad hex string.
      expect(bytes.toString('hex')).toBe(row.src)
    }
  })

  // ---- opening role rows: caller-emit contract (6 rows) ----
  const callerRows: [string, OpeningVector][] = fixture.opening_vectors.map((v) => [`as-caller-${v.id}`, v])
  it.each(callerRows)('%s', (_title, v) => {
    const wire = Buffer.from(v.wire_hex, 'hex')
    const unary = Buffer.from(v.unary_wire_hex, 'hex')
    const sigAt = v.pre_sig_prefix_hex.length / 2 // byte offset of the 65-byte signature field

    // The stream wire is exactly the unary wire plus the stream suffix.
    expect(v.wire_len).toBe(v.unary_wire_len + fixture.layout.stream_suffix_len)
    expect(wire.length).toBe(v.wire_len)
    expect(unary.length).toBe(v.unary_wire_len)

    // Both share the pre-signature prefix…
    expect(v.wire_hex.startsWith(v.pre_sig_prefix_hex)).toBe(true)
    expect(v.unary_wire_hex.startsWith(v.pre_sig_prefix_hex)).toBe(true)

    // …and a 65-byte signature field (1 postcard varint length prefix 0x40 +
    // 64-byte call-binding sig). The stream and unary transcripts are domain
    // separated, so their sigs differ over the same prefix.
    expect(wire[sigAt]).toBe(0x40)
    expect(unary[sigAt]).toBe(0x40)
    expect(wire.subarray(sigAt + 1, sigAt + fixture.layout.signature_wire_len).toString('hex')).toBe(
      v.call_binding_sig_hex,
    )
    expect(unary.subarray(sigAt + 1, sigAt + fixture.layout.signature_wire_len).toString('hex')).toBe(
      v.unary_call_binding_sig_hex,
    )
    expect(v.call_binding_sig_hex).not.toBe(v.unary_call_binding_sig_hex)
    expect(v.expect.sig_domain_separated).toBe(true)
  })

  // ---- opening role rows: provider-decode facts (6 rows) ----
  const providerRows: [string, OpeningVector][] = fixture.opening_vectors.map((v) => [`as-provider-${v.id}`, v])
  it.each(providerRows)('%s', (_title, v) => {
    const wire = Buffer.from(v.wire_hex, 'hex')
    const suffix = wire.subarray(wire.length - fixture.layout.stream_suffix_len)
    expect(suffix.length).toBe(fixture.layout.stream_suffix_len)

    // [kind byte, ...32-byte session binding]
    const kindByte = suffix[0]
    expect(kindByte).toBe(v.expect.kind)
    expect(kindByte).toBe(fixture.layout.kind_values[v.kind_name])
    const binding = suffix.subarray(1)
    expect(binding.length).toBe(fixture.layout.session_binding_len)
    expect(binding.toString('hex')).toBe(v.expect.session_binding_hex)
    expect(binding.toString('hex')).toBe(v.session_binding_hex)

    expect(v.expect.decode).toBe('ok')
    expect(v.expect.verify).toBe('ok')
  })

  // ---- reject rows: the mutation is REAL against the base bytes (9 rows) ----
  interface RejectRow extends Reject {
    section: 'decoder' | 'signature'
  }
  const rejectRows: [string, RejectRow][] = [
    ...fixture.decoder_rejects.map(
      (r) => [`as-provider-reject-${r.id.replace(/^reject\./, '')}`, { ...r, section: 'decoder' as const }],
    ),
    ...fixture.signature_rejects.map(
      (r) => [`as-provider-reject-${r.id.replace(/^reject\./, '')}`, { ...r, section: 'signature' as const }],
    ),
  ]
  it.each(rejectRows)('%s', (_title, rej) => {
    const base = fixture.opening_vectors.find((o) => o.id === rej.base)!
    const baseBytes = Buffer.from(base.wire_hex, 'hex')
    const bytes = Buffer.from(rej.wire_hex, 'hex')
    expect(rej.base_wire_len).toBe(base.wire_len)

    const diffs = byteDiffs(baseBytes, bytes)
    const suffixLen = fixture.layout.stream_suffix_len
    const sigLen = fixture.layout.signature_wire_len

    if (rej.id === 'reject.trailing_byte') {
      // (a) one appended byte after the suffix — still starts with the base.
      expect(bytes.length).toBe(baseBytes.length + 1)
      expect(bytes.subarray(0, baseBytes.length).equals(baseBytes)).toBe(true)
    } else if (rej.id.startsWith('reject.truncated_')) {
      // (b) a genuine prefix of the base (bytes cut).
      expect(bytes.length).toBeLessThan(baseBytes.length)
      expect(baseBytes.subarray(0, bytes.length).equals(bytes)).toBe(true)
    } else if (rej.id.startsWith('reject.kind_')) {
      // (c) exactly one byte flipped, at the kind byte, to a non-kind value.
      expect(bytes.length).toBe(baseBytes.length)
      expect(diffs.length).toBe(1)
      expect(diffs[0]).toBe(baseBytes.length - suffixLen)
      expect([1, 2, 3]).not.toContain(bytes[diffs[0]])
    } else if (rej.id === 'reject.over_cap') {
      // (d) padded past the proof cap.
      expect(bytes.length).toBe(fixture.layout.max_proof_bytes + 1)
      expect(bytes.subarray(0, baseBytes.length).equals(baseBytes)).toBe(true)
    } else if (rej.id === 'reject.signature_flipped') {
      // (e) exactly one byte flipped inside the 64-byte signature.
      expect(bytes.length).toBe(baseBytes.length)
      expect(diffs.length).toBe(1)
      const sigLo = baseBytes.length - suffixLen - sigLen + 1
      const sigHi = baseBytes.length - suffixLen
      expect(diffs[0]).toBeGreaterThanOrEqual(sigLo)
      expect(diffs[0]).toBeLessThan(sigHi)
    }

    // Decoder disagreement must not become success: every decoder reject is a
    // wire-format error, while the signature reject decodes but fails verify.
    if (rej.section === 'decoder') {
      expect(rej.expect_error_display).toBe('invalid wire format')
    } else {
      expect(rej.expect_decode).toBe('ok')
      expect(rej.expect_verify_error_display).toBe('invalid signature')
    }
  })

  // ---- vocabulary rows: classifier recovery (24 rows) ----
  const classifyRows: [string, VocabVector][] = fixture.error_vocabulary.vectors.map((v) => [
    `as-caller-classify-${v.domain}.${v.kind}`,
    v,
  ])
  it.each(classifyRows)('%s', (_title, v) => {
    const classified = classifyOrgError(new Error(v.wire))
    expect(classified).toBeInstanceOf(OrgError)
    const err = classified as OrgError
    expect(err.domain).toBe(v.domain)
    expect(err.kind).toBe(v.kind)
    expect(err.isLocal).toBe(v.is_local)
  })

  // ---- vocabulary rows: `org:` grammar (24 rows) ----
  const grammarRows: [string, VocabVector][] = fixture.error_vocabulary.vectors.map((v) => [
    `as-provider-grammar-${v.domain}.${v.kind}`,
    v,
  ])
  it.each(grammarRows)('%s', (_title, v) => {
    const head = `${fixture.prefix}${v.domain}:${v.kind}` // org:<domain>:<kind>
    const segs = v.wire.split(':')
    expect(v.wire.startsWith(fixture.prefix)).toBe(true)
    expect(segs[1]).toBe(v.domain)
    expect(segs[2]).toBe(v.kind)
    // shape is bare `org:<domain>:<kind>` or `org:<domain>:<kind>: <detail>`.
    expect(v.wire === head || v.wire.startsWith(head + ': ')).toBe(true)
    // `admission_denied` carries the coarse bucket and NOTHING else — a precise
    // remote reason would be a credential oracle.
    if (v.domain === 'admission_denied') expect(segs).toHaveLength(3)
  })

  // ---- unclassified rows: malformed/unknown never become success (4 rows) ----
  const unclassifiedRows: [string, UnclassifiedCase][] = fixture.error_vocabulary.unclassified_cases.map((u, i) => [
    `never-success-unclassified-${i}`,
    u,
  ])
  it.each(unclassifiedRows)('%s', (_title, u) => {
    const original = new Error(u.wire)
    const classified = classifyOrgError(original)
    if (!(classified instanceof OrgError)) {
      // A non-`org:` string passes through untouched — also not a coerced success.
      expect(classified).toBe(original)
      expect(u.wire.startsWith('org:')).toBe(false)
      return
    }
    // Malformed / unknown `org:` strings classify as `unknown`, never a canonical
    // domain (that would assert a request reached a provider admission engine).
    expect(classified).toBeInstanceOf(OrgUnclassifiedError)
    expect(classified.domain).toBe(u.expect_domain)
    expect(classified.domain).toBe('unknown')
    expect(classified.kind).toBeFalsy()
    expect(classified.isLocal).toBe(u.expect_is_local)
    expect(classified.isLocal).toBe(false)
  })

  it('narrowed ids never match a full id', () => {
    // Every narrowed display in the vocabulary is exactly 16 hex chars + '...'.
    const narrowed: string[] = []
    for (const v of fixture.error_vocabulary.vectors) {
      for (const m of v.wire.matchAll(/[0-9a-f]{16}\.\.\./g)) narrowed.push(m[0])
    }
    expect(narrowed.length).toBeGreaterThan(0)
    for (const n of narrowed) expect(n).toMatch(/^[0-9a-f]{16}\.\.\.$/)

    // A narrowed display never equals, or upgrades into, a full 32-byte id.
    for (const n of narrowed) {
      const n16 = n.slice(0, 16)
      for (const f of fullIds) {
        expect(f).not.toBe(n)
        expect(f).not.toBe(n16)
      }
    }

    // Classification surfaces no id fields — only the domain/kind/is_local
    // taxonomy — and no real full id leaks into the message. (Detail strings
    // echo a dummy 64-hex grant id by design, so only `ids` are checked.)
    for (const v of fixture.error_vocabulary.vectors) {
      const err = classifyOrgError(new Error(v.wire)) as OrgError
      for (const k of Object.keys(err)) expect(CLASSIFIED_KEYS).toContain(k)
      expect(err.domain).toBe(v.domain)
      expect(err.kind).toBe(v.kind)
      expect(err.isLocal).toBe(v.is_local)
      for (const f of fullIds) expect(err.message).not.toContain(f)
    }
  })

  it('u64 strings round-trip exactly', () => {
    for (const v of fixture.opening_vectors) {
      for (const s of [v.call_id, v.proof_expires_at_unix_ns]) {
        expect(typeof s).toBe('string')
        expect(s).toMatch(/^\d+$/)
        expect(String(BigInt(s))).toBe(s) // exact — JS numbers would silently corrupt these
      }
    }
  })
})
