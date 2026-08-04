// SSDK S4a — subnet authority refusal paths through the REAL napi
// boundary: construction-time validation, decode-before-mutate
// provisioning, and named-export resolution ordering.

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, installSubnetGatewayCredentials, applySubnetControlFact, serveSubnetExported } =
  binding

// Import BOTH from `../subnet` so the class identity matches the one the
// `admin` wrappers throw through — a mixed `../subnet` + `../errors`
// import trips the compiled-.js-vs-source-.ts dual-module `instanceof`
// hazard even though the thrown value IS a SubnetProvisionError.
import {
  admin,
  classifySubnetError,
  serveSubnetExportedTyped,
  SubnetProvisionError,
} from '../subnet'

const HAS_SUBNET =
  typeof installSubnetGatewayCredentials === 'function' &&
  typeof applySubnetControlFact === 'function' &&
  typeof serveSubnetExported === 'function'

const PSK = '42'.repeat(32)
const AUTHORITY = 'd7'.repeat(32)

function meshOptions(extra: Record<string, unknown> = {}) {
  return {
    bindAddr: '127.0.0.1:0',
    psk: PSK,
    identitySeed: Buffer.from('a1'.repeat(32), 'hex'),
    permissiveChannels: true,
    ...extra,
  }
}

function subnetConfig() {
  return {
    subnetAuthorities: [
      { authorityHex: AUTHORITY, rootHexes: [AUTHORITY], maximumGrantLifetimeSecs: 604800 },
    ],
    subnetAttachment: { levels: [3] },
    subnetExports: [
      {
        name: 'factory-export',
        access: 'granted',
        binding: {
          subnet: { authorityHex: AUTHORITY, path: { levels: [3, 9] } },
          topologyEpoch: 0,
        },
      },
    ],
  }
}

describe.skipIf(!HAS_SUBNET)('subnet authority through the napi boundary', () => {
  it('refuses broken construction config before a node exists', async () => {
    await expect(
      NetMesh.create(
        meshOptions({
          subnetExports: [
            ...subnetConfig().subnetExports,
            ...subnetConfig().subnetExports,
          ],
        }),
      ),
    ).rejects.toThrow(/subnet:duplicate_export_name/)

    await expect(
      NetMesh.create(
        meshOptions({
          subnetAuthorities: [
            { authorityHex: AUTHORITY, rootHexes: [], maximumGrantLifetimeSecs: 60 },
          ],
        }),
      ),
    ).rejects.toThrow(/subnet:empty_authority_roots/)

    await expect(
      NetMesh.create(
        meshOptions({
          subnetAuthorities: [
            { authorityHex: 'zz', rootHexes: [AUTHORITY], maximumGrantLifetimeSecs: 60 },
          ],
        }),
      ),
    ).rejects.toThrow(/subnet:invalid_id_hex/)

    await expect(
      NetMesh.create(meshOptions({ subnetAttachment: { levels: [3, 1, 4, 1, 5] } })),
    ).rejects.toThrow(/subnet:path_too_deep/)

    await expect(
      NetMesh.create(meshOptions({ subnetAttachment: { levels: [300] } })),
    ).rejects.toThrow(/subnet:invalid_path_level/)
  })

  it('provisioning decodes before mutating, and the admin wrapper classifies', async () => {
    const mesh = await NetMesh.create(meshOptions(subnetConfig()))
    try {
      const garbage = Buffer.from([0xff, 0xfe, 0xfd, 0xfc])
      try {
        admin.installGatewayCredentials(mesh, [garbage])
        expect.unreachable('garbage credential bytes must refuse')
      } catch (e) {
        expect(e).toBeInstanceOf(SubnetProvisionError)
        expect((e as SubnetProvisionError).kind).toBe('invalid_format')
      }
      try {
        admin.applyControlFact(mesh, garbage)
        expect.unreachable('garbage fact bytes must refuse')
      } catch (e) {
        expect(e).toBeInstanceOf(SubnetProvisionError)
        expect((e as SubnetProvisionError).kind).toBe('invalid_format')
      }
      // A well-formed boundary declaration is accepted (it is wholesale
      // and infallible after DTO conversion).
      admin.declareBoundaries(mesh, {
        authorityHex: AUTHORITY,
        topologyEpoch: 0,
        boundaries: [{ levels: [3, 9] }],
      })
    } finally {
      await mesh.shutdown().catch(() => {})
    }
  })

  it('an unknown export name fails BEFORE registration; a known one reaches the core', async () => {
    const mesh = await NetMesh.create(meshOptions(subnetConfig()))
    try {
      // Unknown name: refused at resolution, before any registration —
      // even though this node has no org authority installed at all.
      //
      // Asserted on CLASS and KIND, not message text (review-10 P2-1).
      // The registration message wraps the envelope in provider-setup
      // prose, and a text assertion passed happily while the classifier
      // silently declined to classify it at all.
      //
      // The RAW napi seam throws the unclassified message, so this half
      // proves the classifier recovers the kind from the real wire — not
      // from a hand-written sample.
      try {
        serveSubnetExported(mesh, 'fleet.telemetry', 'no-such-export', async () =>
          Buffer.from(''),
        )
        expect.unreachable('an unknown export name must be refused')
      } catch (e) {
        const classified = classifySubnetError(e)
        expect(classified).toBeInstanceOf(SubnetProvisionError)
        expect((classified as SubnetProvisionError).kind).toBe('unknown_export_name')
        // The wrap is genuinely present — this is the embedded-envelope
        // shape, not a message that happens to lead with the token.
        expect(String((e as Error).message)).not.toMatch(/^subnet:/)
      }

      // The application-facing wrapper classifies for you: what an
      // ordinary provider catches is already a SubnetProvisionError.
      try {
        serveSubnetExportedTyped(mesh, 'fleet.telemetry', 'no-such-export', async () => ({}))
        expect.unreachable('an unknown export name must be refused')
      } catch (e) {
        expect(e).toBeInstanceOf(SubnetProvisionError)
        expect((e as SubnetProvisionError).kind).toBe('unknown_export_name')
      }

      // Known name: resolution succeeds and the CORE refusal (no org
      // authority installed) surfaces instead — proving order.
      try {
        serveSubnetExported(mesh, 'fleet.telemetry', 'factory-export', async () =>
          Buffer.from(''),
        )
        expect.unreachable('no org authority is installed, so the core must refuse')
      } catch (e) {
        const msg = String((e as Error).message)
        expect(msg).not.toMatch(/unknown_export_name/)
      }
    } finally {
      await mesh.shutdown().catch(() => {})
    }
  })
})
