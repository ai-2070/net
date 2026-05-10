// Cross-binding wire-format compat for the Capability-System
// Enhancements TS surface. Drives the same JSON fixtures the Rust
// integration test consumes (`tests/cross_lang_capability/`), so
// every supported binding agrees byte-for-byte on the predicate
// envelope and `CapabilitySet::diff` output.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import {
  CapabilitySetDiff,
  CapabilitySetWire,
  MetadataChange,
  Predicate,
  PredicateWire,
  RESERVED_PREFIXES,
  diffCapabilities,
  emptyCapabilities,
  evaluatePredicate,
  p,
  placementFilterFromFn,
  predicateFromRpcHeader,
  predicateFromWire,
  predicateToRpcHeader,
  predicateToWire,
  requireAxisValue,
  requireTag,
  RPC_WHERE_HEADER,
  standardPlacement,
  tagFromString,
  tagFromUserString,
  tagKey,
  tagToString,
  withMetadata,
} from '../src/capability-enhancements';

// ---------------------------------------------------------------------------
// Fixture loaders — paths are relative to the sdk-ts package root.
// ---------------------------------------------------------------------------

const PREDICATE_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/predicate_nrpc_envelope.json',
);
const DIFF_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/capability_set_diff.json',
);
const EVAL_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/predicate_eval.json',
);

function loadJson<T>(path: string, label: string): T {
  if (!existsSync(path)) {
    throw new Error(
      `${label} fixture missing at ${path}; cross-binding tests cannot run`,
    );
  }
  return JSON.parse(readFileSync(path, 'utf-8')) as T;
}

interface PredicateCase {
  name: string;
  summary: string;
  wire: PredicateWire;
}

interface PredicateFixture {
  description: string;
  header_name: string;
  abi_version_expected: number;
  cases: PredicateCase[];
}

interface DiffCase {
  name: string;
  summary: string;
  prev: CapabilitySetWire;
  curr: CapabilitySetWire;
  expected_added_tags: string[];
  expected_removed_tags: string[];
  expected_metadata_changes: MetadataChange[];
}

interface DiffFixture {
  description: string;
  abi_version_expected: number;
  cases: DiffCase[];
}

// ---------------------------------------------------------------------------
// Predicate envelope — round-trip every fixture case through
// PredicateWire ↔ AST ↔ JSON-header form.
// ---------------------------------------------------------------------------

describe('predicate nRPC envelope (cross-binding fixture)', () => {
  const fx = loadJson<PredicateFixture>(PREDICATE_FIXTURE, 'predicate envelope');

  it('header name pins to cyberdeck-where', () => {
    expect(fx.header_name).toBe(RPC_WHERE_HEADER);
  });

  for (const c of fx.cases) {
    it(`round-trips ${c.name}`, () => {
      // Wire → AST → wire equals the original wire (sans formatting).
      const ast = predicateFromWire(c.wire);
      const reEmitted = predicateToWire(ast);
      expect(reEmitted).toEqual(c.wire);

      // Header round-trip: stringified JSON parses cleanly and yields
      // the same AST.
      const headerVal = JSON.stringify(c.wire);
      const fromHeader = predicateFromRpcHeader(headerVal);
      expect(predicateToWire(fromHeader)).toEqual(c.wire);
    });
  }
});

// ---------------------------------------------------------------------------
// Capability-set diff — every fixture case computes byte-equal output.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Predicate evaluation — every fixture case yields the substrate's
// canonical boolean.
// ---------------------------------------------------------------------------

interface EvalCase {
  name: string;
  summary: string;
  wire: PredicateWire;
  tags: string[];
  metadata: Record<string, string>;
  expected: boolean;
}

interface EvalFixture {
  description: string;
  abi_version_expected: number;
  cases: EvalCase[];
}

describe('predicate evaluation (cross-binding fixture)', () => {
  const fx = loadJson<EvalFixture>(EVAL_FIXTURE, 'predicate eval');

  for (const c of fx.cases) {
    it(`evaluates ${c.name}`, () => {
      const pred = predicateFromWire(c.wire);
      const got = evaluatePredicate(pred, c.tags, c.metadata);
      expect(got).toBe(c.expected);
    });
  }
});

