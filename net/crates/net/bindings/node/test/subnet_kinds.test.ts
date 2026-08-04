// SSDK §7.3 — the Node consumer of the Rust-generated stable-kind
// fixture. Deliberately NATIVE-FREE (imports only ./errors), so a kind
// rename in Rust fails this suite before the cdylib is rebuilt.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'

import { describe, expect, it } from 'vitest'

import { classifyError, classifySubnetError, SubnetProvisionError } from '../errors'

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
})
