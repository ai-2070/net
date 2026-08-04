// SSDK §7.3 — the Node consumer of the Rust-generated stable-kind
// fixture. Deliberately NATIVE-FREE (imports only ./errors), so a kind
// rename in Rust fails this suite before the cdylib is rebuilt.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'

import { describe, expect, it } from 'vitest'

import {
  classifyError,
  classifySubnetError,
  parseSubnetKind,
  SubnetProvisionError,
} from '../errors'

type Fixture = {
  version: number
  prefix: string
  auth_kinds: string[]
  local_kinds: string[]
  fact_kinds: string[]
  access: string[]
}

const here = dirname(fileURLToPath(import.meta.url))
const fixturePath = resolve(here, '..', '..', '..', 'tests', 'cross_lang_subnet', 'stable_kinds.json')
const fixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as Fixture

describe('subnet stable-kind fixture (native-free)', () => {
  it('has the frozen shape', () => {
    expect(fixture.version).toBe(1)
    expect(fixture.prefix).toBe('subnet:')
    expect(fixture.auth_kinds.length).toBeGreaterThan(0)
    expect(fixture.local_kinds).toContain('unknown_export_name')
    expect(fixture.fact_kinds).toEqual([
      'descriptor',
      'gateway_advertisement',
      'export_policy',
      'revocation_floor',
    ])
    expect(fixture.access).toEqual(['sameOrg', 'granted'])
  })

  it('classifies every pinned kind, preserving the token verbatim', () => {
    for (const kind of [...fixture.auth_kinds, ...fixture.local_kinds]) {
      const classified = classifySubnetError(new Error(`subnet:${kind}`))
      expect(classified, kind).toBeInstanceOf(SubnetProvisionError)
      expect((classified as SubnetProvisionError).kind).toBe(kind)
    }
  })

  it('routes through the general classifier too', () => {
    const classified = classifyError(new Error('subnet:invalid_format'))
    expect(classified).toBeInstanceOf(SubnetProvisionError)
    expect((classified as SubnetProvisionError).kind).toBe('invalid_format')
  })

  it('never claims a non-subnet error or an empty kind', () => {
    const plain = new Error('org:credentials:signature_invalid')
    expect(classifySubnetError(plain)).toBe(plain)
    const empty = new Error('subnet:')
    expect(classifySubnetError(empty)).toBe(empty)
    const unrelated = new Error('subnet-exported serve registration failed: x')
    expect(classifySubnetError(unrelated)).toBe(unrelated)
  })

  it('an unknown kind still classifies, kind carried as data', () => {
    const classified = classifySubnetError(new Error('subnet:kind_from_the_future'))
    expect(classified).toBeInstanceOf(SubnetProvisionError)
    expect((classified as SubnetProvisionError).kind).toBe('kind_from_the_future')
  })

  // -------------------------------------------------------------------
  // review-10 P2-1 — the envelope is SCANNED for, not required at
  // position 0. Serve registration wraps it in provider-setup prose, and
  // the bare-prefix parse these tests used to encode returned the raw
  // Error for exactly the failure applications hit most.
  // -------------------------------------------------------------------

  it('classifies an EMBEDDED envelope, not just a leading one', () => {
    const wrapped = new Error(
      'subnet-exported serve registration failed: invalid protected registration: ' +
        'subnet:unknown_export_name: no configured subnet export named "no-such"',
    )
    const classified = classifySubnetError(wrapped)
    expect(classified).toBeInstanceOf(SubnetProvisionError)
    expect((classified as SubnetProvisionError).kind).toBe('unknown_export_name')
    // The full message is preserved — the prose is the operator context.
    expect((classified as Error).message).toContain('registration failed')
  })

  it('parseSubnetKind terminates the token at a colon OR whitespace', () => {
    expect(parseSubnetKind(new Error('subnet:path_too_deep: five levels'))).toBe(
      'path_too_deep',
    )
    expect(parseSubnetKind(new Error('serve failed: subnet:invalid_access mode 7'))).toBe(
      'invalid_access',
    )
    expect(parseSubnetKind(new Error('subnet:revoked'))).toBe('revoked')
    expect(parseSubnetKind(new Error('nothing here'))).toBeUndefined()
    expect(parseSubnetKind(new Error('subnet: '))).toBeUndefined()
  })

  it('the general classifier also sees an embedded envelope', () => {
    const classified = classifyError(
      new Error('subnet-exported serve registration failed: subnet:duplicate_export_name'),
    )
    expect(classified).toBeInstanceOf(SubnetProvisionError)
    expect((classified as SubnetProvisionError).kind).toBe('duplicate_export_name')
  })

  it('a leading taxonomy still wins over an embedded subnet token', () => {
    // An org error whose detail clause happens to mention the subnet
    // envelope must stay an org error — subnet classification runs last.
    const classified = classifyError(
      new Error('org:credentials:signature_invalid: while loading subnet:invalid_format'),
    )
    expect(classified).not.toBeInstanceOf(SubnetProvisionError)
  })
})
