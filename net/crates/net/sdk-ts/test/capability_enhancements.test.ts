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

  it('header name pins to net-where', () => {
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

// N-3 regression: diff must compare semantically, ignoring separator
// form on AxisValue tags. Two announcements emitting `hardware.k=v`
// vs `hardware.k:v` carry identical semantics — the substrate's
// `CapabilitySet::diff` was patched in CR-3 to match on
// `(axis, key, value)` and the TS rewrite must follow.
describe('diffCapabilities ignores AxisValue separator form (N-3 regression)', () => {
  it('treats hardware.k=v and hardware.k:v as identical', () => {
    const prev: CapabilitySetWire = {
      tags: ['hardware.gpu=nvidia-h100', 'software.os=linux'],
      metadata: {},
    };
    const curr: CapabilitySetWire = {
      tags: ['hardware.gpu:nvidia-h100', 'software.os:linux'],
      metadata: {},
    };
    const got = diffCapabilities(prev, curr);
    expect(got.added_tags).toEqual([]);
    expect(got.removed_tags).toEqual([]);
    expect(got.metadata_changes).toEqual([]);
  });

  it('still detects a real value change separately from a separator change', () => {
    const prev: CapabilitySetWire = {
      tags: ['hardware.gpu=nvidia-h100', 'software.os=linux'],
      metadata: {},
    };
    const curr: CapabilitySetWire = {
      tags: ['hardware.gpu:nvidia-h200', 'software.os:linux'],
      metadata: {},
    };
    const got = diffCapabilities(prev, curr);
    expect(got.added_tags).toEqual(['hardware.gpu:nvidia-h200']);
    expect(got.removed_tags).toEqual(['hardware.gpu=nvidia-h100']);
  });
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
          p.numericAtLeast(tagKey('hardware', 'memory_gb'), 64),
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

  // Q6: malformed wire input — non-integer root_idx — must throw
  // rather than returning `undefined` (the array-index lookup of
  // `nodes[1.5]` resolves to `undefined`).
  it('rejects non-integer root_idx', () => {
    for (const bogus of [1.5, NaN, Infinity, -Infinity]) {
      const bad: PredicateWire = {
        nodes: [{ kind: 'metadata_exists', key: 'x' }],
        root_idx: bogus,
      };
      expect(() => predicateFromWire(bad)).toThrow(/root_idx/);
    }
  });

  // Q5: same for child indices inside a composite. Pre-fix
  // a fractional or NaN child index passed the bounds check
  // (since `1.5 < selfIdx` may be true) and indexed `prior`,
  // yielding a malformed AST that crashed on later evaluation.
  it('rejects non-integer child indices', () => {
    const bad: PredicateWire = {
      nodes: [
        { kind: 'metadata_exists', key: 'x' },
        // Children list contains a fractional index referencing
        // node 0 — the bounds check `0.5 < 1` was true pre-fix.
        { kind: 'and', children: [0.5] },
      ],
      root_idx: 1,
    };
    expect(() => predicateFromWire(bad)).toThrow(/strictly less/);
  });

  it('predicateToRpcHeader emits the canonical JSON', () => {
    const pred = p.exists(tagKey('hardware', 'gpu'));
    const header = predicateToRpcHeader(pred);
    expect(JSON.parse(header)).toEqual({
      nodes: [{ kind: 'exists', key: { axis: 'hardware', key: 'gpu' } }],
      root_idx: 0,
    });
  });

  it('whereHeader builds a Buffer-valued net-where entry', async () => {
    const { whereHeader, RPC_WHERE_HEADER } = await import(
      '../src/capability-enhancements'
    );
    const pred = p.exists(tagKey('hardware', 'gpu'));
    const entry = whereHeader(pred);
    expect(entry.name).toBe(RPC_WHERE_HEADER);
    // Buffer holds the same bytes as the JSON string from
    // predicateToRpcHeader.
    expect(Buffer.isBuffer(entry.value)).toBe(true);
    expect(entry.value.toString('utf-8')).toBe(predicateToRpcHeader(pred));
  });
});

// P2-F: semverCompatible 0.0.x exact-only.
//
// Cargo's caret rule (`^0.0.x`) treats every 0.0 patch as a
// breaking-change boundary. Pre-fix the TS helper applied the
// 0.x.y minor-band rule even when the major was 0 AND the minor
// was 0, so 0.0.4 satisfied 0.0.3 (it shouldn't). Mirrors the
// Rust + Python fixes (CR / P1-D).
describe('semverCompatible 0.0.x exact-only', () => {
  it('rejects different patch in the 0.0.x band', () => {
    // Stored 0.0.4 against required 0.0.3: should miss (every
    // patch in the 0.0.x band is a breaking change).
    const tags = ['software.runtime.python=0.0.4'];
    const meta = {};
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.0.3'),
        tags,
        meta,
      ),
    ).toBe(false);
  });

  it('matches exact tuple in the 0.0.x band', () => {
    const tags = ['software.runtime.python=0.0.3'];
    const meta = {};
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.0.3'),
        tags,
        meta,
      ),
    ).toBe(true);
  });

  it('still applies minor-band rule for 0.x.y where x > 0', () => {
    const tags = ['software.runtime.python=0.2.5'];
    const meta = {};
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.2.3'),
        tags,
        meta,
      ),
    ).toBe(true);
  });

  it('still applies major-band rule for x.y.z where x > 0', () => {
    const tags = ['software.runtime.python=1.4.5'];
    const meta = {};
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '1.2.3'),
        tags,
        meta,
      ),
    ).toBe(true);
  });

  // Q1: a non-zero major lhs is NOT compatible with a 0.x.y rhs.
  // Pre-fix the TS helper checked only `rhs[1] === lhs[1]` for
  // the 0.x.y branch, so 1.2.5 satisfied ^0.2.3 (lhs >= rhs
  // passes since 1 > 0, then minors match) — diverged from Cargo
  // semantics and the Rust substrate.
  it('rejects non-zero-major lhs against 0.x.y rhs', () => {
    const meta = {};
    // 1.2.5 against ^0.2.3 must fail.
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.2.3'),
        ['software.runtime.python=1.2.5'],
        meta,
      ),
    ).toBe(false);
    // 2.2.5 against ^0.2.3 must also fail.
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.2.3'),
        ['software.runtime.python=2.2.5'],
        meta,
      ),
    ).toBe(false);
    // Sanity: 0.2.5 against ^0.2.3 still passes.
    expect(
      evaluatePredicate(
        p.semverCompatible(tagKey('software', 'runtime.python'), '0.2.3'),
        ['software.runtime.python=0.2.5'],
        meta,
      ),
    ).toBe(true);
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

