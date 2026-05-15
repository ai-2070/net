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
  sections: ["plans", "releases", "misc"],
  folders: {
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
    hide: ["release-steps"],
  },
  hide: [
    "misc",
    "plans",
    "releases/release-steps",
    "releases/Release-V0.8-Notes",
  ],
  // labels: {
  //   releases: "Release Notes",
  //   "releases/release-v0.17-atomic-playboys": "v0.17 — Atomic Playboys",
  // },
};
