// S4 — the Node/TypeScript live cell for the subnet-exported surface.
//
// A provider inside a protected subnet serves a NAMED export over real
// transport, a same-org caller invokes it with organization authority only,
// and a foreign-org caller is refused — all from artifacts MINTED BY RUST and
// loaded from disk. The `gen_subnet_scenario` example writes the whole chain
// (subnet authority root, an EXPORT credential at the exact crossing, the
// boundary declaration, adopted org authorities, both callers' credentials, a
// manifest.json); this suite consumes the SAME manifest the Python, Go, and C
// harnesses load.
//
// Ten points, all proven here:
//
//    1 provider construction: roots, attachment, named exports
//    2 local refusal of an unknown export, before announcement
//    3 serve through the frozen named-export API
//    4 caller construction from real generated org credentials
//    5 live public discovery
//    6 a successful callExported
//    7 verified caller + organization attribution at the handler
//    8 fail-closed for a foreign-org caller
//    9 that denial is not retried
//   10 clean close, with no callback racing teardown
//
// Env: needs a Rust toolchain (to generate the scenario) and the .node built
// with the `org` feature; skips cleanly otherwise.

import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'

import { afterAll, beforeAll, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { NetMesh, OrgCredentials, OrgClient, installOrgAuthority, serveSubnetExported } = binding

// Probe the ROOT entry before touching `../subnet`, which refuses at import on
// a feature-off build.
const HAS_SUBNET =
  typeof installOrgAuthority === 'function' &&
  typeof OrgClient?.bind === 'function' &&
  typeof serveSubnetExported === 'function'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const subnetMod: any = HAS_SUBNET ? await import('../subnet') : {}
const { admin, SubnetProvisionError, classifySubnetError } = subnetMod

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Mesh = any

type Manifest = {
  psk_hex: string
  exported_service: string
  export_name: string
  unknown_export_name: string
  export_access: string
  subnet_authorities: {
    authority_hex: string
    root_hexes: string[]
    maximum_grant_lifetime_secs: number
  }[]
  export_binding: { authority_hex: string; path: number[]; topology_epoch: number }
  provider: {
    seed_hex: string
    entity_id_hex: string
    org_id_hex: string
    authority_dir: string
    attachment: number[]
    gateway_credentials_path: string
    boundary_paths: number[][]
  }
  caller: {
    seed_hex: string
    entity_id_hex: string
    org_id_hex: string
    authority_dir: string
    membership_path: string
    dispatcher_path: string
  }
  foreign_caller: Manifest['caller']
}

const here = dirname(fileURLToPath(import.meta.url))
// bindings/node/test -> crates/net (the cargo workspace root).
const crateRoot = resolve(here, '..', '..', '..')

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const hex = (b: Buffer) => b.toString('hex')

// The a2a handshake: the acceptor waits for the connector's routed handshake
// while the connector dials — both before `start()`.
async function handshake(connector: Mesh, acceptor: Mesh): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId())
  await sleep(50)
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
  await accepted
}