// =====================================================================
// SDK Phase 7 cross-binding compat — wrap a predicate as a
// placement-filter callback and run it against the same
// `predicate_eval.json` fixture every binding consumes. Pins that
// the TS SDK's `placementFilterFromFn` correctly delivers each
// candidate's `(tags, metadata)` to the user closure such that
// direct `evaluatePredicate(pred, tags, metadata)` and the wrapped-
// callback path produce identical booleans.
//
// Mirror of the Rust-side
// `predicate_eval_fixture_matches_via_placement_filter_callback`
// test in `tests/cross_lang_capability_fixtures.rs`. Failures here
// vs there indicate cross-binding drift in either the predicate
// evaluator or the placement-filter helper.
// =====================================================================

// Regression: `metadataMatches` and `metadataNumericAtLeast` used
// to read metadata via direct property access (`metadata[key]`),
// which traverses the prototype chain. A metadata object that
// inherited e.g. `toString` from `Object.prototype` would have
// `metadataMatches('toString', ...)` return true while the
// adjacent `metadataExists('toString')` (which uses
// `hasOwnProperty`) returned false — the two predicates were out
// of lockstep. Pin parity by constructing a metadata object whose
// prototype carries an inherited string and asserting all four
// metadata predicates ignore it.
describe('metadata predicates ignore prototype-chain properties (regression)', () => {
  // Fabricate a metadata-like object with an inherited `inherited`
  // key whose value is a string. Direct access reads through the
  // prototype and returns "from-proto"; `hasOwnProperty` returns
  // false. The fix in evaluatePredicate routes every metadata
  // predicate through the latter so the inherited entry is
  // invisible regardless of which predicate is asked.
  const proto = { inherited: 'from-proto-1234' };
  const metadata: Record<string, string> = Object.create(proto) as Record<string, string>;
  // Sanity: own-property direct access matches what we configured.
  metadata.real = 'real-value';

  const tags: string[] = [];

  it('metadataExists returns false for inherited keys', () => {
    expect(evaluatePredicate(p.metadataExists('inherited'), tags, metadata)).toBe(false);
    expect(evaluatePredicate(p.metadataExists('real'), tags, metadata)).toBe(true);
  });

  it('metadataEquals returns false for inherited keys', () => {
    expect(
      evaluatePredicate(p.metadataEquals('inherited', 'from-proto-1234'), tags, metadata),
    ).toBe(false);
    expect(evaluatePredicate(p.metadataEquals('real', 'real-value'), tags, metadata)).toBe(true);
  });

  it('metadataMatches returns false for inherited keys', () => {
    // Pre-fix: this returned `true` because direct access read
    // through the prototype.
    expect(evaluatePredicate(p.metadataMatches('inherited', 'proto'), tags, metadata)).toBe(
      false,
    );
    expect(evaluatePredicate(p.metadataMatches('real', 'real'), tags, metadata)).toBe(true);
  });

  it('metadataNumericAtLeast returns false for inherited keys', () => {
    // Numeric variant has the same parity issue. Inject an
    // inherited numeric-string and assert it doesn't match.
    const numProto = { inherited_num: '42' };
    const numMeta: Record<string, string> = Object.create(numProto) as Record<string, string>;
    expect(
      evaluatePredicate(p.metadataNumericAtLeast('inherited_num', 1), tags, numMeta),
    ).toBe(false);
  });
});

