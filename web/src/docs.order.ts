import type { DocsOrderConfig } from "@/lib/docs";

// Custom ordering, hiding, and labelling for the /docs sidebar.
//
// - `sections` orders top-level folders. Missing ones append alpha after.
// - `folders[<slug-path>]` orders a folder's children (files + nested
//   folders mixed). Missing ones append alpha after.
// - `hide` removes entries from the sidebar entirely. Folders cascade —
//   hiding `misc` also makes everything under `misc/` unreachable.
// - `labels` overrides the auto-titleized name for any entry, shown in
//   the sidebar, breadcrumbs, and folder/page headers.
//
// Slug paths use lowercased filenames-without-`.md` and lowercased folder
// names, joined by `/`: `"releases"`, `"plans/nested"`,
// `"releases/release-v0.17-atomic-playboys"`. All keys are matched
// case-insensitively, and `_` / `-` are interchangeable — so
// `"release_v0.17_atomic_playboys"` and `"release-v0.17-atomic-playboys"`
// resolve to the same entry. Dashes are the canonical (URL) form.
export const DOCS_ORDER: DocsOrderConfig = {
  sections: ["start", "concepts", "guides", "reference", "tutorials", "releases"],
  folders: {
    start: ["what-is-net", "quickstart", "install"],
    concepts: [
      "architecture",
      "channels",
      "events-and-causality",
      "identity",
      "capabilities",
      "subnets",
      "storage-stack",
    ],
    guides: [
      "event-bus",
      "nrpc",
      "durable-logs",
      "cortex-folds",
      "netdb-queries",
      "dataforts",
      "daemons-and-placement",
      "continuity-and-migration",
      "nat-and-traversal",
    ],
    reference: [
      "eventbus-api",
      "adapter-trait",
      "filter-dsl",
      "subprotocol-ids",
      "capability-schema",
      "wire-format",
      "replication-config",
      "error-codes",
    ],
    tutorials: [
      "fleet-telemetry",
      "distributed-daemon",
      "event-sourced-service",
    ],
    // Releases — newest first.
    releases: [
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
  labels: {
    // Sections
    start: "Start",
    concepts: "Concepts",
    guides: "Guides",
    reference: "Reference",
    tutorials: "Tutorials",
    releases: "Releases",

    // Start
    "start/what-is-net": "What is Net?",
    "start/quickstart": "Quickstart",
    "start/install": "Install",

    // Concepts
    "concepts/architecture": "Architecture",
    "concepts/channels": "Channels",
    "concepts/events-and-causality": "Events and Causality",
    "concepts/identity": "Identity",
    "concepts/capabilities": "Capabilities",
    "concepts/subnets": "Subnets",
    "concepts/storage-stack": "The Storage Stack",

    // Guides
    "guides/event-bus": "Using the Event Bus",
    "guides/nrpc": "Typed RPC with nRPC",
    "guides/durable-logs": "Durable Logs (RedEX)",
    "guides/cortex-folds": "Folded State (CortEX)",
    "guides/netdb-queries": "Querying with NetDB",
    "guides/dataforts": "Blob Storage (Dataforts)",
    "guides/daemons-and-placement": "Daemons and Placement",
    "guides/continuity-and-migration": "Continuity and Migration",
    "guides/nat-and-traversal": "NAT and Traversal",

    // Reference
    "reference/eventbus-api": "EventBus API",
    "reference/adapter-trait": "Adapter Trait",
    "reference/filter-dsl": "Filter DSL",
    "reference/subprotocol-ids": "Subprotocol Registry",
    "reference/capability-schema": "Capability Schema",
    "reference/wire-format": "Wire Format",
    "reference/replication-config": "Replication Configuration",
    "reference/error-codes": "Error Codes",

    // Tutorials
    "tutorials/fleet-telemetry": "Fleet Telemetry",
    "tutorials/distributed-daemon": "Daemon With Failover",
    "tutorials/event-sourced-service": "Event-Sourced Service",

    // Releases
    "releases/release-v0.17-atomic-playboys": "v0.17 — Atomic Playboys",
    "releases/release-v0.16-eye-of-the-tiger": "v0.16 — Eye of the Tiger",
    "releases/release-v0.15-rebel-yell": "v0.15 — Rebel Yell",
    "releases/release-v0.14-the-warriors": "v0.14 — The Warriors",
    "releases/release-v0.13-chippin-in": "v0.13 — Chippin' In",
    "releases/release-v0.12-firestarter": "v0.12 — Firestarter",
    "releases/release-v0.11-black-diamond": "v0.11 — Black Diamond",
    "releases/release-v0.10-hex": "v0.10 — Hex",
    "releases/release-v0.9-first-blood": "v0.9 — First Blood",
    "releases/release-v0.8-killing-moon": "v0.8 — Killing Moon",
  },
};
