// Unit tests for the exported scope-tag builders.
//
// These helpers manipulate raw string arrays and never reach the Rust
// `CapabilitySet` builders, so the `warn`-on-drop added there does not
// cover them. An empty selector used to return the tag list unchanged —
// which reads as "scope applied" but leaves the announcement resolving
// to `Global`, visible to EVERY tenant and region query.
//
// See docs/internal/misc/SECURITY_AUDIT_2026_07_31_SCOPED_CAPABILITIES.md.

import { describe, expect, it } from 'vitest';

import {
  SCOPE_REGION_PREFIX,
  SCOPE_SUBNET_LOCAL,
  SCOPE_TENANT_PREFIX,
  withRegionScope,
  withSubnetLocalScope,
  withTenantScope,
} from '../src/capabilities';

describe('scope tag builders', () => {
  it('withTenantScope appends the reserved tag', () => {
    expect(withTenantScope(['gpu'], 'oem-123')).toEqual([
      'gpu',
      `${SCOPE_TENANT_PREFIX}oem-123`,
    ]);
  });

  it('withTenantScope is idempotent', () => {
    const once = withTenantScope(['gpu'], 'oem-123');
    expect(withTenantScope(once, 'oem-123')).toEqual(once);
  });

  it('withTenantScope accepts an undefined tag list', () => {
    expect(withTenantScope(undefined, 'oem-123')).toEqual([
      `${SCOPE_TENANT_PREFIX}oem-123`,
    ]);
  });

  // The regression. Returning the list unchanged left the caller
  // believing it had scoped an announcement that stayed global.
  it('withTenantScope throws on an empty tenant id rather than silently widening', () => {
    expect(() => withTenantScope(['gpu'], '')).toThrow();
  });

  it('withRegionScope appends the reserved tag', () => {
    expect(withRegionScope(['relay-capable'], 'eu-west')).toEqual([
      'relay-capable',
      `${SCOPE_REGION_PREFIX}eu-west`,
    ]);
  });

  it('withRegionScope is idempotent', () => {
    const once = withRegionScope(['relay-capable'], 'eu-west');
    expect(withRegionScope(once, 'eu-west')).toEqual(once);
  });

  it('withRegionScope throws on an empty region rather than silently widening', () => {
    expect(() => withRegionScope(['relay-capable'], '')).toThrow();
  });

  // No selector to be empty, so this one has no widening mode.
  it('withSubnetLocalScope appends the reserved tag and is idempotent', () => {
    const once = withSubnetLocalScope(['software:photoshop']);
    expect(once).toContain(SCOPE_SUBNET_LOCAL);
    expect(withSubnetLocalScope(once)).toEqual(once);
  });
});