// N-2 regression: numeric leaf predicates must accept every value
// shape Rust's `f64::from_str` accepts — scientific notation
// (`1e6`), leading `+`, decimal forms (`.5`, `1.`), and the
// special-case literals `inf` / `infinity` / `NaN`. Pre-fix the TS
// path applied a `/^-?\d+(\.\d+)?$/` regex pre-filter that rejected
// all of these, diverging from Rust on every cross-binding fixture
// row that exercises scientific notation. Mirrors R1's reasoning
// for the Go path (commit bab01616).
describe('numeric leaf parses match Rust f64::from_str (N-2 regression)', () => {
  const key = tagKey('software', 'context_length');
  const metadata: Record<string, string> = {};

  it('numericAtLeast accepts scientific notation', () => {
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1_000_000),
        ['software.context_length=1e6'],
        metadata,
      ),
    ).toBe(true);
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 2_000_000),
        ['software.context_length=1.5e6'],
        metadata,
      ),
    ).toBe(false);
  });

  it('numericAtLeast accepts leading +', () => {
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1500),
        ['software.context_length=+1500'],
        metadata,
      ),
    ).toBe(true);
  });

  it('numericInRange accepts decimal-leading dot and trailing dot', () => {
    expect(
      evaluatePredicate(
        p.numericInRange(key, 0, 1),
        ['software.context_length=.5'],
        metadata,
      ),
    ).toBe(true);
    expect(
      evaluatePredicate(
        p.numericInRange(key, 0, 2),
        ['software.context_length=1.'],
        metadata,
      ),
    ).toBe(true);
  });

  it('numericAtLeast forwards inf through IEEE comparison', () => {
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1e308),
        ['software.context_length=inf'],
        metadata,
      ),
    ).toBe(true);
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 0),
        ['software.context_length=-inf'],
        metadata,
      ),
    ).toBe(false);
  });

  it('numericAtMost forwards NaN as never-matching (IEEE)', () => {
    // `NaN >= threshold` and `NaN <= threshold` are both false in
    // IEEE 754, so a NaN-valued tag never satisfies any numeric
    // predicate. Rust's path agrees.
    expect(
      evaluatePredicate(
        p.numericAtMost(key, Number.MAX_VALUE),
        ['software.context_length=NaN'],
        metadata,
      ),
    ).toBe(false);
  });

  it('still rejects values Rust f64::from_str rejects', () => {
    // Hex floats, digit-separator underscores, trailing junk, and
    // whitespace-padded strings all fail Rust's parse. Confirm parity.
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1),
        ['software.context_length=0x1p3'],
        metadata,
      ),
    ).toBe(false);
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1),
        ['software.context_length=1_000'],
        metadata,
      ),
    ).toBe(false);
    expect(
      evaluatePredicate(
        p.numericAtLeast(key, 1),
        ['software.context_length= 5 '],
        metadata,
      ),
    ).toBe(false);
  });

  it('metadataNumericAtLeast uses the same parser as the tag path', () => {
    expect(
      evaluatePredicate(
        p.metadataNumericAtLeast('rate_limit', 1000),
        [],
        { rate_limit: '1.5e3' },
      ),
    ).toBe(true);
  });
});

