import type { DocsOrderConfig } from "@/lib/docs";

// Custom ordering, hiding, and labelling for the /docs sidebar.
//
// - `sections` orders top-level folders. Missing ones append alpha after.
// - `folders[<slug-path>]` orders a folder's children (files + nested
//   folders mixed). Missing ones append alpha after.
// - `hide` removes entries from the sidebar entirely. Folders cascade —
//   hiding `misc` also makes everything under `misc/` unreachable.
// - `labels` names entries that have no file to carry frontmatter (a
//   folder with no README). A page's own `title:` frontmatter wins over
//   this map; the fallback is the titleized filename.
//
// Slug paths use lowercased filenames-without-`.md` and lowercased folder
// names, joined by `/`: `"releases"`, `"plans/nested"`,
// `"releases/release-v0.17-atomic-playboys"`. All keys are matched
// case-insensitively, and `_` / `-` are interchangeable — so
// `"release_v0.17_atomic_playboys"` and `"release-v0.17-atomic-playboys"`
// resolve to the same entry. Dashes are the canonical (URL) form.
export const DOCS_ORDER: DocsOrderConfig = {
  sections: [
    "start",
    "sdk",
    "guides",
    "tutorials",
    "concepts",
    "agent-briefs",
    "payments",
    "reference",
    "worldview",
    "releases",
  ],
  folders: {
    sdk: ["rust", "typescript", "python", "go", "c"],
    "sdk/rust": [
      "quickstart",
      "announce",
      "discover",
      "invoke",
      "watch",
      "artifacts",
      "errors",
    ],
    "sdk/typescript": [
      "quickstart",
      "announce",
      "discover",
      "invoke",
      "watch",
      "artifacts",
      "errors",
    ],
    "sdk/python": [
      "quickstart",
      "announce",
      "discover",
      "invoke",
      "watch",
      "artifacts",
      "errors",
    ],
    "sdk/go": [
      "quickstart",
      "announce",
      "discover",
      "invoke",
      "watch",
      "artifacts",
      "errors",
    ],
    // C doesn't follow the announce/discover/invoke spine — it's ten headers
    // across five libraries, so it's organised by boundary concern instead.
    "sdk/c": [
      "quickstart",
      "headers-and-linking",
      "memory-and-threading",
      "errors",
    ],
    "agent-briefs": [
      "wrap-and-use-an-mcp-server",
      "build-a-recoverable-capability",
      "generate-typed-tool-bindings",
    ],
    worldview: [
      "agentic-mesh",
      "right-and-wrong-use-cases",
      "mcp-vs-net",
      "rest-vs-net",
    ],
    payments: [
      "what-net-payments-is",
      "x402-and-net",
      "the-lifecycle",
      "verification-tiers",
      "spend-policy-and-approvals",
      "non-custodial-signing",
      "networks",
      "failure-schematic",
      "billing",
    ],
    start: ["what-is-net", "install", "quickstart", "claude-skills"],
    concepts: [
      "architecture",
      "identity",
      "capabilities",
      "channels",
      "events-and-causality",
      "agent-identity",
      "organizations",
      "security-model",
      "tool-federation",
      "subnets",
      "storage-stack",
    ],
    guides: [
      // Core communication surfaces.
      "event-bus",
      "discover-and-invoke",
      "nrpc",
      "private-capabilities",
      "mesh-streams",
      // Integrations and agent handoff.
      "wrap-mcp-server",
      "expose-net-as-mcp",
      "agent-to-agent",
      // Delivery, recovery, and durable work.
      "submitted-is-not-completed",
      "recover-failed-workflow",
      "task-lifecycle",
      "durable-logs",
      // Materialized state and artifacts.
      "cortex-folds",
      "netdb-queries",
      "dataforts",
      // Placement, continuity, and production operations.
      "daemons-and-placement",
      "gang-scheduler",
      "continuity-and-migration",
      "nat-and-traversal",
      "production-deployment",
      "troubleshooting",
    ],
    reference: [
      // User and operator surfaces first.
      "cli",
      "deck",
      "eventbus-api",
      "capability-schema",
      "mcp-bridge",
      "filter-dsl",
      "error-codes",
      // Extension, persistence, and protocol internals.
      "adapter-trait",
      "replication-config",
      "redis-dedup",
      "subprotocol-ids",
      "wire-format",
      "versioning",
      "glossary",
    ],
    tutorials: [
      "fleet-telemetry",
      "distributed-daemon",
      "event-sourced-service",
    ],
    // Releases — newest first.
    releases: [
      "RELEASE_v0.33_CIRCUS_MAXIMUS",
      "RELEASE_v0.32_SUMMER_MADNESS",
      "RELEASE_v0.31_HOLD_THE_LINE",
      "RELEASE_v0.30_FINAL_COUNTDOWN",
      "RELEASE_v0.29.1_SUMMER_OF_69",
      "RELEASE_v0.29_SUMMER_OF_69",
      "RELEASE_v0.28_ROUND_AND_ROUND",
      "RELEASE_v0.27.7_PURPLE_RAIN",
      "RELEASE_v0.27.6_PURPLE_RAIN",
      "RELEASE_v0.27.5_PURPLE_RAIN",
      "RELEASE_v0.27.4_PURPLE_RAIN",
      "RELEASE_v0.27.3_PURPLE_RAIN",
      "RELEASE_v0.27.2_PURPLE_RAIN",
      "RELEASE_v0.27.1_PURPLE_RAIN",
      "RELEASE_v0.27_PURPLE_RAIN",
      "RELEASE_v0.26_MONKEY_BUSINESS",
      "RELEASE_v0.25_SHOCK_TO_THE_SYSTEM",
      "RELEASE_v0.24_MONEY_FOR_NOTHING",
      "RELEASE_v0.23_GIMME_SHELTER",
      "RELEASE_v0.22_ALL_ALONG_THE_WATCHTOWER",
      "RELEASE_v0.21_RADAR_LOVE",
      "release-v0.20.2",
      "release-v0.20-smoke-on-the-water",
      "release-v0.19-push-it-to-the-limit",
      "release-v0.18-welcome-to-the-jungle",
      "release-v0.17-atomic-playboys",
      "release-v0.16-eye-of-the-tiger",
      "release-v0.15-rebel-yell",
      "release-v0.14-the-warriors",
      "release-v0.13-chippin-in",
      "release-v0.12-firestarter",
      "release-v0.11-black-diamond",
      "release-v0.10-hex",
      "release-v0.9-first-blood",
      "release-v0.8-killing-moon",
    ],
  },
  // Titles live in each page's frontmatter (`title:`), which `lib/docs.ts`
  // reads while walking the tree. This map used to carry one hand-written
  // label per page — 144 of them — duplicating what the page already knew
  // and silently going stale whenever someone added a doc and forgot the
  // second file. Only entries with no file of their own need one now:
  // folders that have no README to carry frontmatter.
  labels: {
    releases: "Releases",
    sdk: "SDKs",
  },
  languages: {
    // D7 — two reference pages are Rust-native content sitting in a
    // language-neutral section. `adapter-trait` documents a Rust trait you cannot
    // implement from Go or TypeScript; `eventbus-api` documents the Rust types.
    // Their physical path and canonical URL do not move (inbound links, and three
    // reference pages were ported into the skill corpus) — only the navigation
    // owner changes, which is exactly the mechanism DOCS_STRATEGY_PLAN.md
    // prescribes for a reclassification.
    //
    // NOT gated, deliberately: `redis-dedup` carries per-language helpers for all
    // five bindings, and `replication-config` documents protocol-level knobs. Both
    // read as Rust-native by fence count and are not.
    "reference/adapter-trait": ["rust"],
    "reference/eventbus-api": ["rust"],
    // Each SDK spine is visible under its language pill. Rust is the default.
    "sdk/rust": ["rust"],
    "sdk/typescript": ["ts"],
    "sdk/python": ["python"],
    "sdk/go": ["go"],
    "sdk/c": ["c"],
  },
};
