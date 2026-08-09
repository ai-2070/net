// Channel names must satisfy the canonical Net grammar at construction.
//
// `ChannelName::new` (Rust) is the only constructor for a distributed
// mesh channel name and has no `From<&str>` escape hatch. The ergonomic
// tagged-topic wrapper never reaches it — `TypedChannel.publish` embeds
// the name in generic EventBus JSON as `_channel` and calls
// `ingestFire` — so every invalid name used to be accepted here and
// diverge from the mesh only later.
//
// These are the inverse tests for each Rust boundary in
// `net/crates/net/src/adapter/net/channel/name.rs:108-156`.

import { describe, expect, it } from 'vitest';

import type { Net as NapiNet } from '@net-mesh/core';
import {
  ChannelNameError,
  MAX_CHANNEL_NAME_LEN,
  TypedChannel,
  validateChannelName,
} from '../src/channel';

// Construction never touches the bus.
const fakeBus = {} as NapiNet;

const VALID = [
  'sensors',
  'sensors/temperature',
  'sensors/lidar/front',
  'a',
  'a1',
  'with-dash',
  'with_underscore',
  'with.dot',
  'net.rpc.v1/call',
  '0',
  '..dots-but-not-a-segment',
  'a'.repeat(MAX_CHANNEL_NAME_LEN),
];

const INVALID: Array<[string, string]> = [
  ['', 'empty'],
  ['a'.repeat(MAX_CHANNEL_NAME_LEN + 1), 'too long'],
  // 256 bytes from 128 two-byte code points: the bound is bytes, not
  // UTF-16 units, so a 128-char name still fails.
  ['é'.repeat(128), 'too long (byte-counted)'],
  ['/leading', 'leading slash'],
  ['trailing/', 'trailing slash'],
  ['/', 'slash only'],
  ['bad//name', 'double slash'],
  ['Upper', 'uppercase'],
  ['sensors/Temp', 'uppercase in later segment'],
  ['has space', 'space'],
  ['has\ttab', 'tab'],
  ['has\nnewline', 'newline'],
  ['colon:name', 'colon'],
  ['star*', 'wildcard'],
  ['plus+name', 'plus'],
  ['hash#name', 'hash'],
  ['emoji\u{1f600}', 'non-ascii'],
  ['café', 'non-ascii letter'],
  ['.', 'dot segment'],
  ['..', 'dot-dot segment'],
  ['a/./b', 'interior dot segment'],
  ['a/../b', 'interior dot-dot segment'],
  ['../escape', 'leading traversal'],
  ['a/..', 'trailing traversal'],
];

describe('validateChannelName', () => {
  it.each(VALID)('accepts %j', (name) => {
    expect(validateChannelName(name)).toBe(name);
  });

  it.each(INVALID)('rejects %j (%s)', (name) => {
    expect(() => validateChannelName(name)).toThrow(ChannelNameError);
  });

  it('names the violation in the message', () => {
    expect(() => validateChannelName('Sensors/Temp')).toThrow(
      /lowercase only/
    );
    expect(() => validateChannelName('')).toThrow(/must not be empty/);
    expect(() => validateChannelName('a/../b')).toThrow(/reserved/);
  });
});

describe('TypedChannel constructor', () => {
  it.each(VALID)('accepts %j', (name) => {
    expect(new TypedChannel(fakeBus, name).name).toBe(name);
  });

  // Direct construction is covered, not just `NetNode.channel()`.
  it.each(INVALID)('rejects %j (%s)', (name) => {
    expect(() => new TypedChannel(fakeBus, name)).toThrow(ChannelNameError);
  });

  it('is reached through NetNode.channel()', async () => {
    const { NetNode } = await import('../src/node');
    // Bypass native construction: `channel()` only reads `this.bus`.
    const node = Object.create(NetNode.prototype) as NetNode;
    (node as unknown as { bus: NapiNet }).bus = fakeBus;

    expect(() => node.channel('Bad//Name')).toThrow(ChannelNameError);
    expect(node.channel('sensors/temperature').name).toBe(
      'sensors/temperature'
    );
  });
});
