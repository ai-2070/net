// Paid A2A through the SDK (`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md`
// D7 / WS-E): the factories adapt a `MeshNode` (or a native `NetMesh`) to
// the native `PaymentProvider` / `CapabilityGateway` and return the native
// objects. These tests import the SDK only — the point is that an SDK user
// never needs `@net-mesh/core` for paid A2A — except where the native arm of
// the adaptation is itself under test.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, describe, expect, it } from 'vitest';

import * as sdk from '../src/index';
import {
  A2aInvalidArgumentError,
  MeshNode,
  a2aDocument,
  a2aU64,
  classifyError,
  createCapabilityGateway,
  createPaymentProvider,
  setA2aOrgCaller,
} from '../src/index';

const PSK = '6e'.repeat(32);
const tmp = mkdtempSync(join(tmpdir(), 'net-sdk-paid-a2a-'));
let seq = 0;
afterAll(() => rmSync(tmp, { recursive: true, force: true }));
const path = (name: string) => join(tmp, `${seq++}-${name}`);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const MOCK_REQS = JSON.stringify([
  {
    scheme: 'mock',
    network: 'mock:net',
    amount: '2500',
    asset: 'musd',
    payTo: 'mock-provider-settle-addr',
    maxTimeoutSeconds: 60,
  },
]);

function offer(pricingTerms?: string) {
  return {
    revision: 'r1',
    pricingTerms,
    bounds: { maxPromptBytes: 1024n, maxContextRefs: 8n, maxTags: 8n, maxTagBytes: 64n, maxInFlight: 4n },
    reservationTtlSecs: 600n,
    reservationRetentionSecs: 604800n,
    retentionSecs: 3600n,
  };
}

const node = () => MeshNode.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true });

async function connectedPair(): Promise<[MeshNode, MeshNode]> {
  const a = await node();
  const b = await node();
  await Promise.all([
    b.accept(a.nodeId()),
    (async () => {
      await sleep(50);
      await a.connect(b.localAddr(), b.publicKey(), b.nodeId());
    })(),
  ]);
  await a.start();
  await b.start();
  return [a, b];
}

async function rejectionOf(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
  } catch (e) {
    return e;
  }
  throw new Error('expected a rejection');
}

describe('paid A2A is exported from the SDK root', () => {
  const VALUES = [
    'createPaymentProvider',
    'createCapabilityGateway',
    'setA2aOrgCaller',
    'a2aDocument',
    'a2aU64',
    'classifyError',
    'PaymentRefusedError',
    'JournalOwnedElsewhereError',
    'A2aInvalidArgumentError',
  ] as const;

  it.each(VALUES)('%s is defined', (name) => {
    expect((sdk as Record<string, unknown>)[name], name).toBeDefined();
  });
});

