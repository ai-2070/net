// Stage-5 stats-shape parity pin for the Node binding.
//
// `NetMesh.traversalStats()` must return the full core snapshot —
// punch outcomes, the derived failure count, the three failure-cause
// counters, background-upgrade activity, and port-mapping state —
// with every key present and booting to zero / false / null on a
// fresh node. Mirrors the Rust SDK's
// `pre_classification_state_is_unknown` and the Go / Python shape
// tests; a core field that stops being forwarded fails here.
//
// Needs the built native addon (nat-traversal is a default build
// feature); skips when it isn't available.

import { describe, expect, it } from 'vitest'

import { NetMesh } from '../index'

const PSK = '42'.repeat(32)

const ZERO_BIGINT_FIELDS = [
  'punchesAttempted',
  'punchesSucceeded',
  'punchesFailed',
  'relayFallbacks',
  'punchTimeouts',
  'punchRejections',
  'rendezvousNoRelay',
  'upgradesAttempted',
  'upgradesSucceeded',
  'upgradesDeferredBusy',
  'portMappingRenewals',
] as const

describe('traversalStats shape parity (stage 5)', () => {
  it('boots the full snapshot to zero / false / null', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK })
    try {
      const stats = mesh.traversalStats()
      for (const field of ZERO_BIGINT_FIELDS) {
        expect(stats, `missing field ${field}`).toHaveProperty(field)
        expect(stats[field], `${field} should boot to 0`).toBe(0n)
      }
      expect(stats.portMappingActive).toBe(false)
      // napi maps `Option::None` to an absent property — no active
      // mapping means `portMappingExternal` is undefined, not null.
      expect(stats.portMappingExternal).toBeUndefined()
      // No undocumented fields — an extra one means the core
      // snapshot grew without the four binding parity pins being
      // updated together.
      const known = new Set<string>([
        ...ZERO_BIGINT_FIELDS,
        'portMappingActive',
        'portMappingExternal',
      ])
      for (const key of Object.keys(stats)) {
        expect(known.has(key), `undocumented stats field ${key}`).toBe(true)
      }
    } finally {
      await mesh.shutdown()
    }
  })

  it('exposes connectDirectAuto', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK })
    try {
      expect(typeof mesh.connectDirectAuto).toBe('function')
    } finally {
      await mesh.shutdown()
    }
  })

  it('accepts the autoDirectUpgrade option', async () => {
    const mesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      autoDirectUpgrade: true,
    })
    await mesh.shutdown()
  })
})
