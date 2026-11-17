// Cross-binding wire-format compat for redactMetadataKeys + JSON
// round-trip on PredicateDebugReport. Drives the same fixture as
// the Rust + Python + Go bindings so all four agree on the
// redaction semantics byte-for-byte.

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

import type { PredicateDebugReport } from '../src/capability-enhancements';
import {
  p,
  predicateDebugReport,
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

  // TSDK-06 / CR-19: labels are matched literally, not by a grammar that
  // assumes delimiter-free keys and single-line values. Mirrors the C
  // ABI's `redact_label_handles_keys_containing_separator`.
  const one = (label: string): PredicateDebugReport => ({
    total_candidates: 1,
    matched: 0,
    clause_stats: [{ label, evaluated: 1, matched: 0 }],
  });
  const labelOf = (report: PredicateDebugReport): string => report.clause_stats[0]!.label;

  it('redacts a MetadataEquals whose key contains "="', () => {
    const report = predicateDebugReport(p.metadataEquals('credential=name', 'sk-secret'), [
      { tags: [], metadata: {} },
    ]);
    expect(labelOf(report)).toBe('MetadataEquals(credential=name=sk-secret)');
    const out = redactMetadataKeys(report, ['credential=name']);
    expect(labelOf(out)).toBe('MetadataEquals(credential=name=<redacted>)');
    expect(JSON.stringify(out)).not.toContain('sk-secret');
    // The earliest split whose left half is a supplied key wins, as in C.
    expect(labelOf(redactMetadataKeys(one('MetadataEquals(weird=key=sk-secret)'), ['weird=key']))).toBe(
      'MetadataEquals(weird=key=<redacted>)',
    );
  });

  it('redacts a MetadataEquals value containing line breaks', () => {
    const report = predicateDebugReport(p.metadataEquals('api_key', 'line-one\nsk-secret\r\nend'), [
      { tags: [], metadata: {} },
    ]);
    const out = redactMetadataKeys(report, ['api_key']);
    expect(labelOf(out)).toBe('MetadataEquals(api_key=<redacted>)');
    expect(JSON.stringify(out)).not.toContain('sk-secret');
  });

  it('redacts MetadataMatches and MetadataNumericAtLeast keys containing their separators', () => {
    expect(
      labelOf(redactMetadataKeys(one('MetadataMatches(a contains "b contains "sk-secret")'), ['a contains "b'])),
    ).toBe('MetadataMatches(a contains "b contains "<redacted>")');
    expect(labelOf(redactMetadataKeys(one('MetadataNumericAtLeast(a >= b >= 42)'), ['a >= b']))).toBe(
      'MetadataNumericAtLeast(a >= b >= <redacted>)',
    );
    expect(labelOf(redactMetadataKeys(one('MetadataMatches(k contains "x\ny")'), ['k']))).toBe(
      'MetadataMatches(k contains "<redacted>")',
    );
  });

  it('leaves non-targeted keys, MetadataExists and look-alike keys alone', () => {
    for (const label of [
      'MetadataEquals(region=us-east)',
      'MetadataExists(api_key)',
      // `api` is not `api_key`: a key must match the whole left half.
      'MetadataEquals(api_key_2=v)',
    ]) {
      expect(labelOf(redactMetadataKeys(one(label), ['api_key', 'api']))).toBe(label);
    }
  });
});
