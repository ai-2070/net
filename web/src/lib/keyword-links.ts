// The curated keyword → page map for automatic linking in docs prose.
//
// Every `term` is a literal that, on its FIRST occurrence in a page's body,
// renders as a link to `slug`. Nothing in the source markdown changes: this is
// a render-time pass in `remark-keyword-links.ts`, so the content stays plain
// markdown for the readers that consume it raw (agents, the skill corpus) and
// for GitHub.
//
// It is a hand-written map on purpose. Auto-deriving terms from page titles
// looks free until "Start", "Install" and "Networks" start linking mid-sentence.
// Curation trades a one-line edit per new term for links that are always
// intended, and `assertKeywordLinksResolve` fails the build when a `slug` stops
// existing — so a rename cannot leave a keyword pointing at nothing.
//
// Matching is case-insensitive: `RedEX`, `Redex`, and `redex` all link, and the
// visible text is the spelling as written. One entry per term — do not add a
// second casing of a term you already listed (the build assertion rejects it).
// Prefer the spelling that appears in the docs; the map is a term list, not a
// casing list.
export type KeywordLink = {
  /** Literal text to match in prose. */
  term: string;
  /** Docs slug path — see `doc-index.ts`. Must resolve, or the build fails. */
  slug: string;
};

// Ordered longest-first where one term contains another (`MCP bridge` before
// `MCP`) so the longer, more specific name wins at a shared position. The
// plugin sorts anyway; the grouping here is for the reader.
export const KEYWORD_LINKS: readonly KeywordLink[] = [
  // ---- concepts -----------------------------------------------------------
  { term: "capability federation", slug: "concepts/tool-federation" },
  { term: "tool federation", slug: "concepts/tool-federation" },
  { term: "capabilities", slug: "concepts/capabilities" },
  { term: "channels", slug: "concepts/channels" },
  { term: "causal links", slug: "concepts/events-and-causality" },
  { term: "Organizations", slug: "concepts/organizations" },
  { term: "agent identity", slug: "concepts/agent-identity" },
  { term: "storage stack", slug: "concepts/storage-stack" },
  { term: "security model", slug: "concepts/security-model" },
  { term: "subnets", slug: "concepts/subnets" },
  { term: "WebRTC", slug: "concepts/webrtc-transport" },

  // ---- the storage and daemon stack ---------------------------------------
  { term: "RedEX", slug: "guides/durable-logs" },
  { term: "CortEX", slug: "guides/cortex-folds" },
  { term: "NetDB", slug: "guides/netdb-queries" },
  { term: "Dataforts", slug: "guides/dataforts" },

  // ---- guides -------------------------------------------------------------
  { term: "nRPC", slug: "guides/nrpc" },
  { term: "gang scheduler", slug: "guides/gang-scheduler" },
  { term: "backpressure", slug: "guides/mesh-streams" },
  { term: "task lifecycle", slug: "guides/task-lifecycle" },
  { term: "NAT traversal", slug: "guides/nat-and-traversal" },
  { term: "private capabilities", slug: "guides/private-capabilities" },
  { term: "agent-to-agent", slug: "guides/agent-to-agent" },

  // ---- payments -----------------------------------------------------------
  { term: "x402", slug: "payments/x402-and-net" },
  { term: "verification tiers", slug: "payments/verification-tiers" },
  { term: "spend policy", slug: "payments/spend-policy-and-approvals" },
  { term: "non-custodial signing", slug: "payments/non-custodial-signing" },

  // ---- reference ----------------------------------------------------------
  { term: "wire format", slug: "reference/wire-format" },
  { term: "error codes", slug: "reference/error-codes" },
  { term: "filter DSL", slug: "reference/filter-dsl" },
  { term: "subprotocol", slug: "reference/subprotocol-ids" },
  { term: "adapter trait", slug: "reference/adapter-trait" },
  { term: "capability schema", slug: "reference/capability-schema" },
  { term: "MCP bridge", slug: "reference/mcp-bridge" },
  { term: "MCP", slug: "reference/mcp-bridge" },
];
