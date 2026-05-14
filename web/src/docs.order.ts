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
// `"releases/release_v0.17_atomic_playboys"`. All keys are matched
// case-insensitively — author them however reads best.
export const DOCS_ORDER: DocsOrderConfig = {
  sections: ["plans", "releases", "misc"],
  folders: {
    // Releases — newest first.
    releases: [
      "release_v0.17_atomic_playboys",
      "release_v0.16_eye_of_the_tiger",
      "release_v0.15_rebel_yell",
      "release_v0.14_the_warriors",
      "release_v0.13_chippin_in",
      "release_v0.12_firestarter",
      "release_v0.11_black_diamond",
      "release_v0.10_hex",
      "release_v0.8_killing_moon",
      "release_steps",
    ],
  },
  // hide: ["misc", "plans/draft_notes"],
  // labels: {
  //   releases: "Release Notes",
  //   "releases/release_v0.17_atomic_playboys": "v0.17 — Atomic Playboys",
  // },
};
