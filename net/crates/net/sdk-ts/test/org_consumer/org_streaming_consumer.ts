// THE executable consumer program for Stage 4's Pure-SDK (Q6) TS row.
//
// This is a REAL external consumer of the shipped packages: it imports
// the org surface through `@net-mesh/sdk` alone (root + `/org` entries —
// never `@net-mesh/core`, never the source tree) plus node's stdlib, and
// then CALLS AND SERVES all four RPC shapes over live two-mesh round
// trips. The `check-ts-consumer.sh` discipline is here but EXECUTABLE:
// `test/org_live.test.ts` stages this file into a consumer project whose
// `node_modules` holds fresh COPIES of the packaged `@net-mesh/sdk`
// (the shipped `dist` + manifest) and `@net-mesh/core`, type-checks it
// with `skipLibCheck: false`, compiles it, and runs
// `node out/org_streaming_consumer.js <scenario> <fixtureDir>` once per
// witness row.
//
// Fixtures are issuer-produced files loaded from disk — exactly the real
// consumer flow (the harness mints them; a consumer only ever LOADS
// credentials). Every scenario is self-contained: two meshes, handshake,
// authority install, serve, call, assert, teardown. A failed named
// assertion exits non-zero with the assertion's name in the message.
//
// Scenarios (= the driver's cell names, 1:1):
//   same_org_unary_call_and_serve          the preserved unary, same-org
//   same_org_streaming_call_and_serve      SS + verified-caller attribution
//   same_org_client_stream_call_and_serve  CS + attribution + never origin
//   same_org_duplex_call_and_serve         DX + attribution
//   granted_unary_call_and_serve           the preserved unary, granted
//   granted_streaming_call_and_serve
//   granted_client_stream_call_and_serve
//   granted_duplex_call_and_serve
//   midstream_outcomes_surface_an_org_error_never_a_false_clean_eof
//   closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly

import { MeshNode, OrgError } from '@net-mesh/sdk';
import {
  OrgAccess,
  OrgClient,
  OrgCredentials,
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedRpcStream,
  classifyOrgError,
  installOrgAuthority,
  installProviderGrantAudience,
  serveOrg,
  serveOrgClientStream,
  serveOrgDuplex,
  serveOrgStreaming,
  type OrgCaller,
  type OrgServeHandle,
  type TypedRequestStream,
} from '@net-mesh/sdk/org';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// ---------------------------------------------------------------------------
// Assertion plumbing — named failures, no assertion library (the consumer
// depends on `@net-mesh/sdk` alone).
// ---------------------------------------------------------------------------

function fail(name: string, detail?: string): never {
  throw new Error(detail === undefined ? name : `${name}: ${detail}`);
}

function check(cond: boolean, name: string, detail?: string): void {
  if (!cond) fail(name, detail);
}

function expectEq(name: string, actual: string | undefined, expected: string): void {
  if (actual !== expected) {
    fail(name, `expected '${expected}' but the handler saw '${String(actual)}'`);
  }
}

function hex(b: Buffer): string {
  return Buffer.from(b).toString('hex');
}

