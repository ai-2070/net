// OSDK-L X2 — a live admitted cross-org call through the Node binding.
//
// The FIRST end-to-end admitted org call in Node: a provider node serves a
// Granted capability that a caller node — in a different organization — invokes
// over real transport, using credentials MINTED BY RUST and loaded from disk.
// The `gen_org_scenario` example writes the whole issuance chain (adopted
// authorities, credential bytes, 0600 audience-secret files, a manifest.json);
// this suite consumes the SAME manifest a Go / Python harness loads.
//
// This closes the "live admitted call owed with X2" gap the plan flags for
// Node: `org_binding.test.ts` proves the refusal paths, and this proves the
// admitted path — that Node can consume real CLI/Rust-issued org artifacts and
// make an admitted cross-org call, with four-party attribution at the handler.
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
const {
  NetMesh,
  OrgCredentials,
  OrgClient,
  serveOrg,
  OrgAccess,
  installOrgAuthority,
  installProviderGrantAudience,
} = binding

const HAS_ORG =
  typeof installOrgAuthority === 'function' &&
  typeof OrgClient?.bind === 'function' &&
  typeof serveOrg === 'function'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Mesh = any
type Manifest = {
  psk_hex: string
  granted_service: string
  provider: { seed_hex: string; authority_dir: string; grant_path: string; grant_secret_path: string }
  caller: {
    seed_hex: string
    authority_dir: string
    membership_path: string
    dispatcher_path: string
    grant_path: string
    grant_secret_path: string
  }
}

const here = dirname(fileURLToPath(import.meta.url))
// bindings/node/test -> crates/net (the cargo workspace root).
const crateRoot = resolve(here, '..', '..', '..')

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

async function meshFromSeed(seedHex: string, pskHex: string): Promise<Mesh> {
  return NetMesh.create({
    bindAddr: '127.0.0.1:0',
    psk: pskHex,
    identitySeed: Buffer.from(seedHex, 'hex'),
    permissiveChannels: true,
  })
}

// The a2a handshake: the acceptor waits for the connector's routed handshake
// while the connector dials — both before `start()`.
async function handshake(connector: Mesh, acceptor: Mesh): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId())
  await sleep(50)
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId())
  await accepted
}

describe.skipIf(!HAS_ORG)('X2 — live cross-org call through the Node binding', () => {
  let dir: string
  let manifest: Manifest

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), 'x2-node-'))
    // Mint a fresh scenario (certs expire, so never a committed fixture).
    execFileSync(
      'cargo',
      ['run', '-q', '-p', 'net-mesh-sdk', '--features', 'net,cortex,fixtures', '--example', 'gen_org_scenario', '--', dir],
      { cwd: crateRoot, stdio: 'inherit' },
    )
    manifest = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')) as Manifest
  }, 300_000)

  afterAll(() => {
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  it('a Node caller invokes a Granted capability a Node provider serves, from generated artifacts', async () => {
    const p = (rel: string) => join(dir, rel)
    const provider = await meshFromSeed(manifest.provider.seed_hex, manifest.psk_hex)
    const caller = await meshFromSeed(manifest.caller.seed_hex, manifest.psk_hex)
    let client: any
    let handle: any
    try {
      // Both nodes load their adopted authority (the binding startup step).
      installOrgAuthority(provider, p(manifest.provider.authority_dir))
      installOrgAuthority(caller, p(manifest.caller.authority_dir))

      await handshake(caller, provider)
      await provider.start()
      await caller.start()

      // Provider: serve first, then install the grant audience (registration
      // first, audience after — the substrate's contract).
      let sawCrossOrgCaller = false
      handle = serveOrg(
        provider,
        manifest.granted_service,
        OrgAccess.Granted,
        async (req: any): Promise<Buffer> => {
          sawCrossOrgCaller = req.caller.isSameOrg === false && req.caller.entity.length === 32
          const body = JSON.parse(req.request.toString('utf8'))
          return Buffer.from(JSON.stringify({ n: body.n + 1, servedBy: 'node-provider' }))
        },
      )
      installProviderGrantAudience(
        provider,
        readFileSync(p(manifest.provider.grant_path)),
        p(manifest.provider.grant_secret_path),
      )

      // Caller: credentials from the generated files (secret by PATH), then bind.
      const credentials = OrgCredentials.create({
        membership: readFileSync(p(manifest.caller.membership_path)),
        dispatcher: readFileSync(p(manifest.caller.dispatcher_path)),
        grants: [readFileSync(p(manifest.caller.grant_path))],
        audienceSecretPaths: [p(manifest.caller.grant_secret_path)],
      })
      client = OrgClient.bind(caller, credentials)

      // Drive private discovery to convergence: force a scoped announce on the
      // provider and retry the call until the grantee resolves it.
      const request = Buffer.from(JSON.stringify({ n: 7 }))
      let reply: Buffer | undefined
      let lastErr: unknown
      // The Node mesh has no way to lower `min_announce_interval` (10s default),
      // so the scoped emission is throttled — wait through a few cycles.
      const deadline = Date.now() + 45_000
      while (Date.now() < deadline && !reply) {
        await Promise.all([
          provider.announceCapabilities({}),
          caller.announceCapabilities({}),
        ]).catch(() => {})
        try {
          reply = await client.callBytes(manifest.granted_service, request)
        } catch (e) {
          lastErr = e
          await sleep(1000)
        }
      }
      if (!reply) {
        // Surface the last failure so a convergence/admission problem is legible.
        // eslint-disable-next-line no-console
        console.error('org_live: call never succeeded; last error =', String(lastErr))
      }
      expect(reply, `the cross-org protected call was admitted (last error: ${String(lastErr)})`).toBeDefined()
      expect(JSON.parse((reply as Buffer).toString('utf8'))).toEqual({ n: 8, servedBy: 'node-provider' })
      expect(sawCrossOrgCaller, 'four-party attribution reached the handler').toBe(true)
    } finally {
      try {
        client?.close()
      } catch {
        /* already closed */
      }
      try {
        handle?.close()
      } catch {
        /* already closed */
      }
      await provider.shutdown().catch(() => {})
      await caller.shutdown().catch(() => {})
    }
  }, 120_000)
})
