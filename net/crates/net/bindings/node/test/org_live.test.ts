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
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'

import { afterAll, beforeAll, describe, expect, it } from 'vitest'

import type { OrgCaller, OrgServeHandle } from '../org'
import {
  classifyOrgError,
  OrgAdmissionDeniedError,
  OrgError,
  serveOrgClientStreamTyped,
  serveOrgDuplexTyped,
  serveOrgStreamingTyped,
  TypedOrgClient,
} from '../org'

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
  MeshRpc,
} = binding

const HAS_ORG =
  typeof installOrgAuthority === 'function' &&
  typeof OrgClient?.bind === 'function' &&
  typeof serveOrg === 'function'

// The Stage 4 streaming surface: the §4.4 verbs plus the `test-helpers`
// same-org scenario minter. Skips cleanly on a build without them.
const HAS_S4 =
  HAS_ORG &&
  typeof binding.serveOrgStreaming === 'function' &&
  typeof binding.testMintSameOrgScenario === 'function'

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

// ===========================================================================
// S4 (Stage 4, Node row) — the four protected streaming shapes, call AND
// serve, same-org AND granted, over the §4.4 verbs.
//
// Fixture consumption: GRANTED rides the `gen_org_scenario` manifest (the
// same issuance chain every language's live cell loads); SAME-ORG rides the
// `test-helpers`-gated `testMintSameOrgScenario` (no generator mints a
// shared-owner-audience same-org chain). Every shape's handler-side
// attribution asserts the five admission-verified `OrgCaller` facts EXACTLY
// — that assertion is the projection witness whose inverse receipt (a
// projection reporting anything but the verified facts) must redden it.
// ===========================================================================

const S4_PSK = '51'.repeat(32)

type SameOrgManifest = {
  org_id_hex: string
  provider: { seed_hex: string; entity_id_hex: string; authority_dir: string }
  caller: {
    seed_hex: string
    entity_id_hex: string
    authority_dir: string
    membership_path: string
    dispatcher_path: string
  }
}

const hex = (b: Buffer): string => Buffer.from(b).toString('hex')

/**
 * Plain `mkdir`, NOT `mkdtemp`: on Windows an mkdtemp directory carries an
 * owner-only ACL whose inheritance the audience-secret loader (rightly)
 * refuses on files created beneath it (the Python twin's live cells carry
 * the same note). The Rust minter's checked writers need ordinary
 * per-user directories.
 */
function tmpScenarioDir(tag: string): string {
  const dir = join(
    tmpdir(),
    `org-s4-${tag}-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`,
  )
  mkdirSync(dir, { recursive: true })
  return dir
}

/**
 * Scoped discovery is announcement-throttled (`min_announce_interval`, 10 s
 * default, no binding-side override) — force an emission per cycle and
 * retry the attempt. Each retry is a FRESH call: a signed proof binds one
 * call id, so the facade never retries underneath. Same shape as the X2
 * cell above.
 */
async function convergeOrg<T>(
  provider: Mesh,
  caller: Mesh,
  attempt: () => Promise<T>,
  budgetMs = 45_000,
): Promise<T> {
  const deadline = Date.now() + budgetMs
  let lastErr: unknown
  while (Date.now() < deadline) {
    await Promise.all([
      provider.announceCapabilities({}),
      caller.announceCapabilities({}),
    ]).catch(() => {})
    try {
      return await attempt()
    } catch (e) {
      lastErr = e
      await sleep(1000)
    }
  }
  // Surface the last failure so a convergence/admission problem is legible.
  // eslint-disable-next-line no-console
  console.error('org_s4: attempt never succeeded; last error =', String(lastErr))
  throw lastErr
}

/**
 * THE projection witness oracle: the five admission-verified `OrgCaller`
 * facts, exact. `capability` is asserted by shape (32 raw bytes) — its
 * value derives from the service tag through `CapabilityAuthorityId`.
 */
function expectVerifiedCaller(
  c: OrgCaller | undefined,
  expected: {
    entity: string
    actingOrg: string
    providerOrg: string
    provider: string
    sameOrg: boolean
  },
): void {
  expect(c, 'the handler ran and captured its caller').toBeDefined()
  const caller = c as OrgCaller
  expect(hex(caller.entity), 'verified caller entity').toBe(expected.entity)
  expect(hex(caller.actingOrg), 'verified acting org').toBe(expected.actingOrg)
  expect(hex(caller.providerOrg), 'verified provider org').toBe(expected.providerOrg)
  expect(hex(caller.provider), 'verified provider entity').toBe(expected.provider)
  expect(caller.isSameOrg, 'the verified same-org relation').toBe(expected.sameOrg)
  expect(caller.capability.length, 'the invoked capability id is 32 raw bytes').toBe(32)
}

