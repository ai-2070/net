/**
 * OSDK-L X1/N5 — Node's consumer of the shared org error vocabulary.
 *
 * Loads `tests/cross_lang_org/error_vectors.json` — the SAME fixture Rust
 * generates and consumes — and asserts this binding's classifier recovers the
 * identical domain, kind, and local/remote verdict.
 *
 * Imports from `../errors`, which is native-free, so this runs with no
 * compiled cdylib at all — following `abi_stability.test.ts`. A vocabulary
 * rename therefore fails here immediately, not after a rebuild.
 */

import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import {
  classifyError,
  classifyOrgError,
  OrgAdmissionDeniedError,
  OrgCredentialsError,
  OrgDiscoveryError,
  OrgError,
  OrgUnclassifiedError,
} from '../errors'

interface Vector {
  wire: string
  domain: string
  kind: string
  is_local: boolean
}
interface Unclassified {
  wire: string
  expect_domain: string
  expect_is_local: boolean
}
interface Fixture {
  version: number
  prefix: string
  domains: { token: string; is_local: boolean }[]
  vectors: Vector[]
  unclassified_cases: Unclassified[]
}

const fixture: Fixture = JSON.parse(
  readFileSync(
    join(__dirname, '..', '..', '..', 'tests', 'cross_lang_org', 'error_vectors.json'),
    'utf8',
  ),
)

describe('org error vocabulary (cross-language fixture)', () => {
  it('loads a fixture with the expected shape', () => {
    expect(fixture.version).toBe(1)
    expect(fixture.prefix).toBe('org:')
    expect(fixture.vectors.length).toBeGreaterThan(0)
    expect(fixture.unclassified_cases.length).toBeGreaterThan(0)
  })

  it('recovers the declared domain and kind from every vector', () => {
    for (const v of fixture.vectors) {
      const classified = classifyOrgError(new Error(v.wire))
      expect(classified, v.wire).toBeInstanceOf(OrgError)
      const err = classified as OrgError
      expect(err.domain, v.wire).toBe(v.domain)
      expect(err.kind, v.wire).toBe(v.kind)
      expect(err.isLocal, v.wire).toBe(v.is_local)
    }
  })

  it('maps each domain onto its own class', () => {
    const byDomain = (d: string) => fixture.vectors.find((v) => v.domain === d)!.wire

    expect(classifyOrgError(new Error(byDomain('credentials')))).toBeInstanceOf(
      OrgCredentialsError,
    )
    expect(classifyOrgError(new Error(byDomain('discovery')))).toBeInstanceOf(OrgDiscoveryError)
    expect(classifyOrgError(new Error(byDomain('admission_denied')))).toBeInstanceOf(
      OrgAdmissionDeniedError,
    )
  })

  /**
   * §D5a — the property a misclassification would destroy. An unparseable or
   * unknown-vocabulary string must NEVER become one of the four canonical
   * domains, because that asserts a request reached a provider and its
   * admission engine evaluated it.
   */
  it('never lets an unclassifiable string impersonate a canonical domain', () => {
    for (const c of fixture.unclassified_cases) {
      const classified = classifyOrgError(new Error(c.wire))
      if (!(classified instanceof OrgError)) {
        // A non-`org:` string is passed through untouched, which is also not an
        // impersonation.
        expect(c.wire.startsWith('org:'), c.wire).toBe(false)
        continue
      }
      expect(classified, c.wire).toBeInstanceOf(OrgUnclassifiedError)
      expect(classified.domain, c.wire).toBe(c.expect_domain)
      expect(classified.isLocal, c.wire).toBe(c.expect_is_local)
    }
  })

  it('agrees with the fixture on which domains are local', () => {
    for (const d of fixture.domains) {
      const sample = fixture.vectors.find((v) => v.domain === d.token)
      if (!sample) continue // `unknown` has no positive vector by construction
      const err = classifyOrgError(new Error(sample.wire)) as OrgError
      expect(err.isLocal, d.token).toBe(d.is_local)
    }
  })

  /**
   * §6 — binding-local lifecycle errors must read as LOCAL, never as the
   * `unknown`/vocabulary-mismatch class (whose `isLocal` is false, implying the
   * request may have reached a provider). A closed client rides the local
   * `credentials` domain (like `already_consumed`); a serve-registration failure
   * is a plain non-`org:` error (like provisioning), passed through untouched.
   */
  it('classifies binding-local lifecycle errors as local, not unclassified', () => {
    const closed = classifyOrgError(
      new Error('org:credentials:closed: this OrgClient has been closed'),
    )
    expect(closed).toBeInstanceOf(OrgCredentialsError)
    expect(closed).not.toBeInstanceOf(OrgUnclassifiedError)
    expect((closed as OrgError).kind).toBe('closed')
    expect((closed as OrgError).isLocal).toBe(true)

    // A serve-registration failure is not an org call-taxonomy event: a plain
    // error, returned untouched rather than coerced into `unknown`.
    const serveErr = new Error('org serve registration failed: already serving')
    expect(classifyOrgError(serveErr)).toBe(serveErr)
  })

  /**
   * A remote denial exposes its coarse bucket and nothing finer — asserted on
   * the fixture so this binding cannot quietly start inferring a richer reason.
   */
  it('exposes only the coarse bucket on an admission denial', () => {
    const denials = fixture.vectors.filter((v) => v.domain === 'admission_denied')
    expect(denials).toHaveLength(3)
    for (const v of denials) {
      const err = classifyOrgError(new Error(v.wire)) as OrgAdmissionDeniedError
      expect(err.reason).toBe(v.kind)
      expect(['denied', 'not_supported', 'unavailable']).toContain(err.reason)
      // org:<domain>:<bucket> — no trailing detail.
      expect(v.wire.split(':')).toHaveLength(3)
    }
  })

  it('passes through errors that are not org errors', () => {
    const other = new Error('nrpc:timeout: elapsed_ms=5000')
    expect(classifyOrgError(other)).toBe(other)
  })

  /**
   * N5 — the GENERIC classifier routes org errors too, so a caller doing
   * `classifyError(e)` at a shared catch site gets a typed org error without
   * having to know the org module exists.
   */
  it('is reachable through the generic classifyError', () => {
    for (const v of fixture.vectors) {
      const classified = classifyError(new Error(v.wire))
      expect(classified, v.wire).toBeInstanceOf(OrgError)
      expect((classified as OrgError).domain, v.wire).toBe(v.domain)
    }
    // And it does not disturb the vocabularies that were already there.
    const rpc = classifyError(new Error('nrpc:timeout: elapsed_ms=5000'))
    expect(rpc).not.toBeInstanceOf(OrgError)
  })
})