describe('CapabilitySet.diff (cross-binding fixture)', () => {
  const fx = loadJson<DiffFixture>(DIFF_FIXTURE, 'capability-set diff');

  for (const c of fx.cases) {
    it(`matches ${c.name}`, () => {
      const got: CapabilitySetDiff = diffCapabilities(c.prev, c.curr);
      expect(got.added_tags).toEqual(c.expected_added_tags);
      expect(got.removed_tags).toEqual(c.expected_removed_tags);
      expect(got.metadata_changes).toEqual(c.expected_metadata_changes);
    });
  }
});

// ---------------------------------------------------------------------------
// Local unit tests — typed taxonomy, reserved-prefix enforcement,
// chain helpers, StandardPlacement builder, predicate fluent API.
// ---------------------------------------------------------------------------

describe('typed taxonomy', () => {
  it('parses + renders axisPresent tags', () => {
    const tag = tagFromString('hardware.gpu');
    expect(tag).toEqual({ kind: 'axisPresent', axis: 'hardware', key: 'gpu' });
    expect(tagToString(tag)).toBe('hardware.gpu');
  });

  it('parses + renders axisValue tags with each separator', () => {
    const eq = tagFromString('software.os=linux');
    expect(eq).toEqual({
      kind: 'axisValue',
      axis: 'software',
      key: 'os',
      value: 'linux',
      separator: '=',
    });
    expect(tagToString(eq)).toBe('software.os=linux');

    const colon = tagFromString('dataforts.region:us-east');
    expect(colon).toEqual({
      kind: 'axisValue',
      axis: 'dataforts',
      key: 'region',
      value: 'us-east',
      separator: ':',
    });
    expect(tagToString(colon)).toBe('dataforts.region:us-east');
  });

  it('routes reserved prefixes into the Reserved variant', () => {
    for (const prefix of RESERVED_PREFIXES) {
      const wire = `${prefix}value`;
      const tag = tagFromString(wire);
      expect(tag).toEqual({ kind: 'reserved', prefix, body: 'value' });
      expect(tagToString(tag)).toBe(wire);
    }
  });

  it('falls back to legacy on unknown axis prefixes', () => {
    expect(tagFromString('myteam-tag')).toEqual({
      kind: 'legacy',
      raw: 'myteam-tag',
    });
    expect(tagFromString('unknown-axis.key')).toEqual({
      kind: 'legacy',
      raw: 'unknown-axis.key',
    });
  });

  it('rejects reserved prefixes via tagFromUserString', () => {
    for (const prefix of RESERVED_PREFIXES) {
      expect(() => tagFromUserString(`${prefix}value`)).toThrow(/reserved prefix/);
    }
  });

  it('tagKey rejects empty key', () => {
    expect(() => tagKey('hardware', '')).toThrow(/non-empty/);
  });
});

describe('chain composition helpers', () => {
  it('requireTag is idempotent and produces axis-prefixed wire', () => {
    let caps = emptyCapabilities();
    caps = requireTag(caps, 'hardware', 'gpu');
    caps = requireTag(caps, 'hardware', 'gpu');
    expect(caps.tags).toEqual(['hardware.gpu']);
  });

  it('requireAxisValue defaults to `=` separator and is idempotent', () => {
    let caps = emptyCapabilities();
    caps = requireAxisValue(caps, 'software', 'os', 'linux');
    caps = requireAxisValue(caps, 'software', 'os', 'linux');
    expect(caps.tags).toEqual(['software.os=linux']);
  });

  it('requireAxisValue with `:` separator round-trips', () => {
    const caps = requireAxisValue(
      emptyCapabilities(),
      'dataforts',
      'region',
      'us-east',
      ':',
    );
    expect(caps.tags).toEqual(['dataforts.region:us-east']);
  });

  it('withMetadata sets keys without mutating the input', () => {
    const a = emptyCapabilities();
    const b = withMetadata(a, 'intent', 'ml-training');
    expect(a.metadata).toEqual({});
    expect(b.metadata).toEqual({ intent: 'ml-training' });
  });

  it('chains compose left-to-right', () => {
    const caps = withMetadata(
      requireAxisValue(
        requireTag(emptyCapabilities(), 'hardware', 'gpu'),
        'software',
        'os',
        'linux',
      ),
      'intent',
      'ml-training',
    );
    expect(caps.tags.sort()).toEqual(['hardware.gpu', 'software.os=linux']);
    expect(caps.metadata).toEqual({ intent: 'ml-training' });
  });
});