describe.skipIf(!HAS_S4)('S4 — same-org streaming: call AND serve through the Node binding', () => {
  const SVC = {
    ss: 's4.same.ss',
    cs: 's4.same.cs',
    dx: 's4.same.dx',
    unary: 's4.same.unary',
    cancel: 's4.same.cancel',
    reject: 's4.same.reject',
  }
  let dir: string
  let manifest: SameOrgManifest
  let provider: Mesh
  let caller: Mesh
  let typed: TypedOrgClient
  const handles: OrgServeHandle[] = []
  let attrSS: OrgCaller | undefined
  let attrCS: OrgCaller | undefined
  let attrDX: OrgCaller | undefined

  beforeAll(async () => {
    dir = tmpScenarioDir('same')
    manifest = JSON.parse(binding.testMintSameOrgScenario(dir)) as SameOrgManifest
    provider = await meshFromSeed(manifest.provider.seed_hex, S4_PSK)
    caller = await meshFromSeed(manifest.caller.seed_hex, S4_PSK)
    installOrgAuthority(provider, join(dir, manifest.provider.authority_dir))
    installOrgAuthority(caller, join(dir, manifest.caller.authority_dir))
    await handshake(caller, provider)
    await provider.start()
    await caller.start()

    handles.push(
      serveOrgStreamingTyped<{ n: number }, { n: number }>(
        provider,
        SVC.ss,
        OrgAccess.SameOrg,
        (c, req, sink) => {
          attrSS = c
          sink.send({ n: req.n + 1 })
          sink.send({ n: req.n + 2 })
        },
      ),
    )
    handles.push(
      serveOrgClientStreamTyped<{ n: number }, { count: number; total: number }>(
        provider,
        SVC.cs,
        OrgAccess.SameOrg,
        async (c, stream) => {
          attrCS = c
          let count = 0
          let total = 0
          for await (const item of stream) {
            count += 1
            total += item.n
          }
          return { count, total }
        },
      ),
    )
    handles.push(
      serveOrgDuplexTyped<{ n: number }, { n: number }>(
        provider,
        SVC.dx,
        OrgAccess.SameOrg,
        async (c, stream, sink) => {
          attrDX = c
          for await (const item of stream) {
            sink.send({ n: item.n * 10 })
          }
        },
      ),
    )

    typed = TypedOrgClient.bind(
      caller,
      OrgCredentials.create({
        membership: readFileSync(join(dir, manifest.caller.membership_path)),
        dispatcher: readFileSync(join(dir, manifest.caller.dispatcher_path)),
        grants: [],
        audienceSecretPaths: [],
      }),
    )
  }, 120_000)

  afterAll(() => {
    try {
      typed?.close()
    } catch {
      /* already closed */
    }
    for (const h of handles) {
      try {
        h.close()
      } catch {
        /* already closed */
      }
    }
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  it('live_same_org_streaming_call_and_serve', async () => {
    const items = await convergeOrg(provider, caller, async () => {
      const stream = await typed.callStreaming<{ n: number }, { n: number }>(SVC.ss, { n: 7 })
      const out: { n: number }[] = []
      for await (const item of stream) {
        out.push(item)
      }
      return out
    })
    expect(items, 'exact streamed payloads').toEqual([{ n: 8 }, { n: 9 }])
    expectVerifiedCaller(attrSS, {
      entity: manifest.caller.entity_id_hex,
      actingOrg: manifest.org_id_hex,
      providerOrg: manifest.org_id_hex,
      provider: manifest.provider.entity_id_hex,
      sameOrg: true,
    })
  }, 120_000)

  it('live_same_org_client_stream_call_and_serve', async () => {
    const summary = await convergeOrg(provider, caller, async () => {
      const call = await typed.callClientStream<{ n: number }, { count: number; total: number }>(
        SVC.cs,
      )
      await call.send({ n: 10 })
      await call.send({ n: 20 })
      return await call.finish()
    })
    expect(summary, 'exact terminal summary').toEqual({ count: 2, total: 30 })
    expectVerifiedCaller(attrCS, {
      entity: manifest.caller.entity_id_hex,
      actingOrg: manifest.org_id_hex,
      providerOrg: manifest.org_id_hex,
      provider: manifest.provider.entity_id_hex,
      sameOrg: true,
    })
  }, 120_000)

  it('live_same_org_duplex_call_and_serve', async () => {
    const echoes = await convergeOrg(provider, caller, async () => {
      const [sink, stream] = await typed.callDuplex<{ n: number }, { n: number }>(SVC.dx)
      await sink.send({ n: 1 })
      await sink.send({ n: 2 })
      await sink.finish()
      const out: { n: number }[] = []
      for await (const item of stream) {
        out.push(item)
      }
      return out
    })
    expect(echoes, 'exact duplex echoes').toEqual([{ n: 10 }, { n: 20 }])
    expectVerifiedCaller(attrDX, {
      entity: manifest.caller.entity_id_hex,
      actingOrg: manifest.org_id_hex,
      providerOrg: manifest.org_id_hex,
      provider: manifest.provider.entity_id_hex,
      sameOrg: true,
    })
  }, 120_000)

  it('a_stream_call_against_a_unary_only_provider_is_refused_not_supported', async () => {
    // The §4.4 error route at the RAW layer: the seam's handle renders the
    // `org:` wire vocabulary and `classifyOrgError` classifies it. The
    // `reason` pin is the coarse-byte mirror's discriminator — a mirror
    // that decoded the wrong byte (or fell back to `denied`) reddens it.
    const handle = serveOrg(
      provider,
      SVC.unary,
      OrgAccess.SameOrg,
      async (req: { request: Buffer }): Promise<Buffer> => req.request,
    )
    try {
      const call = await convergeOrg(provider, caller, () =>
        typed.raw.callClientStreamBytes(SVC.unary),
      )
      const thrown = await call
        .send(Buffer.from(JSON.stringify({ n: 1 })))
        .then(() => call.finish())
        .then(
          () => undefined,
          (e: unknown) => e,
        )
      expect(thrown, 'a stream-shaped call on a unary-only provider is refused').toBeDefined()
      const rawErr = thrown as Error
      expect(rawErr.message.startsWith('org:admission_denied:'), `raw wire vocabulary: ${rawErr.message}`).toBe(true)
      const classified = classifyOrgError(thrown)
      expect(classified).toBeInstanceOf(OrgAdmissionDeniedError)
      expect((classified as OrgAdmissionDeniedError).reason).toBe('not_supported')
    } finally {
      handle.close()
    }
  }, 120_000)

  it('midstream_outcomes_route_through_classify_org_error', async () => {
    // Two midstream observables, both at the §4.4 route:
    //
    // (1) CANCEL retirement (the §2.2 contract at the caller's handle,
    //     EXECUTED here): the retired call's items are DISCARDED and the
    //     stream ends with `null` — never a late item. That EOF plus the
    //     one wire CANCEL is what "cancellation is observed through the
    //     retirement observables" means for the caller. The handler just
    //     sleeps and is never told (the F-S3.1-2 handler-drop level).
    // (2) A midstream terminal ERROR (a handler rejection): the reused
    //     typed stream throws the classifyOrgError OUTPUT — an `OrgError`
    //     with the frozen `org:rpc:` kind, not a bare string.
    const cancelHandle = serveOrgStreamingTyped<{ n: number }, { n: number }>(
      provider,
      SVC.cancel,
      OrgAccess.SameOrg,
      async (_c, _req, sink) => {
        sink.send({ n: 1 })
        await sleep(30_000)
      },
    )
    const rpc = MeshRpc.fromMesh(caller)
    try {
      const opened = await convergeOrg(provider, caller, async () => {
        const token: bigint = rpc.reserveCancelToken()
        const stream = await typed.callStreaming<{ n: number }, { n: number }>(
          SVC.cancel,
          { n: 1 },
          { cancelToken: token },
        )
        return { stream, token }
      })
      expect(await opened.stream.next(), 'the live item before retirement').toEqual({ n: 1 })
      rpc.cancelCall(opened.token)
      expect(
        await opened.stream.next(),
        'cancel retirement discards the call — the stream ends, never a late item',
      ).toBeNull()
      expect(await opened.stream.next(), 'the ended stream stays ended').toBeNull()
    } finally {
      cancelHandle.close()
    }

    const rejectHandle = serveOrgStreamingTyped<{ n: number }, { n: number }>(
      provider,
      SVC.reject,
      OrgAccess.SameOrg,
      async (_c, _req, sink) => {
        sink.send({ n: 1 })
        throw new Error('S4 midstream handler rejection')
      },
    )
    try {
      const stream = await convergeOrg(provider, caller, () =>
        typed.callStreaming<{ n: number }, { n: number }>(SVC.reject, { n: 1 }),
      )
      expect(await stream.next(), 'the live item before the terminal error').toEqual({ n: 1 })
      let thrown: unknown
      try {
        await stream.next()
      } catch (e) {
        thrown = e
      }
      expect(thrown, 'the midstream terminal error surfaces from next()').toBeDefined()
      expect(thrown, 'midstream errors route through classifyOrgError').toBeInstanceOf(OrgError)
      const err = thrown as OrgError
      expect(err.domain, 'the frozen rpc domain').toBe('rpc')
      expect(err.kind, 'a handler rejection is the server_error kind').toBe('server_error')
    } finally {
      rejectHandle.close()
    }
  }, 120_000)
})

describe.skipIf(!HAS_S4)('S4 — granted streaming: call AND serve through the Node binding', () => {
  let dir: string
  let manifest: Manifest
  let provider: Mesh
  let caller: Mesh
  let typed: TypedOrgClient
  let attrSS: OrgCaller | undefined
  let attrCS: OrgCaller | undefined
  let attrDX: OrgCaller | undefined

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), 'x2-node-s4-'))
    execFileSync(
      'cargo',
      ['run', '-q', '-p', 'net-mesh-sdk', '--features', 'net,cortex,fixtures', '--example', 'gen_org_scenario', '--', dir],
      { cwd: crateRoot, stdio: 'inherit' },
    )
    manifest = JSON.parse(readFileSync(join(dir, 'manifest.json'), 'utf8')) as Manifest
    provider = await meshFromSeed(manifest.provider.seed_hex, manifest.psk_hex)
    caller = await meshFromSeed(manifest.caller.seed_hex, manifest.psk_hex)
    installOrgAuthority(provider, join(dir, manifest.provider.authority_dir))
    installOrgAuthority(caller, join(dir, manifest.caller.authority_dir))
    await handshake(caller, provider)
    await provider.start()
    await caller.start()
    // The provider's grant audience (registration before OR after is the
    // substrate's contract — installing first is discoverable immediately).
    installProviderGrantAudience(
      provider,
      readFileSync(join(dir, manifest.provider.grant_path)),
      join(dir, manifest.provider.grant_secret_path),
    )
    typed = TypedOrgClient.bind(
      caller,
      OrgCredentials.create({
        membership: readFileSync(join(dir, manifest.caller.membership_path)),
        dispatcher: readFileSync(join(dir, manifest.caller.dispatcher_path)),
        grants: [readFileSync(join(dir, manifest.caller.grant_path))],
        audienceSecretPaths: [join(dir, manifest.caller.grant_secret_path)],
      }),
    )
  }, 300_000)

  afterAll(() => {
    try {
      typed?.close()
    } catch {
      /* already closed */
    }
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  // One service name holds ONE registration (and the grant covers exactly
  // `nrpc:<granted_service>`), so the three shape siblings run SEQUENTIALLY
  // against it — each registers its own serve, calls, and unregisters.
  const grantedExpectation = () => ({
    // Computed from the live meshes, not the manifest: the entity ids are
    // what `EntityKeypair::from_bytes(seed)` derives.
    entity: hex(caller.entityId()),
    actingOrg: manifest.caller.org_id_hex,
    providerOrg: manifest.provider.org_id_hex,
    provider: hex(provider.entityId()),
    sameOrg: false,
  })

  it('live_granted_streaming_call_and_serve', async () => {
    const handle = serveOrgStreamingTyped<{ n: number }, { n: number }>(
      provider,
      manifest.granted_service,
      OrgAccess.Granted,
      (c, req, sink) => {
        attrSS = c
        sink.send({ n: req.n + 1 })
        sink.send({ n: req.n + 2 })
      },
    )
    try {
      const items = await convergeOrg(provider, caller, async () => {
        const stream = await typed.callStreaming<{ n: number }, { n: number }>(
          manifest.granted_service,
          { n: 7 },
        )
        const out: { n: number }[] = []
        for await (const item of stream) {
          out.push(item)
        }
        return out
      })
      expect(items, 'exact streamed payloads').toEqual([{ n: 8 }, { n: 9 }])
      expectVerifiedCaller(attrSS, grantedExpectation())
    } finally {
      handle.close()
    }
  }, 120_000)

  it('live_granted_client_stream_call_and_serve', async () => {
    const handle = serveOrgClientStreamTyped<{ n: number }, { count: number; total: number }>(
      provider,
      manifest.granted_service,
      OrgAccess.Granted,
      async (c, stream) => {
        attrCS = c
        let count = 0
        let total = 0
        for await (const item of stream) {
          count += 1
          total += item.n
        }
        return { count, total }
      },
    )
    try {
      const summary = await convergeOrg(provider, caller, async () => {
        const call = await typed.callClientStream<
          { n: number },
          { count: number; total: number }
        >(manifest.granted_service)
        await call.send({ n: 10 })
        await call.send({ n: 20 })
        return await call.finish()
      })
      expect(summary, 'exact terminal summary').toEqual({ count: 2, total: 30 })
      expectVerifiedCaller(attrCS, grantedExpectation())
    } finally {
      handle.close()
    }
  }, 120_000)

  it('live_granted_duplex_call_and_serve', async () => {
    const handle = serveOrgDuplexTyped<{ n: number }, { n: number }>(
      provider,
      manifest.granted_service,
      OrgAccess.Granted,
      async (c, stream, sink) => {
        attrDX = c
        for await (const item of stream) {
          sink.send({ n: item.n * 10 })
        }
      },
    )
    try {
      const echoes = await convergeOrg(provider, caller, async () => {
        const [sink, stream] = await typed.callDuplex<{ n: number }, { n: number }>(
          manifest.granted_service,
        )
        await sink.send({ n: 1 })
        await sink.send({ n: 2 })
        await sink.finish()
        const out: { n: number }[] = []
        for await (const item of stream) {
          out.push(item)
        }
        return out
      })
      expect(echoes, 'exact duplex echoes').toEqual([{ n: 10 }, { n: 20 }])
      expectVerifiedCaller(attrDX, grantedExpectation())
    } finally {
      handle.close()
    }
  }, 120_000)
})

