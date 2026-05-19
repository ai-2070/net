/**
 * Subnet configuration — hierarchical 4-level grouping for routing
 * and visibility enforcement.
 *
 * Each node pins itself to one `SubnetId` (1–4 bytes, levels 0–3)
 * and optionally derives peer subnets from their capability
 * announcements via a `SubnetPolicy`. Channel visibility on the
 * publish + subscribe paths then consults `SubnetId` geometry to
 * decide whether a peer may see a given packet.
 *
 * @example
 * ```ts
 * import { MeshNode } from '@net-mesh/sdk';
 *
 * const policy = {
 *   rules: [
 *     { tagPrefix: 'region:', level: 0, values: { us: 3, eu: 4 } },
 *     { tagPrefix: 'fleet:', level: 1, values: { blue: 7, green: 8 } },
 *   ],
 * };
 *
 * const node = await MeshNode.create({
 *   bindAddr: '127.0.0.1:9000',
 *   psk: '42'.repeat(32),
 *   subnet: { levels: [3, 7] },
 *   subnetPolicy: policy,
 * });
 * ```
 *
 * Today visibility enforcement covers `'subnet-local'` and
 * `'parent-visible'`. `'exported'` (per-channel export tables) and
 * multi-hop gateway routing are follow-ups.
 */

// ----------------------------------------------------------------------------
// SubnetId — 1–4 hierarchy levels, each 0–255.
// ----------------------------------------------------------------------------

export interface SubnetId {
  /**
   * 1–4 level bytes. Example: `[3, 7, 2]` = level 0 bucket 3,
   * level 1 bucket 7, level 2 bucket 2, level 3 unset.
   *
   * Each value must fit in a u8 (0–255); at most 4 entries.
   */
  levels: number[];
}

/** Construct the conventional "no restriction" subnet. */
export const GLOBAL_SUBNET: SubnetId = { levels: [0] };

// ----------------------------------------------------------------------------
// SubnetRule / SubnetPolicy — tag-driven assignment.
// ----------------------------------------------------------------------------

/**
 * A single rule in a [`SubnetPolicy`]. When the policy runs against
 * a node's `CapabilitySet`, the first capability tag starting with
 * `tagPrefix` is looked up in `values`; the mapped byte fills the
 * rule's `level` slot in the derived `SubnetId`.
 */
export interface SubnetRule {
  /** Tag prefix to match — e.g. `'region:'`. */
  tagPrefix: string;
  /** Hierarchy level this rule fills (0–3). */
  level: number;
  /**
   * Map from tag suffix to subnet byte. E.g.
   * `{ eu: 1, us: 2, apac: 3 }` — a tag `"region:us"` against a
   * rule with `tagPrefix: "region:"` produces subnet byte `2` at
   * the rule's level.
   */
  values: Record<string, number>;
}

/**
 * Policy that derives each peer's `SubnetId` from their capability
 * tags. Rules are evaluated in order; unmatched levels remain zero.
 *
 * Mesh-wide policy consistency is assumed — mismatched policies
 * across nodes lead to asymmetric views of peer subnets.
 */
export interface SubnetPolicy {
  rules: SubnetRule[];
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/** Build a `SubnetId` from an inline byte list, validating ranges. */
export function subnetId(...levels: number[]): SubnetId {
  if (levels.length === 0 || levels.length > 4) {
    throw new Error(
      `subnet: levels must have 1–4 entries, got ${levels.length}`,
    );
  }
  for (const [i, v] of levels.entries()) {
    if (!Number.isInteger(v) || v < 0 || v > 255) {
      throw new Error(
        `subnet: level ${i} value ${v} must be an integer in [0, 255]`,
      );
    }
  }
  return { levels };
}