describe('p.* fluent predicate builder', () => {
  it('builds the canonical complex case from the fixture', () => {
    // Match the structure of `complex_and_of_or_of_and_with_not`
    // without depending on its exact post-order index assignment;
    // re-emitting the AST yields a wire form that round-trips back
    // to the same AST.
    const pred: Predicate = p.and(
      p.or(
        p.exists(tagKey('hardware', 'gpu')),
        p.and(
          p.numericAtLeast(tagKey('hardware', 'memory_mb'), 65536),
          p.metadataExists('intent'),
        ),
      ),
      p.not(p.metadataEquals('decommissioning', 'true')),
      p.semverAtLeast(tagKey('software', 'runtime.python'), '3.10.0'),
    );
    const wire = predicateToWire(pred);
    // Round-trip property: rebuild + re-emit equals original.
    expect(predicateToWire(predicateFromWire(wire))).toEqual(wire);
    // Root is the outer And; the wire form puts it last.
    expect(wire.nodes[wire.root_idx].kind).toBe('and');
  });

  it('rejects child indices >= parent index in predicateFromWire', () => {
    const bad: PredicateWire = {
      nodes: [
        { kind: 'and', children: [1] },
        { kind: 'metadata_exists', key: 'x' },
      ],
      root_idx: 0,
    };
    expect(() => predicateFromWire(bad)).toThrow(/strictly less/);
  });

  it('rejects out-of-range root_idx', () => {
    const bad: PredicateWire = {
      nodes: [{ kind: 'metadata_exists', key: 'x' }],
      root_idx: 99,
    };
    expect(() => predicateFromWire(bad)).toThrow(/root_idx/);
  });

  it('predicateToRpcHeader emits the canonical JSON', () => {
    const pred = p.exists(tagKey('hardware', 'gpu'));
    const header = predicateToRpcHeader(pred);
    expect(JSON.parse(header)).toEqual({
      nodes: [{ kind: 'exists', key: { axis: 'hardware', key: 'gpu' } }],
      root_idx: 0,
    });
  });
});

describe('StandardPlacement builder', () => {
  it('compiles tag/metadata constraints + predicate into a config object', () => {
    const cfg = standardPlacement()
      .requireTag('hardware', 'gpu')
      .requireAxisValue('software', 'os', 'linux')
      .forbidTag('hardware', 'decommissioned')
      .requireMetadata('intent', 'ml-training')
      .withPredicate(p.metadataExists('owner'))
      .withLimit(3)
      .withCustomFilterId('placement-foo')
      .build();

    expect(cfg.requireTags).toEqual(['hardware.gpu', 'software.os=linux']);
    expect(cfg.forbidTags).toEqual(['hardware.decommissioned']);
    expect(cfg.requireMetadata).toEqual({ intent: 'ml-training' });
    expect(cfg.predicate?.nodes[0]).toEqual({
      kind: 'metadata_exists',
      key: 'owner',
    });
    expect(cfg.limit).toBe(3);
    expect(cfg.customFilterId).toBe('placement-foo');
  });

  it('rejects negative limits', () => {
    expect(() => standardPlacement().withLimit(-1)).toThrow(/non-negative/);
  });

  it('accepts a pre-built PredicateWire as well as an AST', () => {
    const wire = predicateToWire(p.exists(tagKey('hardware', 'gpu')));
    const cfg = standardPlacement().withPredicate(wire).build();
    expect(cfg.predicate).toEqual(wire);
  });
});

describe('placementFilterFromFn', () => {
  it('assigns auto-incrementing ids by default', () => {
    const a = placementFilterFromFn(() => true);
    const b = placementFilterFromFn(() => false);
    expect(a.id).not.toBe(b.id);
    expect(a.fn({ nodeId: 1n, tags: [], metadata: {} })).toBe(true);
    expect(b.fn({ nodeId: 1n, tags: [], metadata: {} })).toBe(false);
  });

  it('honours explicit ids', () => {
    const f = placementFilterFromFn(() => true, 'my-filter');
    expect(f.id).toBe('my-filter');
  });
});