describe.skipIf(!HAS_S4)('S4 — disposal: bounded cleanup through the documented teardown', () => {
  let dir: string

  afterAll(() => {
    if (dir) rmSync(dir, { recursive: true, force: true })
  })

  it('closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly', async () => {
    // The module docs' teardown contract, asserted STRICTLY (no swallowed
    // shutdown): `orgClient.close() -> serveHandle.close() ->
    // await mesh.shutdown()` must release every reference — the failure
    // mode is a REJECTED shutdown ("outstanding references exist").
    dir = tmpScenarioDir('dispose')
    const manifest = JSON.parse(binding.testMintSameOrgScenario(dir)) as SameOrgManifest
    const provider = await meshFromSeed(manifest.provider.seed_hex, S4_PSK)
    const caller = await meshFromSeed(manifest.caller.seed_hex, S4_PSK)
    installOrgAuthority(provider, join(dir, manifest.provider.authority_dir))
    installOrgAuthority(caller, join(dir, manifest.caller.authority_dir))
    await handshake(caller, provider)
    await provider.start()
    await caller.start()
    const handle = serveOrgStreamingTyped<{ n: number }, { n: number }>(
      provider,
      's4.dispose.ss',
      OrgAccess.SameOrg,
      (_c, req, sink) => {
        sink.send({ n: req.n + 1 })
      },
    )
    const typed = TypedOrgClient.bind(
      caller,
      OrgCredentials.create({
        membership: readFileSync(join(dir, manifest.caller.membership_path)),
        dispatcher: readFileSync(join(dir, manifest.caller.dispatcher_path)),
        grants: [],
        audienceSecretPaths: [],
      }),
    )
    try {
      const items = await convergeOrg(provider, caller, async () => {
        const stream = await typed.callStreaming<{ n: number }, { n: number }>('s4.dispose.ss', {
          n: 41,
        })
        const out: { n: number }[] = []
        for await (const item of stream) {
          out.push(item)
        }
        return out
      })
      expect(items, 'the call completed before teardown').toEqual([{ n: 42 }])
    } finally {
      typed.close()
      handle.close()
    }
    await provider.shutdown()
    await caller.shutdown()
  }, 120_000)
})
