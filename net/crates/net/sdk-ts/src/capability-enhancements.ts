/**
 * Capability-System Enhancements — TypeScript surface.
 *
 * This module mirrors the substrate's enhancement-track types:
 *
 * - Typed taxonomy ({@link Tag}, {@link TagKey}, {@link TaxonomyAxis})
 *   with reserved-prefix enforcement at construction time.
 * - Fluent {@link Predicate} builder (`p.*`) producing the canonical
 *   wire IR (`PredicateWire`) consumed by `cyberdeck-where` headers.
 * - {@link diffCapabilities} returning the same `CapabilitySetDiff`
 *   shape the cross-binding fixtures pin.
 * - {@link requireTag} / {@link requireAxisValue} chain helpers
 *   producing `{ tags, metadata }` directly.
 * - {@link StandardPlacement} configuration object + the
 *   {@link placementFilterFromFn} callback shape.
 *
 * All wire-format types match the JSON shape the substrate emits via
 * `serde_json` — the cross-binding tests in
 * `tests/cross_lang_capability/` pin the canonical bytes.
 *
 * @packageDocumentation
 */

// ============================================================================
// Typed taxonomy
// ============================================================================

/**
 * Canonical capability axis. Mirrors `TaxonomyAxis` in the substrate
 * (`adapter::net::behavior::tag::TaxonomyAxis`). The wire form is the
 * lowercase string.
 */
export type TaxonomyAxis = 'hardware' | 'software' | 'devices' | 'dataforts';

/** Every axis the substrate knows about. */
export const TAXONOMY_AXES: readonly TaxonomyAxis[] = [
  'hardware',
  'software',
  'devices',
  'dataforts',
] as const;

/**
 * Reserved cross-axis prefixes. Substrate-privileged paths
 * (`announceChain`, fork-coordination, scope helpers) emit these;
 * user code calling {@link tagFromUserString} is rejected.
 */
export const RESERVED_PREFIXES: readonly string[] = [
  'causal:',
  'fork-of:',
  'heat:',
  'scope:',
] as const;

/**
 * `<axis>.<key>` identifier — the addressing pair for axis-prefixed
 * tags and axis-keyed predicates.
 */
export interface TagKey {
  axis: TaxonomyAxis;
  /** Key within the axis namespace (e.g. `gpu`, `runtime.python`). */
  key: string;
}

/**
 * Build a {@link TagKey}. Throws on empty key — matches the substrate's
 * `TagKey::new` contract (the constructor is fallible-by-debug-assert
 * there; we surface as a thrown error here).
 */
export function tagKey(axis: TaxonomyAxis, key: string): TagKey {
  if (!key) {
    throw new Error(`tagKey: key must be non-empty (axis=${axis})`);
  }
  return { axis, key };
}

/** Separator between an axis-tag's key and its value. */
export type AxisSeparator = ':' | '=';

/**
 * Typed tag. Mirrors `Tag` in the substrate. The wire form is the
 * canonical Display string ({@link tagToString}).
 *
 * - `axisPresent` — `<axis>.<key>` (no value).
 * - `axisValue` — `<axis>.<key>=<value>` or `<axis>.<key>:<value>`.
 * - `reserved` — one of the {@link RESERVED_PREFIXES} cross-axis tags.
 * - `legacy` — arbitrary string outside the typed taxonomy. The
 *   substrate keeps these as-is for forward compatibility.
 */
export type Tag =
  | { kind: 'axisPresent'; axis: TaxonomyAxis; key: string }
  | {
      kind: 'axisValue';
      axis: TaxonomyAxis;
      key: string;
      value: string;
      separator: AxisSeparator;
    }
  | { kind: 'reserved'; prefix: string; body: string }
  | { kind: 'legacy'; raw: string };

/** True iff `s` starts with one of the {@link RESERVED_PREFIXES}. */
export function startsWithReservedPrefix(s: string): string | undefined {
  return RESERVED_PREFIXES.find((p) => s.startsWith(p));
}

/**
 * Render a {@link Tag} to its canonical wire string. Matches the
 * substrate's `Display` impl for `Tag`.
 */
export function tagToString(tag: Tag): string {
  switch (tag.kind) {
    case 'axisPresent':
      return `${tag.axis}.${tag.key}`;
    case 'axisValue':
      return `${tag.axis}.${tag.key}${tag.separator}${tag.value}`;
    case 'reserved':
      return `${tag.prefix}${tag.body}`;
    case 'legacy':
      return tag.raw;
  }
}

function axisFromPrefix(prefix: string): TaxonomyAxis | undefined {
  return (TAXONOMY_AXES as readonly string[]).includes(prefix)
    ? (prefix as TaxonomyAxis)
    : undefined;
}

/**
 * Parse a wire string into a {@link Tag}. Privileged path — does
 * NOT reject reserved prefixes (substrate code that legitimately
 * needs to round-trip e.g. `scope:tenant:foo` calls this).
 *
 * User code should prefer {@link tagFromUserString}, which rejects
 * reserved prefixes.
 */
export function tagFromString(s: string): Tag {
  if (!s) {
    throw new Error('tagFromString: tag must be non-empty');
  }
  const reserved = startsWithReservedPrefix(s);
  if (reserved !== undefined) {
    return { kind: 'reserved', prefix: reserved, body: s.slice(reserved.length) };
  }
  const dot = s.indexOf('.');
  if (dot < 0) {
    return { kind: 'legacy', raw: s };
  }
  const axis = axisFromPrefix(s.slice(0, dot));
  if (!axis) {
    return { kind: 'legacy', raw: s };
  }
  const body = s.slice(dot + 1);
  if (!body) {
    return { kind: 'legacy', raw: s };
  }
  // Pick the earliest of `=` / `:` so `key=value` wins over a
  // later `:`. Mirrors the substrate's `parse_axis_body`.
  const eq = body.indexOf('=');
  const colon = body.indexOf(':');
  let sep: AxisSeparator | undefined;
  let sepIdx = -1;
  if (eq >= 0 && colon >= 0) {
    if (eq < colon) {
      sep = '=';
      sepIdx = eq;
    } else {
      sep = ':';
      sepIdx = colon;
    }
  } else if (eq >= 0) {
    sep = '=';
    sepIdx = eq;
  } else if (colon >= 0) {
    sep = ':';
    sepIdx = colon;
  }
  if (sep === undefined) {
    return { kind: 'axisPresent', axis, key: body };
  }
  const key = body.slice(0, sepIdx);
  const value = body.slice(sepIdx + 1);
  if (!key || !value) {
    // Empty key or value — fall back to legacy so the substrate's
    // round-trip stays lossless.
    return { kind: 'legacy', raw: s };
  }
  return { kind: 'axisValue', axis, key, value, separator: sep };
}

