// Cross-binding wire-format compat for the TS validator. Drives the
// same JSON fixture the Rust integration test consumes
// (`tests/cross_lang_capability/capability_validation.json`) so all
// bindings agree byte-for-byte on validator output.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import type {
  CapabilitySetWire,
  TaxonomyAxis,
} from '../src/capability-enhancements';
import { emptyCapabilities } from '../src/capability-enhancements';
import type {
  SchemaError,
  ValidationReport,
  ValidationWarning,
  ValueType,
} from '../src/capability-schema';
import {
  AXIS_SCHEMA,
  METADATA_SOFT_CAP_BYTES,
  isReportClean,
  isReportValid,
  validateCapabilities,
} from '../src/capability-schema';

// ---------------------------------------------------------------------------
// Fixture loader
// ---------------------------------------------------------------------------

const VALIDATION_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/capability_validation.json',
);

interface ValidationCase {
  name: string;
  summary: string;
  caps: CapabilitySetWire;
  expected_errors: SchemaError[];
  expected_warnings: ValidationWarning[];
}

interface ValidationFixture {
  description: string;
  abi_version_expected: number;
  schema_metadata_soft_cap_bytes: number;
  cases: ValidationCase[];
}

function loadFixture(): ValidationFixture {
  if (!existsSync(VALIDATION_FIXTURE)) {
    throw new Error(
      `validation fixture missing at ${VALIDATION_FIXTURE}; cross-binding test cannot run`,
    );
  }
  return JSON.parse(readFileSync(VALIDATION_FIXTURE, 'utf-8')) as ValidationFixture;
}

// Canonical sort: stable JSON-string comparison. Each binding does
// the same on its output before the equality check.
function sortedByJSON<T>(arr: T[]): T[] {
  return [...arr].sort((a, b) => {
    const sa = JSON.stringify(a);
    const sb = JSON.stringify(b);
    if (sa < sb) return -1;
    if (sa > sb) return 1;
    return 0;
  });
}

// ---------------------------------------------------------------------------
// Cross-binding fixture cases
// ---------------------------------------------------------------------------

describe('validateCapabilities (cross-binding fixture)', () => {
  const fx = loadFixture();

  it('soft-cap constant matches the fixture-pinned value', () => {
    expect(METADATA_SOFT_CAP_BYTES).toBe(fx.schema_metadata_soft_cap_bytes);
  });

  for (const c of fx.cases) {
    it(`matches ${c.name}`, () => {
      const got = validateCapabilities(c.caps);
      expect(sortedByJSON(got.errors)).toEqual(sortedByJSON(c.expected_errors));
      expect(sortedByJSON(got.warnings)).toEqual(
        sortedByJSON(c.expected_warnings),
      );
    });
  }
});

// ---------------------------------------------------------------------------
// Local unit tests
// ---------------------------------------------------------------------------

describe('AXIS_SCHEMA shape', () => {
  it('exposes hardware fixed keys', () => {
    const keys = AXIS_SCHEMA.hardware.keys.map((e) => e.key);
    expect(keys).toContain('cpu_cores');
    expect(keys).toContain('memory_gb');
    expect(keys).toContain('gpu');
    expect(keys).toContain('gpu.vendor');
    expect(keys).toContain('limits.max_concurrent_requests');
  });

  it('exposes software shapes including model + tool indexed collections', () => {
    const prefixes = AXIS_SCHEMA.software.shapes.map((s) => s.prefix);
    expect(prefixes).toEqual(
      expect.arrayContaining([
        'runtime.',
        'framework.',
        'driver.',
        'model.',
        'tool.',
      ]),
    );
  });

  it('devices and dataforts axes are reserved-empty', () => {
    expect(AXIS_SCHEMA.devices.keys).toHaveLength(0);
    expect(AXIS_SCHEMA.devices.shapes).toHaveLength(0);
    expect(AXIS_SCHEMA.dataforts.keys).toHaveLength(0);
    expect(AXIS_SCHEMA.dataforts.shapes).toHaveLength(0);
  });
});

describe('metadata soft-cap warning', () => {
  it('fires once total metadata bytes > soft cap', () => {
    // Build a metadata blob just over the cap. Single key+value pair
    // totalling 5 KB.
    const big = 'x'.repeat(METADATA_SOFT_CAP_BYTES + 100);
    const caps: CapabilitySetWire = {
      tags: [],
      metadata: { padding: big },
    };
    const report = validateCapabilities(caps);
    expect(report.errors).toEqual([]);
    const ovw = report.warnings.find((w) => w.kind === 'metadata_oversize');
    expect(ovw).toBeDefined();
    if (ovw && ovw.kind === 'metadata_oversize') {
      expect(ovw.soft_cap_bytes).toBe(METADATA_SOFT_CAP_BYTES);
      expect(ovw.actual_bytes).toBe('padding'.length + big.length);
    }
  });

  it('does not fire at exactly the cap', () => {
    const value = 'x'.repeat(METADATA_SOFT_CAP_BYTES - 'k'.length);
    const caps: CapabilitySetWire = {
      tags: [],
      metadata: { k: value },
    };
    const report = validateCapabilities(caps);
    expect(report.warnings).toEqual([]);
  });
});