function msg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function sleep(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

// ---------------------------------------------------------------------------
// Fixture shapes (issuer-produced manifests loaded from disk).
// ---------------------------------------------------------------------------

/**
 * `gen_subnet_scenario`'s manifest fields the same-org rig consumes
 * (provider and caller in ONE org; the harness pre-stages the shared
 * owner audience before this program runs).
 */
type SameOrgManifest = {
  psk_hex: string;
  provider: {
    seed_hex: string;
    entity_id_hex: string;
    org_id_hex: string;
    authority_dir: string;
  };
  caller: {
    seed_hex: string;
    entity_id_hex: string;
    org_id_hex: string;
    authority_dir: string;
    membership_path: string;
    dispatcher_path: string;
  };
};

/** `gen_org_scenario`'s manifest (granted: two orgs + one capability grant). */
type GrantedManifest = {
  psk_hex: string;
  granted_service: string;
  provider: {
    seed_hex: string;
    org_id_hex: string;
    authority_dir: string;
    grant_path: string;
    grant_secret_path: string;
  };
  caller: {
    seed_hex: string;
    org_id_hex: string;
    authority_dir: string;
    membership_path: string;
    dispatcher_path: string;
    grant_path: string;
    grant_secret_path: string;
  };
};

/**
 * The five verified `OrgCaller` facts expected at the handler. Entity ids
 * come from the LIVE meshes (what `EntityKeypair::from_bytes(seed)`
 * derives) or the manifest — never from anything the caller sent.
 */
type ExpectedCaller = {
  entity: string;
  actingOrg: string;
  providerOrg: string;
  provider: string;
  sameOrg: boolean;
};

// ---------------------------------------------------------------------------
// The projection witness oracle: the five admission-verified facts, exact.
// This assertion (and its siblings' identical names) is what the inverse
// receipt reddens when the projection reports anything but verified data.
// ---------------------------------------------------------------------------

function expectVerifiedCaller(
  c: OrgCaller | undefined,
  expected: ExpectedCaller,
): void {
  if (c === undefined) fail('the handler ran and captured its caller');
  const caller = c;
  expectEq('verified caller entity', hex(caller.entity), expected.entity);
  expectEq('verified acting org', hex(caller.actingOrg), expected.actingOrg);
  expectEq('verified provider org', hex(caller.providerOrg), expected.providerOrg);
  expectEq('verified provider entity', hex(caller.provider), expected.provider);
  check(
    caller.isSameOrg === expected.sameOrg,
    'the verified same-org relation',
    `isSameOrg=${String(caller.isSameOrg)}`,
  );
  check(
    caller.capability.length === 32,
    'the invoked capability id is 32 raw bytes',
    `length=${caller.capability.length}`,
  );
}

/**
 * The org seam's request stream carries no routing metadata — its
 * accessors report their documented EMPTY values, so attribution can
 * only ride the verified `OrgCaller` ("never origin").
 */
function expectEmptyStreamMetadata(reqs: TypedRequestStream<unknown> | undefined): void {
  if (reqs === undefined) fail('the handler received a request stream');
  const stream = reqs;
  check(
    stream.callerOrigin === 0n,
    'attribution rides the verified OrgCaller, never the stream origin',
    `callerOrigin=${stream.callerOrigin}`,
  );
  check(
    stream.callId === 0n,
    'the org seam carries no stream callId (documented empty)',
    `callId=${stream.callId}`,
  );
  check(
    stream.deadlineNs === 0n,
    'the org seam declares no stream deadline (documented empty)',
    `deadlineNs=${stream.deadlineNs}`,
  );
  check(
    stream.headers.length === 0,
    'the org seam carries no stream headers (documented empty)',
    `headers=${stream.headers.length}`,
  );
}

// ---------------------------------------------------------------------------
// Live two-mesh rig.
// ---------------------------------------------------------------------------

async function meshFromSeed(seedHex: string, pskHex: string): Promise<MeshNode> {
  return MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: pskHex,
    identitySeed: Buffer.from(seedHex, 'hex'),
  });
}

/** The a2a handshake: acceptor waits for the connector while it dials. */
async function handshake(connector: MeshNode, acceptor: MeshNode): Promise<void> {
  const accepted = acceptor.accept(connector.nodeId());
  await sleep(50);
  await connector.connect(acceptor.localAddr(), acceptor.publicKey(), acceptor.nodeId());
  await accepted;
}

/**
 * Scoped discovery is announcement-throttled — force an emission per
 * cycle and retry the attempt. Each retry is a FRESH call: a signed
 * proof binds one call id, so the facade never retries underneath.
 */
async function converge<T>(
  provider: MeshNode,
  caller: MeshNode,
  attempt: () => Promise<T>,
  budgetMs = 45_000,
): Promise<T> {
  const deadline = Date.now() + budgetMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    await Promise.all([
      provider.announceCapabilities({}),
      caller.announceCapabilities({}),
    ]).catch(() => {});
    try {
      return await attempt();
    } catch (e) {
      lastErr = e;
      await sleep(1000);
    }
  }
  fail('the protected call never converged', msg(lastErr));
}

type Rig = {
  provider: MeshNode;
  caller: MeshNode;
  client: OrgClient;
  handles: OrgServeHandle[];
  expected: ExpectedCaller;
  /** The service name for one shape (granted has exactly one). */
  svc: string;
};

type RigRunner = (rig: Rig) => Promise<void>;

/**
 * Same-org rig: two adopted node authorities in ONE org sharing the
 * owner audience. `strictTeardown` asserts the documented
 * `client.close() → handle.close() → mesh.shutdown()` teardown STRICTLY
 * (the failure mode is a rejected shutdown: "outstanding references
 * exist").
 */