/**
 * Parse a wire string from user code. Rejects the reserved
 * cross-axis prefixes ({@link RESERVED_PREFIXES}) — application code
 * cannot emit those by design. Mirrors the substrate's
 * `Tag::parse_user`.
 */
export function tagFromUserString(s: string): Tag {
  if (!s) {
    throw new Error('tagFromUserString: tag must be non-empty');
  }
  const reserved = startsWithReservedPrefix(s);
  if (reserved !== undefined) {
    throw new Error(
      `tag ${JSON.stringify(s)} starts with reserved prefix ${JSON.stringify(
        reserved,
      )}; user code cannot emit reserved-prefix tags`,
    );
  }
  return tagFromString(s);
}

// ============================================================================
// Predicate IR — mirrors `PredicateWire` / `PredicateNodeWire` in
// `adapter::net::behavior::predicate`.
//
// The IR is a flat post-order array of nodes plus a `root_idx`. AND
// / OR / NOT children reference earlier indices. This lets the
// JSON form sidestep the recursion-limit explosions a derive-based
// recursive serde would hit.
// ============================================================================

/** Numeric kinds — match the substrate's wire `kind` strings exactly. */
export type PredicateNode =
  | { kind: 'exists'; key: TagKey }
  | { kind: 'equals'; key: TagKey; value: string }
  | { kind: 'numeric_at_least'; key: TagKey; threshold: number }
  | { kind: 'numeric_at_most'; key: TagKey; threshold: number }
  | { kind: 'numeric_in_range'; key: TagKey; min: number; max: number }
  | { kind: 'semver_at_least'; key: TagKey; version: string }
  | { kind: 'semver_at_most'; key: TagKey; version: string }
  | { kind: 'semver_compatible'; key: TagKey; version: string }
  | { kind: 'string_prefix'; key: TagKey; prefix: string }
  | { kind: 'string_matches'; key: TagKey; pattern: string }
  | { kind: 'metadata_exists'; key: string }
  | { kind: 'metadata_equals'; key: string; value: string }
  | { kind: 'metadata_matches'; key: string; pattern: string }
  | { kind: 'metadata_numeric_at_least'; key: string; threshold: number }
  | { kind: 'and'; children: number[] }
  | { kind: 'or'; children: number[] }
  | { kind: 'not'; child: number };

/**
 * Wire-format predicate. The exact JSON shape the substrate's
 * `cyberdeck-where` request header carries; pinned by the
 * `predicate_nrpc_envelope.json` cross-binding fixture.
 */
export interface PredicateWire {
  nodes: PredicateNode[];
  root_idx: number;
}

/**
 * In-memory predicate AST. Sugar over {@link PredicateWire} — the
 * fluent {@link p} builder constructs these and {@link predicateToWire}
 * flattens them.
 */
export type Predicate =
  | { type: 'exists'; key: TagKey }
  | { type: 'equals'; key: TagKey; value: string }
  | { type: 'numericAtLeast'; key: TagKey; threshold: number }
  | { type: 'numericAtMost'; key: TagKey; threshold: number }
  | { type: 'numericInRange'; key: TagKey; min: number; max: number }
  | { type: 'semverAtLeast'; key: TagKey; version: string }
  | { type: 'semverAtMost'; key: TagKey; version: string }
  | { type: 'semverCompatible'; key: TagKey; version: string }
  | { type: 'stringPrefix'; key: TagKey; prefix: string }
  | { type: 'stringMatches'; key: TagKey; pattern: string }
  | { type: 'metadataExists'; key: string }
  | { type: 'metadataEquals'; key: string; value: string }
  | { type: 'metadataMatches'; key: string; pattern: string }
  | { type: 'metadataNumericAtLeast'; key: string; threshold: number }
  | { type: 'and'; children: Predicate[] }
  | { type: 'or'; children: Predicate[] }
  | { type: 'not'; child: Predicate };

/**
 * Fluent predicate constructors. Match the substrate's `Predicate::*`
 * factory methods one-to-one.
 *
 * @example
 * ```ts
 * import { p, predicateToWire } from '@ai2070/net-sdk';
 *
 * const pred = p.and(
 *   p.exists({ axis: 'hardware', key: 'gpu' }),
 *   p.numericAtLeast({ axis: 'hardware', key: 'memory_mb' }, 65536),
 *   p.metadataEquals('intent', 'ml-training'),
 * );
 * const wire = predicateToWire(pred);
 * ```
 */
export const p = {
  exists(key: TagKey): Predicate {
    return { type: 'exists', key };
  },
  equals(key: TagKey, value: string): Predicate {
    return { type: 'equals', key, value };
  },
  numericAtLeast(key: TagKey, threshold: number): Predicate {
    return { type: 'numericAtLeast', key, threshold };
  },
  numericAtMost(key: TagKey, threshold: number): Predicate {
    return { type: 'numericAtMost', key, threshold };
  },
  numericInRange(key: TagKey, min: number, max: number): Predicate {
    return { type: 'numericInRange', key, min, max };
  },
  semverAtLeast(key: TagKey, version: string): Predicate {
    return { type: 'semverAtLeast', key, version };
  },
  semverAtMost(key: TagKey, version: string): Predicate {
    return { type: 'semverAtMost', key, version };
  },
  semverCompatible(key: TagKey, version: string): Predicate {
    return { type: 'semverCompatible', key, version };
  },
  stringPrefix(key: TagKey, prefix: string): Predicate {
    return { type: 'stringPrefix', key, prefix };
  },
  stringMatches(key: TagKey, pattern: string): Predicate {
    return { type: 'stringMatches', key, pattern };
  },
  metadataExists(key: string): Predicate {
    return { type: 'metadataExists', key };
  },
  metadataEquals(key: string, value: string): Predicate {
    return { type: 'metadataEquals', key, value };
  },
  metadataMatches(key: string, pattern: string): Predicate {
    return { type: 'metadataMatches', key, pattern };
  },
  metadataNumericAtLeast(key: string, threshold: number): Predicate {
    return { type: 'metadataNumericAtLeast', key, threshold };
  },
  and(...children: Predicate[]): Predicate {
    return { type: 'and', children };
  },
  or(...children: Predicate[]): Predicate {
    return { type: 'or', children };
  },
  not(child: Predicate): Predicate {
    return { type: 'not', child };
  },
} as const;

