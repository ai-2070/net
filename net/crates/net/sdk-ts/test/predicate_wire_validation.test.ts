// SDK audit 2026-10-11: predicateFromWire decoded unknown kinds to
// `undefined` and accepted mistyped fields (threshold: null evaluated as
// `n >= null`). Malformed nodes are now rejected at decode time.
import { describe, expect, it } from 'vitest';
import { predicateFromWire, tagKey } from '../src/capability-enhancements';

const key = { axis: 'hardware', key: 'gpu.vram_gb' };

describe('predicateFromWire validation', () => {
  it('rejects an unknown kind', () => {
    expect(() => predicateFromWire({ nodes: [{ kind: 'from_the_future' } as never], root_idx: 0 })).toThrow(
      /unknown kind/,
    );
  });

  it('rejects a mistyped threshold', () => {
    expect(() =>
      predicateFromWire({ nodes: [{ kind: 'numeric_at_least', key, threshold: null } as never], root_idx: 0 }),
    ).toThrow(/threshold/);
  });

  it('rejects a key that is not a tag key', () => {
    expect(() => predicateFromWire({ nodes: [{ kind: 'exists', key: 'gpu' } as never], root_idx: 0 })).toThrow(
      /tag key/,
    );
  });

  // cubic review of PR #1228: the axis is a closed enum.
  it('rejects a tag key with an unknown axis', () => {
    expect(() =>
      predicateFromWire({ nodes: [{ kind: 'exists', key: { axis: 'firmware', key: 'gpu' } } as never], root_idx: 0 }),
    ).toThrow(/key\.axis/);
    expect(() => tagKey('firmware' as never, 'gpu')).toThrow(/unknown axis/);
  });

  it('still decodes well-formed trees', () => {
    const pred = predicateFromWire({
      nodes: [
        { kind: 'numeric_at_least', key: key as never, threshold: 24 },
        { kind: 'not', child: 0 },
      ],
      root_idx: 1,
    });
    expect(pred).toEqual({ type: 'not', child: { type: 'numericAtLeast', key, threshold: 24 } });
  });
});