async function withSameOrg(
  dir: string,
  serviceSuffix: string,
  run: RigRunner,
  strictTeardown = false,
): Promise<void> {
  const manifest = JSON.parse(
    readFileSync(join(dir, 'manifest.json'), 'utf8'),
  ) as SameOrgManifest;
  const provider = await meshFromSeed(manifest.provider.seed_hex, manifest.psk_hex);
  const caller = await meshFromSeed(manifest.caller.seed_hex, manifest.psk_hex);
  const handles: OrgServeHandle[] = [];
  let client: OrgClient | undefined;
  try {
    installOrgAuthority(provider, join(dir, manifest.provider.authority_dir));
    installOrgAuthority(caller, join(dir, manifest.caller.authority_dir));
    await handshake(caller, provider);
    await provider.start();
    await caller.start();
    client = OrgClient.bind(
      caller,
      OrgCredentials.create({
        membership: readFileSync(join(dir, manifest.caller.membership_path)),
        dispatcher: readFileSync(join(dir, manifest.caller.dispatcher_path)),
        grants: [],
        audienceSecretPaths: [],
      }),
    );
    await run({
      provider,
      caller,
      client,
      handles,
      svc: `q6.tssdk.${serviceSuffix}`,
      expected: {
        entity: manifest.caller.entity_id_hex,
        actingOrg: manifest.caller.org_id_hex,
        providerOrg: manifest.provider.org_id_hex,
        provider: manifest.provider.entity_id_hex,
        sameOrg: true,
      },
    });
  } finally {
    await teardown(provider, caller, client, handles, strictTeardown);
  }
}

/**
 * Granted rig: two orgs joined by ONE capability grant (the
 * `gen_org_scenario` issuance chain). The grant covers exactly
 * `nrpc:<granted_service>`, so every granted shape registers its serve
 * on that one name inside its own process. Attribution expectations are
 * computed from the LIVE meshes (the org ids come from the manifest).
 */
async function withGranted(
  dir: string,
  run: RigRunner,
  strictTeardown = false,
): Promise<void> {
  const manifest = JSON.parse(
    readFileSync(join(dir, 'manifest.json'), 'utf8'),
  ) as GrantedManifest;
  const provider = await meshFromSeed(manifest.provider.seed_hex, manifest.psk_hex);
  const caller = await meshFromSeed(manifest.caller.seed_hex, manifest.psk_hex);
  const handles: OrgServeHandle[] = [];
  let client: OrgClient | undefined;
  try {
    installOrgAuthority(provider, join(dir, manifest.provider.authority_dir));
    installOrgAuthority(caller, join(dir, manifest.caller.authority_dir));
    await handshake(caller, provider);
    await provider.start();
    await caller.start();
    // Registration before OR after the audience is the substrate's
    // contract — installing first is discoverable immediately.
    installProviderGrantAudience(
      provider,
      readFileSync(join(dir, manifest.provider.grant_path)),
      join(dir, manifest.provider.grant_secret_path),
    );
    client = OrgClient.bind(
      caller,
      OrgCredentials.create({
        membership: readFileSync(join(dir, manifest.caller.membership_path)),
        dispatcher: readFileSync(join(dir, manifest.caller.dispatcher_path)),
        grants: [readFileSync(join(dir, manifest.caller.grant_path))],
        audienceSecretPaths: [join(dir, manifest.caller.grant_secret_path)],
      }),
    );
    await run({
      provider,
      caller,
      client,
      handles,
      svc: manifest.granted_service,
      expected: {
        entity: hex(caller.entityId()),
        actingOrg: manifest.caller.org_id_hex,
        providerOrg: manifest.provider.org_id_hex,
        provider: hex(provider.entityId()),
        sameOrg: false,
      },
    });
  } finally {
    await teardown(provider, caller, client, handles, strictTeardown);
  }
}