function emit(node: Predicate, out: PredicateNode[]): number {
  switch (node.type) {
    case 'exists':
      out.push({ kind: 'exists', key: node.key });
      return out.length - 1;
    case 'equals':
      out.push({ kind: 'equals', key: node.key, value: node.value });
      return out.length - 1;
    case 'numericAtLeast':
      out.push({
        kind: 'numeric_at_least',
        key: node.key,
        threshold: node.threshold,
      });
      return out.length - 1;
    case 'numericAtMost':
      out.push({
        kind: 'numeric_at_most',
        key: node.key,
        threshold: node.threshold,
      });
      return out.length - 1;
    case 'numericInRange':
      out.push({
        kind: 'numeric_in_range',
        key: node.key,
        min: node.min,
        max: node.max,
      });
      return out.length - 1;
    case 'semverAtLeast':
      out.push({
        kind: 'semver_at_least',
        key: node.key,
        version: node.version,
      });
      return out.length - 1;
    case 'semverAtMost':
      out.push({
        kind: 'semver_at_most',
        key: node.key,
        version: node.version,
      });
      return out.length - 1;
    case 'semverCompatible':
      out.push({
        kind: 'semver_compatible',
        key: node.key,
        version: node.version,
      });
      return out.length - 1;
    case 'stringPrefix':
      out.push({ kind: 'string_prefix', key: node.key, prefix: node.prefix });
      return out.length - 1;
    case 'stringMatches':
      out.push({
        kind: 'string_matches',
        key: node.key,
        pattern: node.pattern,
      });
      return out.length - 1;
    case 'metadataExists':
      out.push({ kind: 'metadata_exists', key: node.key });
      return out.length - 1;
    case 'metadataEquals':
      out.push({
        kind: 'metadata_equals',
        key: node.key,
        value: node.value,
      });
      return out.length - 1;
    case 'metadataMatches':
      out.push({
        kind: 'metadata_matches',
        key: node.key,
        pattern: node.pattern,
      });
      return out.length - 1;
    case 'metadataNumericAtLeast':
      out.push({
        kind: 'metadata_numeric_at_least',
        key: node.key,
        threshold: node.threshold,
      });
      return out.length - 1;
    case 'and': {
      const childIdxs = node.children.map((c) => emit(c, out));
      out.push({ kind: 'and', children: childIdxs });
      return out.length - 1;
    }
    case 'or': {
      const childIdxs = node.children.map((c) => emit(c, out));
      out.push({ kind: 'or', children: childIdxs });
      return out.length - 1;
    }
    case 'not': {
      const childIdx = emit(node.child, out);
      out.push({ kind: 'not', child: childIdx });
      return out.length - 1;
    }
  }
}

/** Flatten an AST into the wire IR. Children always sit at lower
 * indices than their parents (post-order). */
export function predicateToWire(pred: Predicate): PredicateWire {
  const nodes: PredicateNode[] = [];
  const root_idx = emit(pred, nodes);
  return { nodes, root_idx };
}

/** Inverse of {@link predicateToWire}. Rebuilds the AST from a wire
 * IR. Throws on out-of-range indices or malformed nodes. */
export function predicateFromWire(wire: PredicateWire): Predicate {
  const built: Predicate[] = new Array(wire.nodes.length);
  // Post-order — every child has a strictly smaller index than its
  // parent, so a left-to-right walk is sufficient.
  for (let i = 0; i < wire.nodes.length; i++) {
    const n = wire.nodes[i];
    built[i] = nodeFromWire(n, built, i);
  }
  if (wire.root_idx < 0 || wire.root_idx >= built.length) {
    throw new Error(
      `predicateFromWire: root_idx ${wire.root_idx} out of range [0, ${built.length})`,
    );
  }
  return built[wire.root_idx];
}

function nodeFromWire(
  n: PredicateNode,
  prior: Predicate[],
  selfIdx: number,
): Predicate {
  const checkChild = (idx: number): Predicate => {
    if (idx < 0 || idx >= selfIdx) {
      throw new Error(
        `predicateFromWire: child index ${idx} not strictly less than self ${selfIdx}`,
      );
    }
    return prior[idx];
  };
  switch (n.kind) {
    case 'exists':
      return { type: 'exists', key: n.key };
    case 'equals':
      return { type: 'equals', key: n.key, value: n.value };
    case 'numeric_at_least':
      return { type: 'numericAtLeast', key: n.key, threshold: n.threshold };
    case 'numeric_at_most':
      return { type: 'numericAtMost', key: n.key, threshold: n.threshold };
    case 'numeric_in_range':
      return {
        type: 'numericInRange',
        key: n.key,
        min: n.min,
        max: n.max,
      };
    case 'semver_at_least':
      return { type: 'semverAtLeast', key: n.key, version: n.version };
    case 'semver_at_most':
      return { type: 'semverAtMost', key: n.key, version: n.version };
    case 'semver_compatible':
      return { type: 'semverCompatible', key: n.key, version: n.version };
    case 'string_prefix':
      return { type: 'stringPrefix', key: n.key, prefix: n.prefix };
    case 'string_matches':
      return { type: 'stringMatches', key: n.key, pattern: n.pattern };
    case 'metadata_exists':
      return { type: 'metadataExists', key: n.key };
    case 'metadata_equals':
      return { type: 'metadataEquals', key: n.key, value: n.value };
    case 'metadata_matches':
      return { type: 'metadataMatches', key: n.key, pattern: n.pattern };
    case 'metadata_numeric_at_least':
      return {
        type: 'metadataNumericAtLeast',
        key: n.key,
        threshold: n.threshold,
      };
    case 'and':
      return {
        type: 'and',
        children: n.children.map(checkChild),
      };
    case 'or':
      return {
        type: 'or',
        children: n.children.map(checkChild),
      };
    case 'not':
      return { type: 'not', child: checkChild(n.child) };
  }
}

// ============================================================================
// nRPC envelope helpers — mirror `predicate_to_rpc_header` /
// `predicate_from_rpc_headers` in the substrate.
// ============================================================================

/** Header the substrate uses to carry a predicate over nRPC. */
export const RPC_WHERE_HEADER = 'cyberdeck-where';

/** Encode a predicate into the request-header value. The substrate
 * pins this as the canonical JSON-encoded {@link PredicateWire}. */
export function predicateToRpcHeader(pred: Predicate): string {
  return JSON.stringify(predicateToWire(pred));
}

/** Decode a `cyberdeck-where` header value back into an AST. Throws
 * on malformed JSON or out-of-range indices. */
export function predicateFromRpcHeader(value: string): Predicate {
  const wire = JSON.parse(value) as PredicateWire;
  return predicateFromWire(wire);
}

/**
 * Build the `cyberdeck-where:` request-header entry for a
 * Phase 9b predicate-pushdown call. Drops straight into a
 * `MeshRpc.call` `CallOptions.requestHeaders` array.
 *
 * @example
 * ```ts
 * import { p, tagKey, whereHeader } from '@ai2070/net-sdk';
 * const pred = p.exists(tagKey('hardware', 'gpu'));
 * await rpc.call(targetNodeId, 'filter-svc', body, {
 *   requestHeaders: [whereHeader(pred)],
 * });
 * ```
 *
 * The header value is the canonical JSON-encoded `PredicateWire`
 * pinned by `predicate_nrpc_envelope.json`.
 */