describe.skipIf(!HAS_SUBNET)('S4 — live subnet-exported call through the Node binding', () => {
  let dir: string
  let manifest: Manifest
  const p = (rel: string) => join(dir, rel)

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), 's4-node-'))
    // Mint a fresh scenario (credentials expire, so never a committed fixture).
    execFileSync(
      'cargo',
      [
        'run', '-q', '-p', 'net-mesh-sdk',
        '--features', 'net,cortex,fixtures',
        '--example', 'gen_subnet_scenario', '--', dir,
      ],
      { cwd: crateRoot, stdio: 'inherit' },
    )
    manifest = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')) as Manifest
  }, 300_000)

  afterAll(() => {
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  it('serves a named export, admits the same org, refuses another, and closes clean', async () => {
    // ---- (1) provider construction: roots, attachment, named export ----
    //
    // Every subnet input is CONSTRUCTION state, validated by Rust before the
    // node exists. Application code names them; it builds no authority object.
    const provider = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: manifest.psk_hex,
      identitySeed: Buffer.from(manifest.provider.seed_hex, 'hex'),
      permissiveChannels: true,
      subnetAuthorities: manifest.subnet_authorities.map((a) => ({
        authorityHex: a.authority_hex,
        rootHexes: a.root_hexes,
        maximumGrantLifetimeSecs: a.maximum_grant_lifetime_secs,
      })),
      subnetAttachment: { levels: manifest.provider.attachment },
      subnetExports: [
        {
          name: manifest.export_name,
          access: manifest.export_access,
          binding: {
            subnet: {
              authorityHex: manifest.export_binding.authority_hex,
              path: { levels: manifest.export_binding.path },
            },
            topologyEpoch: manifest.export_binding.topology_epoch,
          },
        },
      ],
    })

    let caller: Mesh | undefined
    let foreign: Mesh | undefined
    let client: any
    let foreignClient: any
    let handle: any
    try {
      installOrgAuthority(provider, p(manifest.provider.authority_dir))

      // Gateway provisioning from the generated artifacts — wholesale.
      admin.installGatewayCredentials(provider, [
        readFileSync(p(manifest.provider.gateway_credentials_path)),
      ])
      admin.declareBoundaries(provider, {
        authorityHex: manifest.export_binding.authority_hex,
        topologyEpoch: manifest.export_binding.topology_epoch,
        boundaries: manifest.provider.boundary_paths.map((levels) => ({ levels })),
      })

      // ---- (2) an unknown export is refused LOCALLY, before announcement ----
      try {
        serveSubnetExported(
          provider,
          manifest.exported_service,
          manifest.unknown_export_name,
          async () => Buffer.from(''),
        )
        expect.unreachable('an unconfigured export name must be refused')
      } catch (e) {
        const classified = classifySubnetError(e)
        expect(classified).toBeInstanceOf(SubnetProvisionError)
        expect((classified as { kind: string }).kind).toBe('unknown_export_name')
      }

      // ---- (3) serve through the frozen named-export API ----
      let calls = 0
      let attributionOk = false
      handle = subnetMod.serveSubnetExported(
        provider,
        manifest.exported_service,
        manifest.export_name,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        async (c: any, req: { n: number }) => {
          calls += 1
          // ---- (7) attribution: the provider's VERIFIED view, checked
          // against the identities the manifest itself declares.
          attributionOk =
            hex(c.entity) === manifest.caller.entity_id_hex &&
            hex(c.actingOrg) === manifest.caller.org_id_hex &&
            hex(c.providerOrg) === manifest.provider.org_id_hex &&
            c.isSameOrg === true
          return { n: req.n + 1, servedBy: 'node-s4' }
        },
      )

      // ---- (4) caller construction from the generated credentials ----
      caller = await NetMesh.create({
        bindAddr: '127.0.0.1:0',
        psk: manifest.psk_hex,
        identitySeed: Buffer.from(manifest.caller.seed_hex, 'hex'),
        permissiveChannels: true,
      })
      installOrgAuthority(caller, p(manifest.caller.authority_dir))
      await handshake(caller, provider)
      await provider.start()
      await caller.start()

      client = OrgClient.bind(
        caller,
        OrgCredentials.create({
          membership: readFileSync(p(manifest.caller.membership_path)),
          dispatcher: readFileSync(p(manifest.caller.dispatcher_path)),
          grants: [],
          audienceSecretPaths: [],
        }),
      )

      // ---- (5) live public discovery, and (6) the call ----
      //
      // Discovery is announcement-driven, so drive it to convergence rather
      // than assuming the first attempt lands.
      const request = Buffer.from(JSON.stringify({ n: 1 }))
      let reply: Buffer | undefined
      let lastErr: unknown
      const deadline = Date.now() + 45_000
      while (Date.now() < deadline && !reply) {
        await Promise.all([
          provider.announceCapabilities({}),
          caller.announceCapabilities({}),
        ]).catch(() => {})
        try {
          reply = await client.callExportedBytes(manifest.exported_service, request)
        } catch (e) {
          lastErr = e
          await sleep(500)
        }
      }
      expect(
        reply,
        `the exported call was admitted (last error: ${String(lastErr)})`,
      ).toBeDefined()
      expect(JSON.parse((reply as Buffer).toString('utf8'))).toEqual({
        n: 2,
        servedBy: 'node-s4',
      })
      expect(calls, 'handler ran exactly once').toBe(1)
      // ---- (7) ----
      expect(attributionOk, 'the handler saw the verified caller and org attribution').toBe(true)

      // ---- (8) fail-closed: a FOREIGN-org caller with valid credentials ----
      //
      // Its membership and dispatcher grant are correctly signed — by the
      // WRONG organization. That is what makes this a boundary test rather
      // than a decoder test.
      foreign = await NetMesh.create({
        bindAddr: '127.0.0.1:0',
        psk: manifest.psk_hex,
        identitySeed: Buffer.from(manifest.foreign_caller.seed_hex, 'hex'),
        permissiveChannels: true,
      })
      installOrgAuthority(foreign, p(manifest.foreign_caller.authority_dir))
      await handshake(foreign, provider)
      await foreign.start()
      foreignClient = OrgClient.bind(
        foreign,
        OrgCredentials.create({
          membership: readFileSync(p(manifest.foreign_caller.membership_path)),
          dispatcher: readFileSync(p(manifest.foreign_caller.dispatcher_path)),
          grants: [],
          audienceSecretPaths: [],
        }),
      )

      const before = calls
      await expect(
        foreignClient.callExportedBytes(
          manifest.exported_service,
          Buffer.from(JSON.stringify({ n: 50 })),
        ),
      ).rejects.toBeDefined()
      expect(calls, 'the handler must never run for a refused caller').toBe(before)

      // ---- (9) the denial is not retried ----
      //
      // A signed proof is never resent. Observed provider-side: a second
      // refused call still never reaches the handler.
      await expect(
        foreignClient.callExportedBytes(
          manifest.exported_service,
          Buffer.from(JSON.stringify({ n: 51 })),
        ),
      ).rejects.toBeDefined()
      expect(calls, 'no retry may smuggle a refused caller into the handler').toBe(before)

      // ---- (10) clean close, no callback racing teardown ----
      handle.close()
      handle.close() // idempotent
      handle = undefined
      await expect(
        client.callExportedBytes(
          manifest.exported_service,
          Buffer.from(JSON.stringify({ n: 99 })),
        ),
      ).rejects.toBeDefined()
      expect(calls, 'no handler invocation may land after close').toBe(1)
    } finally {
      for (const c of [client, foreignClient]) {
        try {
          c?.close()
        } catch {
          /* already closed */
        }
      }
      try {
        handle?.close()
      } catch {
        /* already closed */
      }
      await provider.shutdown().catch(() => {})
      await caller?.shutdown().catch(() => {})
      await foreign?.shutdown().catch(() => {})
    }
  }, 180_000)
})