/** The documented teardown order; strict or best-effort per `strict`. */
async function teardown(
  provider: MeshNode,
  caller: MeshNode,
  client: OrgClient | undefined,
  handles: OrgServeHandle[],
  strict: boolean,
): Promise<void> {
  try {
    client?.close();
  } catch {
    /* idempotent */
  }
  for (const h of handles) {
    try {
      h.close();
    } catch {
      /* idempotent */
    }
  }
  if (strict) {
    try {
      await provider.shutdown();
    } catch (e) {
      fail('the documented teardown releases every reference (provider)', msg(e));
    }
    try {
      await caller.shutdown();
    } catch (e) {
      fail('the documented teardown releases every reference (caller)', msg(e));
    }
  } else {
    await provider.shutdown().catch(() => {});
    await caller.shutdown().catch(() => {});
  }
}

// ---------------------------------------------------------------------------
// Scenario rows — per shape × authority mode, each call AND serve.
// ---------------------------------------------------------------------------

// -- same-org ---------------------------------------------------------------

async function sameOrgUnary(dir: string): Promise<void> {
  await withSameOrg(dir, 'unary', async (rig) => {
    let attr: OrgCaller | undefined;
    const handle = serveOrg<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.SameOrg,
      (c, req) => {
        attr = c;
        return { n: req.n + 1 };
      },
    );
    rig.handles.push(handle);
    const reply = await converge(rig.provider, rig.caller, () =>
      rig.client.call<{ n: number }, { n: number }>(rig.svc, { n: 7 }),
    );
    expectEq('the preserved unary response payload', JSON.stringify(reply), '{"n":8}');
    expectVerifiedCaller(attr, rig.expected);
  });
}

async function sameOrgStreaming(dir: string): Promise<void> {
  await withSameOrg(dir, 'ss', async (rig) => {
    let attr: OrgCaller | undefined;
    const handle = serveOrgStreaming<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.SameOrg,
      (c, req, sink) => {
        attr = c;
        sink.send({ n: req.n + 1 });
        sink.send({ n: req.n + 2 });
      },
    );
    rig.handles.push(handle);
    const stream = await converge(rig.provider, rig.caller, () =>
      rig.client.callStreaming<{ n: number }, { n: number }>(rig.svc, { n: 7 }),
    );
    check(
      stream instanceof TypedRpcStream,
      'the SDK reuses the existing typed stream class (no new stream wrapper)',
    );
    const out: { n: number }[] = [];
    for await (const item of stream) out.push(item);
    expectEq('exact streamed payloads', JSON.stringify(out), '[{"n":8},{"n":9}]');
    expectVerifiedCaller(attr, rig.expected);
  });
}

async function sameOrgClientStream(dir: string): Promise<void> {
  await withSameOrg(dir, 'cs', async (rig) => {
    let attr: OrgCaller | undefined;
    let reqs: TypedRequestStream<{ n: number }> | undefined;
    const handle = serveOrgClientStream<{ n: number }, { count: number; total: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.SameOrg,
      async (c, requests) => {
        attr = c;
        reqs = requests;
        let count = 0;
        let total = 0;
        for await (const item of requests) {
          count += 1;
          total += item.n;
        }
        return { count, total };
      },
    );
    rig.handles.push(handle);
    const summary = await converge(rig.provider, rig.caller, async () => {
      const call = await rig.client.callClientStream<{ n: number }, { count: number; total: number }>(
        rig.svc,
      );
      check(
        call instanceof TypedClientStreamCall,
        'the SDK reuses the existing typed client-stream class (no new stream wrapper)',
      );
      await call.send({ n: 10 });
      await call.send({ n: 20 });
      return await call.finish();
    });
    expectEq('exact terminal summary', JSON.stringify(summary), '{"count":2,"total":30}');
    expectVerifiedCaller(attr, rig.expected);
    expectEmptyStreamMetadata(reqs);
  });
}

async function sameOrgDuplex(dir: string): Promise<void> {
  await withSameOrg(dir, 'dx', async (rig) => {
    let attr: OrgCaller | undefined;
    let reqs: TypedRequestStream<{ n: number }> | undefined;
    const handle = serveOrgDuplex<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.SameOrg,
      async (c, requests, sink) => {
        attr = c;
        reqs = requests;
        for await (const item of requests) {
          sink.send({ n: item.n * 10 });
        }
      },
    );
    rig.handles.push(handle);
    const echoes = await converge(rig.provider, rig.caller, async () => {
      const [sink, stream] = await rig.client.callDuplex<{ n: number }, { n: number }>(rig.svc);
      check(
        sink instanceof TypedDuplexSink && stream instanceof TypedDuplexStream,
        'the SDK reuses the existing typed duplex halves (no new stream wrapper)',
      );
      await sink.send({ n: 1 });
      await sink.send({ n: 2 });
      await sink.finish();
      const out: { n: number }[] = [];
      for await (const item of stream) out.push(item);
      return out;
    });
    expectEq('exact duplex echoes', JSON.stringify(echoes), '[{"n":10},{"n":20}]');
    expectVerifiedCaller(attr, rig.expected);
    expectEmptyStreamMetadata(reqs);
  });
}

