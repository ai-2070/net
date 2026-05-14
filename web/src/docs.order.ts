import type { DocsOrderConfig } from "@/lib/docs";

// Custom ordering for the /docs sidebar.
//
// - `sections` controls the order of top-level folders (anything missing
//   appends in alpha order after).
// - `folders[<slug-path>]` controls the order of a folder's children
//   (files and nested folders mixed, again alpha-fallback for unlisted).
//
// Slugs are lowercased filenames-without-`.md` and lowercased folder names.
// Folder keys use the full slug path: `"releases"`, `"plans/nested"`, etc.
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
};