describe('paid A2A over MeshNode', () => {
  it('a provider and a caller built from MeshNodes run a paid task once, then shut down cleanly', async () => {
    const [caller, providerNode] = await connectedPair();
    const provider = createPaymentProvider(providerNode, {
      statePath: path('engine.json'),
      billingLogPath: path('billing.jsonl'),
      unsafeDevMockFacilitator: true,
    });
    const ran: string[] = [];
    const terms = await provider.pricingTerms(
      `${providerNode.nodeId()}/net.a2a.task/summarize`,
      MOCK_REQS,
    );
    const handle = await provider.serveA2aConfigured(
      async (brief: { taskId: string }) => {
        ran.push(brief.taskId);
        return `blob://${brief.taskId}`;
      },
      { summarize: offer(terms) },
      path('journal.json'),
    );
    const gateway = createCapabilityGateway(caller, {
      paymentPolicyPath: path('spend-policy.json'),
      paymentProfile: 'dev_test',
      a2aPurchasePath: path('a2a-purchases.json'),
    });
    try {
      let env = '';
      for (let i = 0; i < 8; i++) {
        env = await gateway.prepareTask(providerNode.nodeId(), 'summarize', 'sdk paid', [], [], 'sdk-1');
        if (JSON.parse(env).status !== 'busy') break;
        await sleep(100);
      }
      expect(JSON.parse(env).status, env).toBe('ok');
      const prepared = a2aDocument(env, '/prepared');
      expect(a2aU64(prepared, '/provider_node')).toBe(providerNode.nodeId());
      expect(JSON.parse(await gateway.purchaseTask(prepared)).status).toBe('paid');
      expect(JSON.parse(await gateway.submitTask(prepared)).status).toBe('accepted');

      const deadline = Date.now() + 8000;
      let state = '';
      while (Date.now() < deadline && state !== 'completed') {
        const raw = await caller.taskStatus(providerNode.nodeId(), 'sdk-1');
        state = raw === null ? '' : JSON.parse(raw).state.state;
        if (state !== 'completed') await sleep(50);
      }
      expect(state).toBe('completed');
      expect(ran).toEqual(['sdk-1']);

      // The forwards on MeshNode reach the same native verbs.
      const offers = JSON.parse(await caller.describeA2a(providerNode.nodeId()));
      expect(offers.map((o: { service_id: string }) => o.service_id)).toEqual(['summarize']);

      // Errors are the native ones until classified; the SDK's classes are
      // the native classes.
      const refused = await rejectionOf(gateway.purchaseTask('{}'));
      expect(classifyError(refused)).toBeInstanceOf(A2aInvalidArgumentError);
    } finally {
      handle.stop();
      provider.close();
      gateway.close();
    }
    // The factories retained nothing beyond the native objects' documented
    // references: once those are released, both nodes shut down.
    await caller.shutdown();
    await providerNode.shutdown();
  }, 60000);

  it('a native NetMesh is accepted by the same factories', async () => {
    const { NetMesh } = await import('@net-mesh/core');
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true });
    await mesh.start();
    const provider = createPaymentProvider(mesh, {
      statePath: path('engine.json'),
      unsafeDevMockFacilitator: true,
    });
    const gateway = createCapabilityGateway(mesh, { pinStorePath: path('pins.json') });
    expect(provider.registryVersion).toBe('net-default-1');
    expect(gateway.pinStorePath).toMatch(/pins\.json$/);
    provider.close();
    gateway.close();
    await mesh.shutdown();
  }, 30000);

  // Each option reaches the native behavior it names, so a mis-ordered
  // mapping fails here (the compile-time arity guard catches a missing one).
  it('every option reaches the native argument it names', async () => {
    const n = await node();
    await n.start();
    try {
      expect(() => createPaymentProvider(n, { statePath: path('e.json') })).toThrow(
        /no settlement backend configured/,
      );
      expect(() =>
        createPaymentProvider(n, {
          statePath: path('e.json'),
          facilitatorUrl: 'https://facilitator.example.com',
          unsafeDevMockFacilitator: true,
        }),
      ).toThrow(/not both/);
      const noLog = createPaymentProvider(n, { statePath: path('e.json'), unsafeDevMockFacilitator: true });
      await expect(noLog.readBilling()).rejects.toThrow(/no billing log configured/);
      noLog.close();
      const withLog = createPaymentProvider(n, {
        statePath: path('e.json'),
        billingLogPath: path('b.jsonl'),
        unsafeDevMockFacilitator: true,
      });
      expect(await withLog.readBilling()).toEqual([]);
      withLog.close();

      const pins = path('pins.json');
      const g = createCapabilityGateway(n, { pinStorePath: pins });
      expect(g.pinStorePath).toBe(pins);
      g.close();
      expect(() => createCapabilityGateway(n, { paymentProfile: 'dev_test' })).toThrow(
        /require paymentPolicyPath/,
      );
      expect(() => createCapabilityGateway(n, { paymentUnsafeMockAutoAllow: true })).toThrow(
        /require paymentPolicyPath/,
      );
      expect(() =>
        createCapabilityGateway(n, { paymentPolicyPath: path('p.json'), paymentProfile: 'no-such-profile' }),
      ).toThrow();
      expect(() => createCapabilityGateway(n, { a2aPurchasePath: path('a.json') })).toThrow(
        /a2aPurchasePath requires paymentPolicyPath/,
      );
      expect(() =>
        createCapabilityGateway(n, {
          paymentPolicyPath: path('p.json'),
          paymentSignerSvm: { address: 'svm-payer', sign: undefined as never },
        }),
      ).toThrow(/paymentSignerSvm and paymentSignerSvmAddress must be provided together/);
      expect(() =>
        createCapabilityGateway(n, {
          paymentPolicyPath: path('p.json'),
          paymentSignerXrpl: { address: 'xrpl-payer', sign: undefined as never },
        }),
      ).toThrow(/paymentSignerXrpl and paymentSignerXrplAddress must be provided together/);
      expect(() =>
        createCapabilityGateway(n, {
          paymentPolicyPath: path('p.json'),
          paymentSigner: { address: '0xpayer', sign: undefined as never },
        }),
      ).toThrow(/paymentSigner and paymentSignerAddress must be provided together/);
      // a2aPurchasePath lands where the paid verbs look for it.
      const paid = createCapabilityGateway(n, {
        paymentPolicyPath: path('p.json'),
        paymentProfile: 'dev_test',
        a2aPurchasePath: path('a.json'),
      });
      expect(await paid.a2aAttempts()).toBe('[]');
      paid.close();
    } finally {
      await n.shutdown();
    }
  }, 30000);

  it('setA2aOrgCaller picks the slot by target and accepts null', async () => {
    const n = await node();
    await n.start();
    const gateway = createCapabilityGateway(n, {
      paymentPolicyPath: path('p.json'),
      a2aPurchasePath: path('a.json'),
    });
    try {
      expect(() => setA2aOrgCaller(n, null)).not.toThrow();
      expect(() => setA2aOrgCaller(gateway, null)).not.toThrow();
      expect(() => n.setA2aOrgCaller(null)).not.toThrow();
    } finally {
      gateway.close();
      await n.shutdown();
    }
  }, 30000);
});