export function whereHeader(pred: Predicate): {
  name: string;
  value: Buffer;
} {
  return {
    name: RPC_WHERE_HEADER,
    value: Buffer.from(predicateToRpcHeader(pred), 'utf-8'),
  };
}

// ============================================================================
// CapabilitySet diff — mirrors `CapabilitySet::diff` and the
// cross-binding `capability_set_diff.json` fixture.
//
// Inputs are the wire-format `{ tags: string[], metadata:
// Record<string, string> }` shape — what the announcement carries.
// Outputs are sorted-by-wire-form arrays so consumers can rely on
// deterministic order without a second sort pass.
// ============================================================================

/** Wire-format capability shape — string tags + string→string metadata. */
export interface CapabilitySetWire {
  tags: string[];
  metadata: Record<string, string>;
}

/** Per-key metadata change. Mirrors `MetadataChange` in the substrate. */
export type MetadataChange =
  | { kind: 'added'; key: string; value: string }
  | { kind: 'removed'; key: string; prev_value: string }
  | {
      kind: 'updated';
      key: string;
      prev_value: string;
      new_value: string;
    };

/** Diff between two capability snapshots. Mirrors
 * `CapabilitySetDiff` in the substrate. */
export interface CapabilitySetDiff {
  added_tags: string[];
  removed_tags: string[];
  metadata_changes: MetadataChange[];
}

/**
 * Compute `curr.diff(prev)`. Pinned by the
 * `capability_set_diff.json` cross-binding fixture.
 *
 * - Tag arrays are sorted by wire string.
 * - Metadata changes are sorted by key (BTreeMap semantics in the
 *   substrate).
 * - A key rename surfaces as Removed + Added (NOT Updated). Only a
 *   value change for the same key is Updated.
 */
export function diffCapabilities(
  prev: CapabilitySetWire,
  curr: CapabilitySetWire,
): CapabilitySetDiff {
  const prevTagSet = new Set(prev.tags);
  const currTagSet = new Set(curr.tags);
  const added_tags: string[] = [];
  const removed_tags: string[] = [];
  for (const t of currTagSet) {
    if (!prevTagSet.has(t)) added_tags.push(t);
  }
  for (const t of prevTagSet) {
    if (!currTagSet.has(t)) removed_tags.push(t);
  }
  added_tags.sort();
  removed_tags.sort();

  const metadata_changes: MetadataChange[] = [];
  const allKeys = new Set<string>([
    ...Object.keys(prev.metadata),
    ...Object.keys(curr.metadata),
  ]);
  const sortedKeys = Array.from(allKeys).sort();
  for (const key of sortedKeys) {
    const inPrev = Object.prototype.hasOwnProperty.call(prev.metadata, key);
    const inCurr = Object.prototype.hasOwnProperty.call(curr.metadata, key);
    if (inPrev && inCurr) {
      const prev_value = prev.metadata[key];
      const new_value = curr.metadata[key];
      if (prev_value !== new_value) {
        metadata_changes.push({
          kind: 'updated',
          key,
          prev_value,
          new_value,
        });
      }
    } else if (inCurr) {
      metadata_changes.push({
        kind: 'added',
        key,
        value: curr.metadata[key],
      });
    } else if (inPrev) {
      metadata_changes.push({
        kind: 'removed',
        key,
        prev_value: prev.metadata[key],
      });
    }
  }
  return { added_tags, removed_tags, metadata_changes };
}

// ============================================================================
// Chain composition helpers — `requireTag`, `requireAxisValue`, and
// metadata setters operating on the wire `{ tags, metadata }` shape.
//
// These are the user-facing builders for the typed-tag taxonomy. Each
// one returns a NEW object — the inputs are not mutated, so chains
// can be composed left-to-right without aliasing surprises.
// ============================================================================

function freshTags(caps: CapabilitySetWire): string[] {
  // Use a Set to keep tag membership unique, then return as a stable
  // array. Insertion order is preserved unless the tag was already
  // present.
  return Array.from(new Set(caps.tags));
}

/**
 * Add an axis-tag (no value) to the wire shape. Idempotent; no-op
 * if the tag is already present.
 */
export function requireTag(
  caps: CapabilitySetWire,
  axis: TaxonomyAxis,
  key: string,
): CapabilitySetWire {
  if (!key) {
    throw new Error('requireTag: key must be non-empty');
  }
  const wire = tagToString({ kind: 'axisPresent', axis, key });
  const tags = freshTags(caps);
  if (!tags.includes(wire)) tags.push(wire);
  return { tags, metadata: { ...caps.metadata } };
}

/**
 * Add an axis-value tag (`<axis>.<key>=<value>` by default) to the
 * wire shape. Idempotent for the exact (axis, key, value, separator)
 * triple.
 */
export function requireAxisValue(
  caps: CapabilitySetWire,
  axis: TaxonomyAxis,
  key: string,
  value: string,
  separator: AxisSeparator = '=',
): CapabilitySetWire {
  if (!key) {
    throw new Error('requireAxisValue: key must be non-empty');
  }
  if (!value) {
    throw new Error('requireAxisValue: value must be non-empty');
  }
  const wire = tagToString({
    kind: 'axisValue',
    axis,
    key,
    value,
    separator,
  });
  const tags = freshTags(caps);
  if (!tags.includes(wire)) tags.push(wire);
  return { tags, metadata: { ...caps.metadata } };
}

/** Set / overwrite a metadata entry. */
export function withMetadata(
  caps: CapabilitySetWire,
  key: string,
  value: string,
): CapabilitySetWire {
  if (!key) throw new Error('withMetadata: key must be non-empty');
  return {
    tags: [...caps.tags],
    metadata: { ...caps.metadata, [key]: value },
  };
}

/** Empty wire-format capability set. */
export function emptyCapabilities(): CapabilitySetWire {
  return { tags: [], metadata: {} };
}

// ============================================================================
// StandardPlacement — config object mirroring the substrate's typed
// placement filter. The execution side lives in the substrate (the
// runtime walks the live capability index using these constraints);
// the SDK exposes the *configuration* shape so daemons can declare
// their placement requirements.
// ============================================================================

/**
 * Configuration for the substrate's `StandardPlacement` filter. All
 * fields are optional — an empty config matches every node.
 *
 * The wire shape is JSON-friendly (`predicate` carries a
 * {@link PredicateWire}, not the AST). Bindings encode this object
 * before handing it to the runtime.
 */