describe('helper predicates', () => {
  it('isReportClean true only when both lists empty', () => {
    expect(isReportClean({ errors: [], warnings: [] })).toBe(true);
    expect(
      isReportClean({
        errors: [],
        warnings: [{ kind: 'legacy_tag', tag: 'foo' }],
      }),
    ).toBe(false);
  });

  it('isReportValid tolerates warnings', () => {
    expect(
      isReportValid({
        errors: [],
        warnings: [{ kind: 'legacy_tag', tag: 'foo' }],
      }),
    ).toBe(true);
    expect(
      isReportValid({
        errors: [
          {
            kind: 'type_mismatch',
            axis: 'hardware',
            key: 'memory_gb',
            expected: 'number',
            actual: 'lots',
          },
        ],
        warnings: [],
      }),
    ).toBe(false);
  });
});

describe('default schema usage', () => {
  it('clean report for empty capabilities', () => {
    const report = validateCapabilities(emptyCapabilities());
    expect(report.errors).toEqual([]);
    expect(report.warnings).toEqual([]);
  });
});

// Q10: indexed-collection index must fit in u32. The substrate
// parses `<axis>.<prefix><i>.<sub>` with `<i>` as `u32`; values
// above `u32::MAX` (4294967295) get rejected as IndexMalformed.
// Pre-fix the TS regex `/^\d+$/` admitted any digit string,
// silently passing payloads the substrate would later reject.
describe('regression: indexed collection index must fit u32', () => {
  it('rejects an index above u32::MAX as IndexMalformed', () => {
    // u32::MAX + 1 = 4294967296.
    const caps: CapabilitySetWire = {
      tags: ['software.model.4294967296.id=foo'],
      metadata: {},
    };
    const report = validateCapabilities(caps);
    const malformed = report.errors.find(
      (e) =>
        e.kind === 'index_malformed' &&
        (e as { index: string }).index === '4294967296',
    );
    expect(malformed).toBeDefined();
  });

  it('accepts u32::MAX itself', () => {
    const caps: CapabilitySetWire = {
      tags: ['software.model.4294967295.id=ok'],
      metadata: {},
    };
    const report = validateCapabilities(caps);
    expect(
      report.errors.find((e) => e.kind === 'index_malformed'),
    ).toBeUndefined();
  });
});

// P2-H: mirror substrate CR-14 reserved-metadata warnings.
// The schema declares `metadataReserved` (exact-match) and
// `metadataReservedPrefixes` (prefix-match) but pre-fix the TS
// validator never consulted them, so a user's
// `with_metadata("intent", …)` smuggling onto a scheduler-reserved
// key emitted no warning.
describe('regression: reserved metadata keys + prefixes', () => {
  it('warns on exact-match reserved metadata keys', () => {
    const caps: CapabilitySetWire = {
      tags: [],
      metadata: { intent: 'ml-training', benign: 'ok' },
    };
    const report = validateCapabilities(caps);
    const reserved = report.warnings.find(
      (w) =>
        w.kind === 'metadata_reserved_key' &&
        (w as { key: string }).key === 'intent',
    );
    expect(reserved).toBeDefined();
    // Benign key produces no warning.
    expect(
      report.warnings.find(
        (w) =>
          w.kind === 'metadata_reserved_key' &&
          (w as { key: string }).key === 'benign',
      ),
    ).toBeUndefined();
  });

  it('warns on reserved-prefix metadata keys', () => {
    const caps: CapabilitySetWire = {
      tags: [],
      metadata: { 'tool::evil::input_schema': 'spoof' },
    };
    const report = validateCapabilities(caps);
    const w = report.warnings.find(
      (w) => w.kind === 'metadata_reserved_prefix',
    ) as { key: string; prefix: string } | undefined;
    expect(w).toBeDefined();
    expect(w?.key).toBe('tool::evil::input_schema');
    expect(w?.prefix).toBe('tool::');
  });
});

// P1-B: substrate `Number` is unsigned (u64-only) — see CR-15 +
// `schema.rs::ValueType::Number`. Negative values surface as
// TypeMismatch errors in the substrate validator; the TS validator
// must mirror that decision so client-side checks don't pass a
// CapabilitySet the substrate would later reject.
describe('regression: number values reject negatives', () => {
  it('flags `hardware.memory_gb=-1` as a TypeMismatch error', () => {
    const caps: CapabilitySetWire = {
      tags: ['hardware.memory_gb=-1'],
      metadata: {},
    };
    const report = validateCapabilities(caps);
    const mismatch = report.errors.find(
      (e) =>
        e.kind === 'type_mismatch' &&
        e.axis === 'hardware' &&
        e.key === 'memory_gb',
    );
    expect(mismatch).toBeDefined();
    expect((mismatch as { actual: string }).actual).toBe('-1');
  });

  it('still accepts unsigned integer values', () => {
    const caps: CapabilitySetWire = {
      tags: ['hardware.memory_gb=64'],
      metadata: {},
    };
    const report = validateCapabilities(caps);
    expect(report.errors).toEqual([]);
  });
});
