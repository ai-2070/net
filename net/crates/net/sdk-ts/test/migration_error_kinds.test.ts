// MigrationError kinds (NODE_SDK_GAPS_PLAN.md S6).
//
// The napi binding formats every migration failure as
// `migration: <kind>[: <detail>]`; `parseMigrationError` turns that into a
// typed MigrationError. Three kinds the core emits (`no-target-available`,
// `buffer-full`, `wrong-peer`) were missing, so a caller saw them as
// `'unknown'`. The drift check below reads the kinds straight out of the
// binding's formatter, so a new Rust variant fails here instead of
// degrading silently.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { describe, expect, it } from 'vitest';

import { MigrationError, parseMigrationError } from '../src/compute';

const parse = (body: string) => parseMigrationError(body, body);

describe('MigrationError kinds', () => {
  it('every kind the napi formatter emits parses to itself, not unknown', () => {
    const src = readFileSync(join(__dirname, '../../bindings/node/src/compute.rs'), 'utf8');
    const kinds = [...new Set([...src.matchAll(/"migration: ([a-z-]+)/g)].map((m) => m[1]))];
    expect(kinds.length).toBeGreaterThanOrEqual(14);
    for (const kind of kinds) {
      expect(parse(`migration: ${kind}`).kind, kind).toBe(kind);
    }
  });

  it('no-target-available', () => {
    const err = parse('migration: no-target-available');
    expect(err).toBeInstanceOf(MigrationError);
    expect(err.kind).toBe('no-target-available');
  });

  it('buffer-full carries events and bytes', () => {
    const err = parse('migration: buffer-full: 4096 events / 67108864 bytes');
    expect(err.kind).toBe('buffer-full');
    expect(err.events).toBe(4096);
    expect(err.bytes).toBe(67108864);
  });

  it('wrong-peer carries the origin and both nodes as exact u64s', () => {
    const err = parse(
      'migration: wrong-peer: 0xfedcba9876543210: from=0xffffffffffffffff expected=0x1',
    );
    expect(err.kind).toBe('wrong-peer');
    expect(err.originHash).toBe(0xfedcba9876543210n);
    expect(err.from).toBe(0xffffffffffffffffn);
    expect(err.expected).toBe(1n);
  });

  it('originHash is an exact bigint, comparable with DaemonHandle.originHash', () => {
    // Above 2^53: the old `number` parse rounded this to ...3200.
    const err = parse('migration: daemon-not-found: 0xfedcba9876543210');
    expect(err.originHash).toBe(0xfedcba9876543210n);
    expect(parse('migration: already-migrating: 0x2a').originHash).toBe(42n);
  });

  it('an unrecognised kind is unknown, with the raw text kept', () => {
    const err = parse('migration: from-the-future: x');
    expect(err.kind).toBe('unknown');
    expect(err.message).toBe('migration: from-the-future: x');
  });
});
