// The trust surfaces through @net-mesh/sdk (NODE_SDK_GAPS_PLAN.md S3):
// consent / pins, delegation, enrollment. Until S3 none of these was
// exported by the SDK, so a TS caller imported them from @net-mesh/core,
// outside the supported surface.
//
// Every import here is from the SDK root (`../src/index`), never from
// @net-mesh/core: that is the witness. The behaviour itself is covered by the
// binding's own tests; these prove the SDK path reaches it. The suite does NOT
// skip when a native family is missing: CI's sdk-ts build compiles these
// features, so an absent one is a failure, not a silent no-op.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, describe, expect, it } from 'vitest';

import * as sdk from '../src/index';

const {
  ConsentPolicy,
  DelegationChain,
  Identity,
  InviteToken,
  JoinRequest,
  OperatorEnrollment,
  PinStore,
  RevocationRegistry,
  deriveChildIdentity,
  fingerprint,
  isDelegationError,
  isEnrollmentError,
} = sdk;

const tmp = mkdtempSync(join(tmpdir(), 'net-sdk-trust-'));
let seq = 0;
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

describe('trust surfaces are exported from the SDK root', () => {
  const VALUES = [
    'CapabilityGateway',
    'CapabilityId',
    'ConsentPolicy',
    'PinStore',
    'credentialRequiresConsent',
    'DelegationChain',
    'GATEWAY_DELEGATION_CHANNEL',
    'RevocationRegistry',
    'defaultRevocationStorePath',
    'deriveChildIdentity',
    'isDelegationError',
    'DeviceEnrollment',
    'DeviceRecord',
    'EnrollmentServeHandle',
    'InviteToken',
    'JoinOutcome',
    'JoinRequest',
    'OperatorEnrollment',
    'fingerprint',
    'isEnrollmentError',
  ] as const;

  it.each(VALUES)('%s is defined (the native family is compiled in)', (name) => {
    expect((sdk as Record<string, unknown>)[name], name).toBeDefined();
  });
});

describe('consent', () => {
  it('a pin requested through the SDK is pending until approved, then read back', async () => {
    const store = new PinStore(join(tmp, `pins-${seq++}.json`));
    expect(await store.request('acme/search')).toBe('pending');
    expect(await store.isApproved('acme/search')).toBe(false);
    expect(await store.approve('acme/search')).toBe(true);
    expect(await store.isApproved('acme/search')).toBe(true);
    expect(await store.approved()).toEqual(['acme/search']);
  });

  it('a policy gates a capability until it is pinned', () => {
    const policy = new ConsentPolicy();
    expect(policy.requiresApproval('acme/search', 'none')).toBe(true);
    policy.pin('acme/search');
    expect(policy.isPinned('acme/search')).toBe(true);
  });
});

describe('delegation', () => {
  it('a derived gateway chain verifies, and dies when its machine is revoked', () => {
    const root = Identity.generate().toNapi();
    const machine = deriveChildIdentity(root, 'machine:hostA');
    const gateway = deriveChildIdentity(root, 'gateway:hostA:agent');
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600);
    const registry = new RevocationRegistry();

    expect(chain.verify(gateway.entityId, root.entityId, registry)).toBe(true);
    registry.revoke(machine.entityId);
    expect(chain.verify(gateway.entityId, root.entityId, registry)).toBe(false);
  });

  it('errors come in two documented families', () => {
    const thrown = (fn: () => unknown): unknown => {
      try {
        fn();
      } catch (err) {
        return err;
      }
      throw new Error('expected a throw');
    };
    // A bad argument: the `delegation: ` family.
    const argument = thrown(() => new RevocationRegistry().revokeBelow(Buffer.alloc(3), 1));
    expect(isDelegationError(argument)).toBe(true);
    expect(isEnrollmentError(argument)).toBe(false);
    // A malformed chain: the token taxonomy, not `delegation: `.
    const chain = thrown(() => DelegationChain.fromBytes(Buffer.from('not a chain')));
    expect((chain as Error).message).toBe('token: invalid_format');
    expect(isDelegationError(chain)).toBe(false);
  });
});

describe('enrollment', () => {
  function operator(root = Identity.generate().toNapi()) {
    const dir = join(tmp, `op-${seq++}`);
    return {
      root,
      op: new OperatorEnrollment(root, join(dir, 'devices.json'), join(dir, 'revocations.json')),
    };
  }

  it('invite → join → approve mints a chain rooted at the operator', async () => {
    const { op, root } = operator();
    const invite = op.invite('relay://rv', 300);
    const parsed = InviteToken.decode(invite.encode());
    expect(parsed.rootFingerprint()).toBe(fingerprint(root.entityId));

    const device = Identity.generate().toNapi();
    const request = JoinRequest.create(device, 'pc', ['region:office'], parsed);
    expect(request.verifySelfSignature()).toBe(true);

    const chain = await op.approve(request, 3600);
    expect(chain.leaf.equals(device.entityId)).toBe(true);
    expect(chain.root.equals(root.entityId)).toBe(true);
    expect((await op.devices()).map((d) => d.name)).toEqual(['pc']);
  });

  it('a malformed invite is a classified enrollment error', () => {
    let caught: unknown;
    try {
      InviteToken.decode('net-invite:garbage');
    } catch (err) {
      caught = err;
    }
    expect(isEnrollmentError(caught)).toBe(true);
  });
});
