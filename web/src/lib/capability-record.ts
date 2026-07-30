import "server-only";
import record from "@/lib/generated/capability-record.json";
import { LENSES, LENS_SLUG, type Language } from "./docs-language";

// The parity record, as the site sees it.
//
// `capability-record.json` is GENERATED from `docs/data/capabilities/*.yaml` by
// `.github/scripts/capability_records.py --write`, and `--check` fails the build
// when it drifts. The site has no YAML parser and the docs build is fully
// static, so this JSON is the bridge D5 called for — the thing that lets a
// rendition state a support status without a human typing one into a page.
//
// The distinction that makes this worth the machinery: a page can be wrong about
// a binding, and a page that is wrong about a binding is worse than a page that
// says nothing. The record is checked against the bindings' own trees (every
// positive cell resolves a real symbol). A sentence in a markdown file is not.

export type CapabilityStatus =
  | "supported"
  | "partial"
  | "experimental"
  | "not exposed"
  | "n/a";

/** Qualifies a status. `core-only` is the load-bearing one: the operation
 *  exists, but only on the low-level binding — not the ergonomic wrapper. */
export type CapabilityMode = "poll" | "verify-only" | "core-only";

export type CapabilityCell = { status: CapabilityStatus; mode?: CapabilityMode };

/** One row of the record: every binding's answer for one operation. */
export type CapabilityRow = {
  operation: string;
  domain: string;
  cells: Array<{ binding: string; lang: Language | null; cell: CapabilityCell }>;
};

// The record's binding column names are the reader-facing labels ("Node / TS"),
// while the lens taxonomy uses ids (`ts`). One map, so a rendition can mark
// which column is the reader's own without either side guessing.
const BINDING_LANG: Record<string, Language> = {
  Rust: "rust",
  "Node / TS": "ts",
  Python: "python",
  Go: "go",
  C: "c",
};

type Bridge = {
  bindings: string[];
  operations: Record<
    string,
    { domain: string; bindings: Record<string, CapabilityCell> }
  >;
};

const bridge = record as unknown as Bridge;

/** The record's row for one operation, or null if it names none.
 *
 * Returning null rather than throwing is deliberate: a page with no
 * `capability:` is the normal case, and the checker — not the renderer — is
 * where a page that names a nonexistent operation gets caught. */
export function capabilityRow(operation: string | undefined): CapabilityRow | null {
  if (!operation) return null;
  const entry = bridge.operations[operation];
  if (!entry) return null;
  return {
    operation,
    domain: entry.domain,
    cells: bridge.bindings
      .filter((b) => entry.bindings[b] !== undefined)
      .map((b) => ({
        binding: b,
        lang: BINDING_LANG[b] ?? null,
        cell: entry.bindings[b]!,
      })),
  };
}

/** The lens whose column a rendition should highlight. */
export function lensLanguage(lensSlug: string): Language | null {
  for (const lens of LENSES) if (LENS_SLUG[lens] === lensSlug) return lens;
  return lensSlug === LENS_SLUG.c ? "c" : null;
}