export interface StandardPlacement {
  /**
   * Required tags — every listed wire-string must be present. Use
   * {@link tagToString} to turn typed {@link Tag} values into
   * wire form.
   */
  requireTags?: string[];
  /**
   * Forbidden tags — none of these may be present. Same wire form
   * as {@link requireTags}.
   */
  forbidTags?: string[];
  /** Required metadata key/value equalities. */
  requireMetadata?: Record<string, string>;
  /**
   * Free-form predicate evaluated against each candidate's tag set
   * + metadata. Combined with the tag/metadata constraints via AND.
   */
  predicate?: PredicateWire;
  /**
   * Maximum candidates to return. The runtime picks deterministically
   * by node id when more match.
   */
  limit?: number;
  /**
   * Custom placement filter — a string id resolved by the runtime
   * to a registered callback (see {@link placementFilterFromFn}).
   */
  customFilterId?: string;
}

/**
 * Builder for {@link StandardPlacement}. Returns a frozen config
 * object suitable for handing to the runtime.
 */
export class StandardPlacementBuilder {
  private cfg: StandardPlacement = {};

  requireTag(axis: TaxonomyAxis, key: string): this {
    const wire = tagToString({ kind: 'axisPresent', axis, key });
    this.cfg.requireTags = [...(this.cfg.requireTags ?? []), wire];
    return this;
  }

  requireAxisValue(
    axis: TaxonomyAxis,
    key: string,
    value: string,
    separator: AxisSeparator = '=',
  ): this {
    const wire = tagToString({
      kind: 'axisValue',
      axis,
      key,
      value,
      separator,
    });
    this.cfg.requireTags = [...(this.cfg.requireTags ?? []), wire];
    return this;
  }

  forbidTag(axis: TaxonomyAxis, key: string): this {
    const wire = tagToString({ kind: 'axisPresent', axis, key });
    this.cfg.forbidTags = [...(this.cfg.forbidTags ?? []), wire];
    return this;
  }

  requireMetadata(key: string, value: string): this {
    this.cfg.requireMetadata = { ...(this.cfg.requireMetadata ?? {}), [key]: value };
    return this;
  }

  withPredicate(pred: Predicate | PredicateWire): this {
    this.cfg.predicate = isPredicateWire(pred) ? pred : predicateToWire(pred);
    return this;
  }

  withLimit(n: number): this {
    if (!Number.isFinite(n) || n < 0) {
      throw new Error('StandardPlacementBuilder.withLimit: n must be non-negative finite');
    }
    this.cfg.limit = Math.floor(n);
    return this;
  }

  withCustomFilterId(id: string): this {
    if (!id) throw new Error('StandardPlacementBuilder.withCustomFilterId: id must be non-empty');
    this.cfg.customFilterId = id;
    return this;
  }

  build(): StandardPlacement {
    // Defensive copy + freeze. The builder retains its own state so
    // a chain like `b.build(); b.requireTag(...).build()` works.
    return Object.freeze({
      ...this.cfg,
      requireTags: this.cfg.requireTags
        ? [...this.cfg.requireTags]
        : undefined,
      forbidTags: this.cfg.forbidTags ? [...this.cfg.forbidTags] : undefined,
      requireMetadata: this.cfg.requireMetadata
        ? { ...this.cfg.requireMetadata }
        : undefined,
      predicate: this.cfg.predicate
        ? {
            nodes: [...this.cfg.predicate.nodes],
            root_idx: this.cfg.predicate.root_idx,
          }
        : undefined,
    });
  }
}

function isPredicateWire(v: Predicate | PredicateWire): v is PredicateWire {
  return (
    typeof v === 'object' &&
    v !== null &&
    Array.isArray((v as PredicateWire).nodes) &&
    typeof (v as PredicateWire).root_idx === 'number'
  );
}

/** Convenience constructor for a {@link StandardPlacementBuilder}. */
export function standardPlacement(): StandardPlacementBuilder {
  return new StandardPlacementBuilder();
}

// ============================================================================
// Custom placement-filter callback
// ============================================================================

/**
 * Candidate handed to a custom placement-filter callback. The
 * runtime materializes one of these per candidate before evaluating
 * the user predicate.
 */
export interface PlacementCandidate {
  nodeId: bigint;
  tags: string[];
  metadata: Record<string, string>;
}

/**
 * Synchronous predicate: `true` to keep, `false` to drop.
 *
 * Custom filters run under the placement hot path — keep them tight
 * and avoid I/O. The runtime registers them by id; the daemon's
 * {@link StandardPlacement} references that id via
 * {@link StandardPlacement.customFilterId}.
 */
export type PlacementFilterFn = (candidate: PlacementCandidate) => boolean;

/**
 * Wrap a user-supplied predicate as a placement filter. Returns a
 * `{ id, fn }` pair the binding can register with the runtime —
 * future `StandardPlacement.customFilterId = id` uses route through
 * the wrapped function.
 *
 * The default id generator uses a counter; callers can supply an
 * explicit id when they want stable identity (e.g. for hot-reload).
 */
export interface RegisteredPlacementFilter {
  id: string;
  fn: PlacementFilterFn;
}

let placementFilterCounter = 0;

export function placementFilterFromFn(
  fn: PlacementFilterFn,
  explicitId?: string,
): RegisteredPlacementFilter {
  const id = explicitId ?? `pf-${++placementFilterCounter}`;
  return { id, fn };
}

// ============================================================================
// Predicate evaluation — pure local evaluator over (tags, metadata).
//
// Mirrors the substrate's `Predicate::evaluate_unplanned`: composite
// recursion in declaration order, leaf semantics matching
// `evaluate_leaf`. The planned variant in the substrate reorders
// And / Or children by static cost; the boolean answer is invariant
// to that reordering, so the SDK-side evaluator skips planning.
//
// Pinned across bindings by `tests/cross_lang_capability/predicate_eval.json`.
// ============================================================================

type SemverTriple = readonly [number, number, number];

function parseSemver(s: string): SemverTriple | undefined {
  // Drop pre-release / build suffix.
  const dash = s.indexOf('-');
  const plus = s.indexOf('+');
  let core: string;
  if (dash >= 0 && plus >= 0) {
    core = s.slice(0, Math.min(dash, plus));
  } else if (dash >= 0) {
    core = s.slice(0, dash);
  } else if (plus >= 0) {
    core = s.slice(0, plus);
  } else {
    core = s;
  }
  const parts = core.split('.').map((p) => p.trim());
  if (parts.length === 0 || parts.length > 3) return undefined;
  const major = Number.parseInt(parts[0], 10);
  if (!Number.isFinite(major) || parts[0] === '' || /[^0-9]/.test(parts[0])) {
    return undefined;
  }
  const parsePart = (s: string | undefined): number | undefined => {
    if (s === undefined) return 0;
    if (s === '' || /[^0-9]/.test(s)) return undefined;
    const n = Number.parseInt(s, 10);
    return Number.isFinite(n) ? n : undefined;
  };
  const minor = parsePart(parts[1]);
  const patch = parsePart(parts[2]);
  if (minor === undefined || patch === undefined) return undefined;
  return [major, minor, patch];
}