// -- granted ----------------------------------------------------------------

async function grantedUnary(dir: string): Promise<void> {
  await withGranted(dir, async (rig) => {
    let attr: OrgCaller | undefined;
    const handle = serveOrg<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.Granted,
      (c, req) => {
        attr = c;
        return { n: req.n + 1 };
      },
    );
    rig.handles.push(handle);
    const reply = await converge(rig.provider, rig.caller, () =>
      rig.client.call<{ n: number }, { n: number }>(rig.svc, { n: 7 }),
    );
    expectEq('the preserved unary response payload', JSON.stringify(reply), '{"n":8}');
    expectVerifiedCaller(attr, rig.expected);
  });
}

async function grantedStreaming(dir: string): Promise<void> {
  await withGranted(dir, async (rig) => {
    let attr: OrgCaller | undefined;
    const handle = serveOrgStreaming<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.Granted,
      (c, req, sink) => {
        attr = c;
        sink.send({ n: req.n + 1 });
        sink.send({ n: req.n + 2 });
      },
    );
    rig.handles.push(handle);
    const out = await converge(rig.provider, rig.caller, async () => {
      const stream = await rig.client.callStreaming<{ n: number }, { n: number }>(rig.svc, {
        n: 7,
      });
      check(
        stream instanceof TypedRpcStream,
        'the SDK reuses the existing typed stream class (no new stream wrapper)',
      );
      const items: { n: number }[] = [];
      for await (const item of stream) items.push(item);
      return items;
    });
    expectEq('exact streamed payloads', JSON.stringify(out), '[{"n":8},{"n":9}]');
    expectVerifiedCaller(attr, rig.expected);
  });
}

async function grantedClientStream(dir: string): Promise<void> {
  await withGranted(dir, async (rig) => {
    let attr: OrgCaller | undefined;
    let reqs: TypedRequestStream<{ n: number }> | undefined;
    const handle = serveOrgClientStream<{ n: number }, { count: number; total: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.Granted,
      async (c, requests) => {
        attr = c;
        reqs = requests;
        let count = 0;
        let total = 0;
        for await (const item of requests) {
          count += 1;
          total += item.n;
        }
        return { count, total };
      },
    );
    rig.handles.push(handle);
    const summary = await converge(rig.provider, rig.caller, async () => {
      const call = await rig.client.callClientStream<{ n: number }, { count: number; total: number }>(
        rig.svc,
      );
      await call.send({ n: 10 });
      await call.send({ n: 20 });
      return await call.finish();
    });
    expectEq('exact terminal summary', JSON.stringify(summary), '{"count":2,"total":30}');
    expectVerifiedCaller(attr, rig.expected);
    expectEmptyStreamMetadata(reqs);
  });
}

async function grantedDuplex(dir: string): Promise<void> {
  await withGranted(dir, async (rig) => {
    let attr: OrgCaller | undefined;
    let reqs: TypedRequestStream<{ n: number }> | undefined;
    const handle = serveOrgDuplex<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.Granted,
      async (c, requests, sink) => {
        attr = c;
        reqs = requests;
        for await (const item of requests) {
          sink.send({ n: item.n * 10 });
        }
      },
    );
    rig.handles.push(handle);
    const echoes = await converge(rig.provider, rig.caller, async () => {
      const [sink, stream] = await rig.client.callDuplex<{ n: number }, { n: number }>(rig.svc);
      await sink.send({ n: 1 });
      await sink.send({ n: 2 });
      await sink.finish();
      const out: { n: number }[] = [];
      for await (const item of stream) out.push(item);
      return out;
    });
    expectEq('exact duplex echoes', JSON.stringify(echoes), '[{"n":10},{"n":20}]');
    expectVerifiedCaller(attr, rig.expected);
    expectEmptyStreamMetadata(reqs);
  });
}

// -- midstream vocabulary + strict disposal ---------------------------------

