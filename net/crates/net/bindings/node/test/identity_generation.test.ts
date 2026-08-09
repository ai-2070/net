// Durable issuer state through the Node binding (decision 4b).
//
// `issuerGeneration` rides on every token and drives revocation, but
// the binding had no way to set it: `Identity` carried a keypair and a
// cache and nothing else, so every token it minted was generation
// zero. The field was visible on `parseToken` output and settable by
// nobody.
//
// The state encoding is core's, shared byte-for-byte with the Rust,
// Python and C surfaces — a file written by one has to be readable by
// the others.

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { Identity, parseToken, delegateToken } = binding

const CHANNEL = 'issuer/rotation'
const STATE_SIZE = 37

function issue(signer: any, subject: any): Buffer {
  return signer.issueToken(subject.entityId, ['publish'], CHANNEL, 3600, 0)
}

describe('issuer generation', () => {
  it('starts at zero and stamps every issued token', () => {
    const id = Identity.generate()
    const subject = Identity.generate()
    expect(id.issuerGeneration).toBe(0)
    expect(parseToken(issue(id, subject)).issuerGeneration).toBe(0)
  })

  it('rotates to a new identity, leaving the original alone', () => {
    const id = Identity.generate()
    const subject = Identity.generate()

    const rotated = id.atGeneration(3)
    expect(rotated.issuerGeneration).toBe(3)
    expect(id.issuerGeneration).toBe(0)
    expect(rotated.entityId).toEqual(id.entityId)

    expect(parseToken(issue(rotated, subject)).issuerGeneration).toBe(3)
    expect(parseToken(issue(id, subject)).issuerGeneration).toBe(0)
  })

  it('round-trips key and generation through state bytes', () => {
    const id = Identity.generate().atGeneration(6)
    const state = id.toStateBytes()
    expect(state.length).toBe(STATE_SIZE)
    // Layout is a cross-SDK contract, not an implementation detail.
    expect(state[0]).toBe(1)
    expect(state.subarray(33, 37)).toEqual(Buffer.from([6, 0, 0, 0]))

    const restored = Identity.fromStateBytes(state)
    expect(restored.issuerGeneration).toBe(6)
    expect(restored.entityId).toEqual(id.entityId)

    const subject = Identity.generate()
    expect(parseToken(issue(restored, subject)).issuerGeneration).toBe(6)
  })

  it('key-only restoration comes back at generation zero', () => {
    // The documented cost of `toBytes` / `fromBytes`. An issuer that
    // rotated to 4 and published floor 4 comes back here unable to
    // mint anything a verifier accepts.
    const id = Identity.generate().atGeneration(4)
    const seedOnly = Identity.fromBytes(id.toBytes())
    expect(seedOnly.entityId).toEqual(id.entityId)
    expect(seedOnly.issuerGeneration).toBe(0)
  })

  it('refuses to go backwards, and is idempotent at the same value', () => {
    const id = Identity.generate().atGeneration(5)
    expect(() => id.atGeneration(4)).toThrow(/generation_went_backwards/)
    expect(id.atGeneration(5).issuerGeneration).toBe(5)
    expect(id.atGeneration(6).issuerGeneration).toBe(6)
  })

  it('is usable at the ceiling, including across a restart', () => {
    // This used to assert that re-applying `0xffffffff` at the ceiling
    // threw `generation_exhausted`, contradicting the idempotence the
    // line above pins at generation 5 — and doing it to the one issuer
    // that most needs it. `atGeneration` names a target, and at the
    // ceiling the only nameable target is the ceiling itself: a
    // re-apply, not a rotation. An issuer there could not restore its
    // own persisted state, and had no other way back.
    const ceiling = Identity.generate().atGeneration(0xffffffff)
    expect(ceiling.issuerGeneration).toBe(0xffffffff)

    // Usable for issuance...
    const subject = Identity.generate()
    expect(parseToken(issue(ceiling, subject)).issuerGeneration).toBe(
      0xffffffff,
    )

    // ...and for the restart path.
    const restored = Identity.fromStateBytes(ceiling.toStateBytes())
    expect(restored.issuerGeneration).toBe(0xffffffff)
    expect(restored.atGeneration(0xffffffff).issuerGeneration).toBe(0xffffffff)

    // Backwards is still backwards here.
    expect(() => ceiling.atGeneration(0xfffffffe)).toThrow(
      /generation_went_backwards/,
    )
  })

  it('refuses malformed and future state rather than parsing part of it', () => {
    const good = Identity.generate().atGeneration(2).toStateBytes()

    expect(() => Identity.fromStateBytes(good.subarray(0, 36))).toThrow(
      /invalid_state_length/,
    )
    // A bare seed is not identity state — accepting it would put the
    // generation-zero trap back through the versioned door.
    expect(() => Identity.fromStateBytes(Buffer.alloc(32))).toThrow(
      /invalid_state_length/,
    )

    const future = Buffer.from(good)
    future[0] = 2
    expect(() => Identity.fromStateBytes(future)).toThrow(
      /unsupported_state_version/,
    )
  })

  it('delegation stamps the signer generation, not the parent token one', () => {
    // `delegateToken` used to copy the parent's generation onto the
    // child. The child's issuer is the signer, so that stamped an
    // epoch belonging to an entity that had not signed it.
    const root = Identity.generate().atGeneration(3)
    const machine = Identity.generate().atGeneration(7)
    const leaf = Identity.generate()

    const rootLink = root.issueToken(
      machine.entityId,
      ['publish', 'delegate'],
      CHANNEL,
      3600,
      2,
    )
    expect(parseToken(rootLink).issuerGeneration).toBe(3)

    const machineLink = delegateToken(machine, rootLink, leaf.entityId, [
      'publish',
    ])
    expect(parseToken(machineLink).issuerGeneration).toBe(7)
  })
})