function semverCmp(a: SemverTriple, b: SemverTriple): number {
  if (a[0] !== b[0]) return a[0] - b[0];
  if (a[1] !== b[1]) return a[1] - b[1];
  return a[2] - b[2];
}

function semverCompatible(lhs: SemverTriple, rhs: SemverTriple): boolean {
  if (semverCmp(lhs, rhs) < 0) return false;
  if (rhs[0] === 0) {
    // 0.x.y — minor is the compatibility band.
    return rhs[1] === lhs[1];
  }
  return rhs[0] === lhs[0];
}

/**
 * Find the value of an axis-keyed tag in the wire-format tag list,
 * if any. AxisPresent tags ("hardware.gpu") have no value — the
 * substrate's `match_axis_tag` calls `value_pred("")` for those, so
 * SDK-side leaf evaluators that need a value (e.g. numeric_at_least)
 * naturally fail when the tag is AxisPresent.
 *
 * Returns the matched value string for AxisValue tags, the empty
 * string for AxisPresent tags, or `undefined` if no tag matches.
 */
function axisTagValue(
  tags: readonly string[],
  key: TagKey,
): string | undefined {
  const prefix = `${key.axis}.${key.key}`;
  for (const wire of tags) {
    if (wire === prefix) return '';
    // Match `<axis>.<key>=<value>` or `<axis>.<key>:<value>`. Reject
    // longer key-prefixes (`hardware.gpu` should NOT match
    // `hardware.gpu.vendor=nvidia` — that's a different key).
    if (wire.length <= prefix.length) continue;
    if (!wire.startsWith(prefix)) continue;
    const sep = wire.charAt(prefix.length);
    if (sep === '=' || sep === ':') {
      return wire.slice(prefix.length + 1);
    }
  }
  return undefined;
}

function evalLeaf(
  pred: Predicate,
  tags: readonly string[],
  metadata: Readonly<Record<string, string>>,
): boolean {
  switch (pred.type) {
    case 'exists': {
      return axisTagValue(tags, pred.key) !== undefined;
    }
    case 'equals': {
      const v = axisTagValue(tags, pred.key);
      return v !== undefined && v === pred.value;
    }
    case 'numericAtLeast': {
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const n = Number.parseFloat(v);
      return Number.isFinite(n) && /^-?\d+(\.\d+)?$/.test(v) && n >= pred.threshold;
    }
    case 'numericAtMost': {
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const n = Number.parseFloat(v);
      return Number.isFinite(n) && /^-?\d+(\.\d+)?$/.test(v) && n <= pred.threshold;
    }
    case 'numericInRange': {
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const n = Number.parseFloat(v);
      return (
        Number.isFinite(n) &&
        /^-?\d+(\.\d+)?$/.test(v) &&
        n >= pred.min &&
        n <= pred.max
      );
    }
    case 'semverAtLeast': {
      const rhs = parseSemver(pred.version);
      if (rhs === undefined) return false;
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const lhs = parseSemver(v);
      return lhs !== undefined && semverCmp(lhs, rhs) >= 0;
    }
    case 'semverAtMost': {
      const rhs = parseSemver(pred.version);
      if (rhs === undefined) return false;
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const lhs = parseSemver(v);
      return lhs !== undefined && semverCmp(lhs, rhs) <= 0;
    }
    case 'semverCompatible': {
      const rhs = parseSemver(pred.version);
      if (rhs === undefined) return false;
      const v = axisTagValue(tags, pred.key);
      if (v === undefined) return false;
      const lhs = parseSemver(v);
      return lhs !== undefined && semverCompatible(lhs, rhs);
    }
    case 'stringPrefix': {
      const v = axisTagValue(tags, pred.key);
      return v !== undefined && v.startsWith(pred.prefix);
    }
    case 'stringMatches': {
      const v = axisTagValue(tags, pred.key);
      return v !== undefined && v.includes(pred.pattern);
    }
    case 'metadataExists': {
      return Object.prototype.hasOwnProperty.call(metadata, pred.key);
    }
    case 'metadataEquals': {
      return (
        Object.prototype.hasOwnProperty.call(metadata, pred.key) &&
        metadata[pred.key] === pred.value
      );
    }
    case 'metadataMatches': {
      const v = metadata[pred.key];
      return v !== undefined && v.includes(pred.pattern);
    }
    case 'metadataNumericAtLeast': {
      const v = metadata[pred.key];
      if (v === undefined) return false;
      const n = Number.parseFloat(v);
      return Number.isFinite(n) && /^-?\d+(\.\d+)?$/.test(v) && n >= pred.threshold;
    }
    case 'and':
    case 'or':
    case 'not':
      throw new Error(
        `evalLeaf: composite predicate ${pred.type} routed through leaf evaluator (internal bug)`,
      );
  }
}

/**
 * Evaluate a {@link Predicate} against a wire-format `(tags, metadata)`
 * context. Mirrors the substrate's `Predicate::evaluate_unplanned`
 * — children of `and` / `or` evaluate in declaration order with
 * standard short-circuit semantics.
 *
 * Pinned across bindings by `predicate_eval.json`. Use this for local
 * pre-filtering of result sets before sending an nRPC `where:`
 * predicate over the wire, or for client-side validation of a
 * predicate against a known capability set.
 */
export function evaluatePredicate(
  pred: Predicate,
  tags: readonly string[],
  metadata: Readonly<Record<string, string>>,
): boolean {
  switch (pred.type) {
    case 'and':
      return pred.children.every((c) => evaluatePredicate(c, tags, metadata));
    case 'or':
      return pred.children.some((c) => evaluatePredicate(c, tags, metadata));
    case 'not':
      return !evaluatePredicate(pred.child, tags, metadata);
    default:
      return evalLeaf(pred, tags, metadata);
  }
}

// ============================================================================
// Predicate trace evaluator — Phase 9d slice. Mirrors the substrate's
// `Predicate::evaluate_with_trace`: children of `and` / `or` evaluate
// in cost-ascending order (planner reorders); short-circuited
// siblings are dropped from the trace. Pinned across bindings by
// `predicate_trace.json`.
// ============================================================================

/**
 * Per-clause trace entry. Mirrors the substrate's `ClauseTrace`:
 * each leaf carries a one-line `label` + the boolean `result`;
 * composites carry the planner-ordered subset of children that
 * actually ran.
 */
export interface ClauseTrace {
  label: string;
  result: boolean;
  children: ClauseTrace[];
}

/**
 * Static per-variant cost — matches the substrate's `static_cost`.
 * Lower = cheaper; planner sorts children ascending. Composites sum
 * their children's costs.
 */