async function midstreamOrgError(dir: string): Promise<void> {
  await withSameOrg(dir, 'mid', async (rig) => {
    const handle = serveOrgStreaming<{ n: number }, { n: number }>(
      rig.provider,
      rig.svc,
      OrgAccess.SameOrg,
      (_c, req, sink) => {
        sink.send({ n: req.n + 1 });
        throw new Error('q6 midstream handler rejection');
      },
    );
    rig.handles.push(handle);
    const stream = await converge(rig.provider, rig.caller, () =>
      rig.client.callStreaming<{ n: number }, { n: number }>(rig.svc, { n: 1 }),
    );
    expectEq(
      'the live item before the terminal error',
      JSON.stringify(await stream.next()),
      '{"n":2}',
    );
    let thrown: unknown;
    let resolved: { n: number } | null | undefined;
    try {
      resolved = await stream.next();
    } catch (e) {
      thrown = e;
    }
    check(
      resolved === undefined && thrown !== undefined,
      'a midstream terminal error is an OrgError, never a false clean EOF',
      resolved === null
        ? 'next() resolved null — the terminal error became a false clean EOF'
        : `next() resolved ${JSON.stringify(resolved)}`,
    );
    if (!(thrown instanceof OrgError)) {
      fail('midstream errors route through classifyOrgError', `got ${msg(thrown)}`);
    }
    const err = thrown;
    expectEq('the frozen rpc domain', err.domain, 'rpc');
    expectEq('a handler rejection is the server_error kind', err.kind, 'server_error');
    // The mirror at the wrapper level round-trips: classifying the
    // surfaced error again is stable (same domain/kind).
    const again = classifyOrgError(thrown);
    check(
      again instanceof OrgError && again.domain === err.domain && again.kind === err.kind,
      'the classifyOrgError mirror is stable at the wrapper level',
      `classify output ${msg(again)} != ${err.domain}:${err.kind}`,
    );
  });
}

async function strictDisposal(dir: string): Promise<void> {
  await withSameOrg(
    dir,
    'dispose',
    async (rig) => {
      const handle = serveOrgStreaming<{ n: number }, { n: number }>(
        rig.provider,
        rig.svc,
        OrgAccess.SameOrg,
        (_c, req, sink) => {
          sink.send({ n: req.n + 1 });
        },
      );
      rig.handles.push(handle);
      const out = await converge(rig.provider, rig.caller, async () => {
        const stream = await rig.client.callStreaming<{ n: number }, { n: number }>(rig.svc, {
          n: 41,
        });
        const items: { n: number }[] = [];
        for await (const item of stream) items.push(item);
        return items;
      });
      expectEq('the call completed before teardown', JSON.stringify(out), '[{"n":42}]');
    },
    true,
  );
}

// ---------------------------------------------------------------------------
// Dispatch.
// ---------------------------------------------------------------------------

const SCENARIOS: Record<string, (dir: string) => Promise<void>> = {
  same_org_unary_call_and_serve: sameOrgUnary,
  same_org_streaming_call_and_serve: sameOrgStreaming,
  same_org_client_stream_call_and_serve: sameOrgClientStream,
  same_org_duplex_call_and_serve: sameOrgDuplex,
  granted_unary_call_and_serve: grantedUnary,
  granted_streaming_call_and_serve: grantedStreaming,
  granted_client_stream_call_and_serve: grantedClientStream,
  granted_duplex_call_and_serve: grantedDuplex,
  midstream_outcomes_surface_an_org_error_never_a_false_clean_eof: midstreamOrgError,
  closing_client_and_serve_handle_lets_both_meshes_shut_down_cleanly: strictDisposal,
};

async function main(): Promise<void> {
  const scenario: string | undefined = process.argv[2];
  const fixtureDir: string | undefined = process.argv[3];
  const run = scenario === undefined ? undefined : SCENARIOS[scenario];
  if (!run || fixtureDir === undefined) {
    fail(
      'usage: org_streaming_consumer.js <scenario> <fixtureDir>',
      `scenarios: ${Object.keys(SCENARIOS).join(', ')}`,
    );
  }
  await run(fixtureDir);
  process.stdout.write(`OK ${scenario}\n`);
}

main().then(
  () => process.exit(0),
  (e: unknown) => {
    process.stderr.write(`FAILED ${msg(e)}\n`);
    process.exit(1);
  },
);
