// Cross-binding wire-format compat for redactMetadataKeys + JSON
// round-trip on PredicateDebugReport. Drives the same fixture as
// the Rust + Python + Go bindings so all four agree on the
// redaction semantics byte-for-byte.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import type { PredicateDebugReport } from '../src/capability-enhancements';
import {
  predicateDebugReportFromWire,
  redactMetadataKeys,
} from '../src/capability-enhancements';

const REDACT_FIXTURE = resolve(
  __dirname,
  '../../tests/cross_lang_capability/predicate_debug_report_redacted.json',
);

interface RedactCase {
  name: string;
  summary: string;
  report: PredicateDebugReport;
  redact_keys: string[];
  redacted_report: PredicateDebugReport;
}

interface RedactFixture {
  description: string;
  abi_version_expected: number;
  redaction_rules: string[];
  cases: RedactCase[];
}

function loadFixture(): RedactFixture {
  if (!existsSync(REDACT_FIXTURE)) {
    throw new Error(
      `redaction fixture missing at ${REDACT_FIXTURE}; cross-binding test cannot run`,
    );
  }
  return JSON.parse(readFileSync(REDACT_FIXTURE, 'utf-8')) as RedactFixture;
}

describe('redactMetadataKeys (cross-binding fixture)', () => {
  const fx = loadFixture();
  for (const c of fx.cases) {
    it(`matches ${c.name}`, () => {
      const got = redactMetadataKeys(c.report, c.redact_keys);
      expect(got).toEqual(c.redacted_report);
    });
  }
});

describe('predicateDebugReportFromWire round-trip', () => {
  const fx = loadFixture();

  for (const c of fx.cases) {
    it(`reparses ${c.name} losslessly`, () => {
      const json = JSON.stringify(c.report);
      const decoded = predicateDebugReportFromWire(JSON.parse(json));
      expect(decoded).toEqual(c.report);
    });
  }

  it('rejects missing required fields', () => {
    expect(() => predicateDebugReportFromWire({})).toThrow();
    expect(() =>
      predicateDebugReportFromWire({
        total_candidates: 1,
        matched: 0,
        clause_stats: [{ label: 'X' }],
      }),
    ).toThrow(/clause_stats entry/);
  });

  it('rejects non-object input', () => {
    expect(() => predicateDebugReportFromWire(null)).toThrow();
    expect(() => predicateDebugReportFromWire(42)).toThrow();
    expect(() => predicateDebugReportFromWire('hi')).toThrow();
  });
});

describe('redaction local properties', () => {
  it('is idempotent: redact(redact(r, keys), keys) == redact(r, keys)', () => {
    const fx = loadFixture();
    for (const c of fx.cases) {
      const once = redactMetadataKeys(c.report, c.redact_keys);
      const twice = redactMetadataKeys(once, c.redact_keys);
      expect(twice).toEqual(once);
    }
  });

  it('preserves total_candidates and matched (only labels change)', () => {
    const report: PredicateDebugReport = {
      total_candidates: 100,
      matched: 42,
      clause_stats: [
        { label: 'MetadataEquals(intent=ml-training)', evaluated: 100, matched: 42 },
      ],
    };
    const out = redactMetadataKeys(report, ['intent']);
    expect(out.total_candidates).toBe(100);
    expect(out.matched).toBe(42);
  });
});