function predStaticCost(p: Predicate): number {
  switch (p.type) {
    case 'metadataExists':
      return 10;
    case 'metadataEquals':
      return 11;
    case 'exists':
      return 20;
    case 'equals':
      return 21;
    case 'metadataNumericAtLeast':
      return 25;
    case 'numericAtLeast':
    case 'numericAtMost':
    case 'numericInRange':
      return 30;
    case 'stringPrefix':
      return 40;
    case 'metadataMatches':
      return 45;
    case 'stringMatches':
      return 50;
    case 'semverAtLeast':
    case 'semverAtMost':
    case 'semverCompatible':
      return 60;
    case 'and':
    case 'or':
      return p.children.reduce(
        (acc, c) => Math.min(acc + predStaticCost(c), 0xffffffff),
        0,
      );
    case 'not':
      return predStaticCost(p.child);
  }
}

function predDebugLabel(p: Predicate): string {
  // Rust's `{:?}` on a string adds quotes + escapes. We match that
  // for string-bearing leaves so labels round-trip with the
  // substrate's `debug_label`.
  const dbg = (s: string): string => JSON.stringify(s);
  const tk = (k: TagKey): string => `${k.axis}.${k.key}`;
  switch (p.type) {
    case 'exists':
      return `Exists(${tk(p.key)})`;
    case 'equals':
      return `Equals(${tk(p.key)}=${p.value})`;
    case 'numericAtLeast':
      return `NumericAtLeast(${tk(p.key)} >= ${p.threshold})`;
    case 'numericAtMost':
      return `NumericAtMost(${tk(p.key)} <= ${p.threshold})`;
    case 'numericInRange':
      return `NumericInRange(${tk(p.key)} in [${p.min}, ${p.max}])`;
    case 'semverAtLeast':
      return `SemverAtLeast(${tk(p.key)} >= ${p.version})`;
    case 'semverAtMost':
      return `SemverAtMost(${tk(p.key)} <= ${p.version})`;
    case 'semverCompatible':
      return `SemverCompatible(${tk(p.key)} ~= ${p.version})`;
    case 'stringPrefix':
      return `StringPrefix(${tk(p.key)} starts with ${dbg(p.prefix)})`;
    case 'stringMatches':
      return `StringMatches(${tk(p.key)} contains ${dbg(p.pattern)})`;
    case 'metadataExists':
      return `MetadataExists(${p.key})`;
    case 'metadataEquals':
      return `MetadataEquals(${p.key}=${p.value})`;
    case 'metadataMatches':
      return `MetadataMatches(${p.key} contains ${dbg(p.pattern)})`;
    case 'metadataNumericAtLeast':
      return `MetadataNumericAtLeast(${p.key} >= ${p.threshold})`;
    case 'and':
      return `And(${p.children.length} clauses)`;
    case 'or':
      return `Or(${p.children.length} clauses)`;
    case 'not':
      return 'Not';
  }
}

/**
 * Stable sort by `static_cost` ascending. Mirrors Rust's
 * `sort_by_key` (stable). Children with equal cost preserve their
 * declaration order.
 */
function planChildren(children: Predicate[]): Predicate[] {
  const indexed = children.map((c, i) => ({
    child: c,
    cost: predStaticCost(c),
    i,
  }));
  indexed.sort((a, b) => a.cost - b.cost || a.i - b.i);
  return indexed.map((x) => x.child);
}

/**
 * Evaluate a predicate against `(tags, metadata)` and produce a
 * trace tree.
 *
 * Mirrors the substrate's `Predicate::evaluate_with_trace`:
 * - `And` / `Or` children evaluated in cost-ascending order.
 * - Short-circuited siblings DON'T appear in the trace — operators
 *   see "the metadata clause failed; we never got to the GPU
 *   check."
 * - `Not`'s child carries the pre-negation result; `Not`'s own node
 *   carries the post-negation result.
 *
 * Pinned across bindings by `predicate_trace.json`. Useful for
 * client-side debugging of why a candidate did / didn't match
 * before hitting the wire.
 */
export function evaluatePredicateWithTrace(
  pred: Predicate,
  tags: readonly string[],
  metadata: Readonly<Record<string, string>>,
): { result: boolean; trace: ClauseTrace } {
  const label = predDebugLabel(pred);
  if (pred.type === 'and') {
    const ordered = planChildren(pred.children);
    const traces: ClauseTrace[] = [];
    let result = true;
    for (const c of ordered) {
      const { result: r, trace } = evaluatePredicateWithTrace(c, tags, metadata);
      traces.push(trace);
      if (!r) {
        result = false;
        break;
      }
    }
    return { result, trace: { label, result, children: traces } };
  }
  if (pred.type === 'or') {
    const ordered = planChildren(pred.children);
    const traces: ClauseTrace[] = [];
    let result = false;
    for (const c of ordered) {
      const { result: r, trace } = evaluatePredicateWithTrace(c, tags, metadata);
      traces.push(trace);
      if (r) {
        result = true;
        break;
      }
    }
    return { result, trace: { label, result, children: traces } };
  }
  if (pred.type === 'not') {
    const { result: r, trace } = evaluatePredicateWithTrace(
      pred.child,
      tags,
      metadata,
    );
    return {
      result: !r,
      trace: { label, result: !r, children: [trace] },
    };
  }
  const r = evalLeaf(pred, tags, metadata);
  return { result: r, trace: { label, result: r, children: [] } };
}

// ============================================================================
// PredicateDebugReport — aggregate per-clause stats over a corpus.
//
// Mirrors the substrate's `PredicateDebugReport::from_evaluations`:
// each candidate is evaluated via `evaluatePredicateWithTrace`; the
// returned trace is walked post-order to update per-label
// ClauseStats. Stats keyed by label so structurally-identical
// clauses across different positions in the AST collapse to one
// entry.
//
// Pinned across bindings by `predicate_debug_report.json`.
// ============================================================================

/**
 * Per-clause aggregated stats. Mirrors the substrate's `ClauseStats`.
 */
export interface ClauseStats {
  /** Clause label — same string as `ClauseTrace.label`. */
  label: string;
  /** Number of candidates that reached this clause (not short-circuited). */
  evaluated: number;
  /** Number of those evaluations that returned `true`. */
  matched: number;
}

/**
 * Wire-format debug report. The `clause_stats` array is sorted by
 * label (BTreeMap semantics in the substrate); bindings produce that
 * canonical order.
 */
export interface PredicateDebugReport {
  total_candidates: number;
  matched: number;
  clause_stats: ClauseStats[];
}

/** Wire-format evaluation context — what `evaluate*` consumes. */
export interface EvalContextWire {
  tags: string[];
  metadata: Record<string, string>;
}