// N-1 regression: AxisPresent tags ("hardware.gpu" with no separator)
// must satisfy `Exists` but must NOT satisfy any value predicate
// (Equals / StringPrefix / StringMatches), even when the predicate's
// value/prefix/pattern is the empty string. Mirrors the substrate's
// `match_axis_tag` (predicate.rs:1749-1757) which explicitly skips
// `AxisPresent` for value-predicate evaluation. Pre-fix:
// `axisTagValue` returned "" for an AxisPresent tag, and
// `equals(_, "")` etc. then matched it.
describe('AxisPresent tags do not satisfy value predicates (N-1 regression)', () => {
  const tags = ['hardware.gpu']; // AxisPresent — no separator, no value
  const metadata: Record<string, string> = {};
  const key = tagKey('hardware', 'gpu');

  it('exists matches AxisPresent', () => {
    expect(evaluatePredicate(p.exists(key), tags, metadata)).toBe(true);
  });

  it('equals(_, "") does NOT match AxisPresent', () => {
    expect(evaluatePredicate(p.equals(key, ''), tags, metadata)).toBe(false);
  });

  it('stringPrefix(_, "") does NOT match AxisPresent', () => {
    expect(evaluatePredicate(p.stringPrefix(key, ''), tags, metadata)).toBe(false);
  });

  it('stringMatches(_, "") does NOT match AxisPresent', () => {
    expect(evaluatePredicate(p.stringMatches(key, ''), tags, metadata)).toBe(false);
  });

  it('AxisValue tag still satisfies value predicates normally', () => {
    const valueTags = ['hardware.gpu=nvidia-h100'];
    expect(evaluatePredicate(p.equals(key, 'nvidia-h100'), valueTags, metadata)).toBe(true);
    expect(evaluatePredicate(p.stringPrefix(key, 'nvidia'), valueTags, metadata)).toBe(true);
    expect(evaluatePredicate(p.stringMatches(key, 'h100'), valueTags, metadata)).toBe(true);
  });
});

describe('placementFilterFromFn (cross-binding fixture)', () => {
  const fx = loadJson<EvalFixture>(EVAL_FIXTURE, 'predicate eval (placement filter)');

  for (const c of fx.cases) {
    it(`matches direct predicate evaluation for ${c.name}`, () => {
      const pred = predicateFromWire(c.wire);
      // Wrap the predicate evaluator as a `PlacementFilterFn`. The
      // candidate carries the case's `(tags, metadata)`; node_id is
      // arbitrary because the predicate doesn't read it.
      const filter = placementFilterFromFn((cand) =>
        evaluatePredicate(pred, cand.tags, cand.metadata),
      );
      const candidate = {
        nodeId: 0x1234_5678n,
        tags: c.tags,
        metadata: c.metadata,
      };
      expect(filter.fn(candidate)).toBe(c.expected);
    });
  }
});