function accumulateTrace(
  trace: ClauseTrace,
  stats: Map<string, ClauseStats>,
): void {
  const entry = stats.get(trace.label) ?? {
    label: trace.label,
    evaluated: 0,
    matched: 0,
  };
  entry.evaluated += 1;
  if (trace.result) entry.matched += 1;
  stats.set(trace.label, entry);
  for (const child of trace.children) {
    accumulateTrace(child, stats);
  }
}

/**
 * Run `pred` against each context in `contexts`, accumulating
 * per-clause hit / miss stats. Mirrors the substrate's
 * `PredicateDebugReport::from_evaluations`.
 *
 * The returned report's `clause_stats` is sorted by label
 * (BTreeMap semantics) so bindings produce byte-identical output
 * for the same input corpus.
 */
export function predicateDebugReport(
  pred: Predicate,
  contexts: readonly EvalContextWire[],
): PredicateDebugReport {
  const stats = new Map<string, ClauseStats>();
  let matched = 0;
  for (const ctx of contexts) {
    const { result, trace } = evaluatePredicateWithTrace(
      pred,
      ctx.tags,
      ctx.metadata,
    );
    if (result) matched += 1;
    accumulateTrace(trace, stats);
  }
  const sortedLabels = Array.from(stats.keys()).sort();
  return {
    total_candidates: contexts.length,
    matched,
    clause_stats: sortedLabels.map((l) => stats.get(l)!),
  };
}

/**
 * Redact metadata-clause values in a {@link PredicateDebugReport}.
 *
 * Walks the report's `clause_stats` and rewrites any label whose
 * metadata key is in the supplied `keys` list:
 *
 * - `MetadataEquals(<key>=<value>)` → `MetadataEquals(<key>=<redacted>)`
 * - `MetadataMatches(<key> contains "<pattern>")` → `MetadataMatches(<key> contains "<redacted>")`
 * - `MetadataNumericAtLeast(<key> >= <threshold>)` → `MetadataNumericAtLeast(<key> >= <redacted>)`
 * - `MetadataExists(<key>)` — unchanged (no value to redact)
 * - All non-metadata labels (Exists, Equals, Numeric*, Semver*,
 *   String*, And, Or, Not on tags) unchanged.
 *
 * After rewriting, stats with the same redacted label are merged
 * (`evaluated` and `matched` summed). Output is sorted by label.
 *
 * Use this before persisting a debug report to disk or sharing with
 * a teammate when the predicate's authored metadata values are
 * sensitive (PII, secrets, internal classifications).
 *
 * Pinned across bindings by `predicate_debug_report_redacted.json`.
 */
export function redactMetadataKeys(
  report: PredicateDebugReport,
  keys: readonly string[],
): PredicateDebugReport {
  const keySet = new Set(keys);
  const merged = new Map<string, ClauseStats>();
  for (const stat of report.clause_stats) {
    const newLabel = redactLabel(stat.label, keySet);
    const existing = merged.get(newLabel) ?? {
      label: newLabel,
      evaluated: 0,
      matched: 0,
    };
    existing.evaluated += stat.evaluated;
    existing.matched += stat.matched;
    merged.set(newLabel, existing);
  }
  const sortedLabels = Array.from(merged.keys()).sort();
  return {
    total_candidates: report.total_candidates,
    matched: report.matched,
    clause_stats: sortedLabels.map((l) => merged.get(l)!),
  };
}

/**
 * Pre-compiled regexes for the three redactable metadata-clause
 * label shapes. The `^` / `$` anchors prevent accidental matches in
 * pathological clause values.
 */
const META_EQUALS_RE = /^MetadataEquals\(([^=]+)=(.+)\)$/;
const META_MATCHES_RE = /^MetadataMatches\((.+) contains "(.*)"\)$/;
const META_NUMERIC_RE = /^MetadataNumericAtLeast\((.+) >= (.+)\)$/;

function redactLabel(label: string, keys: ReadonlySet<string>): string {
  let m: RegExpMatchArray | null;
  if ((m = label.match(META_EQUALS_RE))) {
    if (keys.has(m[1])) return `MetadataEquals(${m[1]}=<redacted>)`;
  } else if ((m = label.match(META_MATCHES_RE))) {
    if (keys.has(m[1])) return `MetadataMatches(${m[1]} contains "<redacted>")`;
  } else if ((m = label.match(META_NUMERIC_RE))) {
    if (keys.has(m[1])) return `MetadataNumericAtLeast(${m[1]} >= <redacted>)`;
  }
  return label;
}

/**
 * Reconstruct a {@link PredicateDebugReport} from its wire JSON form
 * (the shape produced by JSON-stringifying the report). Validates
 * required fields; on malformed input throws a descriptive error.
 *
 * Symmetric inverse of `JSON.stringify(report)` — call
 * `predicateDebugReportFromWire(JSON.parse(text))` to round-trip a
 * report through disk.
 */
export function predicateDebugReportFromWire(wire: unknown): PredicateDebugReport {
  if (
    typeof wire !== 'object' ||
    wire === null ||
    typeof (wire as PredicateDebugReport).total_candidates !== 'number' ||
    typeof (wire as PredicateDebugReport).matched !== 'number' ||
    !Array.isArray((wire as PredicateDebugReport).clause_stats)
  ) {
    throw new Error(
      'predicateDebugReportFromWire: expected { total_candidates: number, matched: number, clause_stats: array }',
    );
  }
  const w = wire as PredicateDebugReport;
  for (const s of w.clause_stats) {
    if (
      typeof s.label !== 'string' ||
      typeof s.evaluated !== 'number' ||
      typeof s.matched !== 'number'
    ) {
      throw new Error(
        `predicateDebugReportFromWire: bad clause_stats entry ${JSON.stringify(s)}`,
      );
    }
  }
  return {
    total_candidates: w.total_candidates,
    matched: w.matched,
    clause_stats: w.clause_stats.map((s) => ({
      label: s.label,
      evaluated: s.evaluated,
      matched: s.matched,
    })),
  };
}

/** Render a one-line-per-clause text summary suitable for CLI output. */
export function renderDebugReport(report: PredicateDebugReport): string {
  const pct = (num: number, denom: number): string =>
    denom === 0 ? '0.0%' : `${((100 * num) / denom).toFixed(1)}%`;
  const lines: string[] = [];
  lines.push('Predicate evaluation report');
  lines.push('─────────────────────────────────────────');
  lines.push(`Total candidates: ${report.total_candidates}`);
  lines.push(
    `Matched:          ${report.matched} (${pct(report.matched, report.total_candidates)})`,
  );
  lines.push('');
  lines.push('Per-clause stats (alphabetical):');
  for (const s of report.clause_stats) {
    lines.push(
      `  ${s.label.padEnd(60)} evaluated ${String(s.evaluated).padStart(5)}, ` +
        `matched ${String(s.matched).padStart(5)} (${pct(s.matched, s.evaluated)})`,
    );
  }
  return lines.join('\n') + '\n';
}
