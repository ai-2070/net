#!/usr/bin/env node
// Regenerates `src/content/docs/releases/` from the release notes in the
// crate.
//
// The notes are authored at `net/crates/net/docs/releases/` and were, until
// this script, hand-copied into the site. Two copies of 35 files drift, and
// the copy nobody edits is the one readers see. The site copy differs from
// the source in exactly two mechanical ways, both of which belong in a
// transform rather than in a human's memory:
//
//   1. Frontmatter. The site needs `title` / `description`; the crate copy
//      shouldn't carry web metadata. Existing frontmatter is preserved so
//      curated titles ("v0.27 — Purple Rain") survive a resync; only new
//      files get a generated default.
//   2. Links. The notes reference sibling docs by relative path
//      (`../plans/FOO.md`, `../../LICENSE-APACHE`) — correct in the repo,
//      404s on the site, since those files were never published. They're
//      rewritten to GitHub blob URLs.
//
// Run: `npm run sync:releases`  (or `-- --check` to fail on drift, as CI does)

import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const WEB = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = resolve(WEB, "..");
const SRC = join(REPO, "net", "crates", "net", "docs", "releases");
const DEST = join(WEB, "src", "content", "docs", "releases");
const GH = "https://github.com/ai-2070/net/blob/master";

// Authored alongside the release notes but not published: the checklist, the
// beta notes, and a stray v0.8 draft superseded by the real v0.8 notes.
const NOT_PUBLISHED = new Set([
  "BETA_NOTES.md",
  "RELEASE_STEPS.md",
  "RELEASE_v0.8_NOTES.md",
]);

// Paths the notes reference that moved, or that were always one directory
// off. Applied before the generic resolution below.
const LEGACY = [
  [
    "../misc/DATAFORTS_PLAN.md",
    `${GH}/net/crates/net/docs/plans/DATAFORTS_PLAN.md`,
  ],
  ["../sdk/README.md#", `${GH}/net/crates/net/sdk/README.md#`],
  ["../sdk-py/README.md#", `${GH}/net/crates/net/sdk-py/README.md#`],
  ["../sdk-ts/README.md#", `${GH}/net/crates/net/sdk-ts/README.md#`],
  ["../../include/README.md#", `${GH}/net/crates/net/include/README.md#`],
  ["../../../../../go/README.md#", `${GH}/go/README.md#`],
  ["../../../LICENSE-APACHE", `${GH}/LICENSE-APACHE`],
  [
    "misc/BUG_AUDIT_2026_05_03_MESH.md",
    `${GH}/net/crates/net/docs/misc/BUG_AUDIT_2026_05_03_MESH.md`,
  ],
];

// Curated display titles, keyed by source filename. These are the sidebar
// and <title> text, and they are NOT derivable from the filename:
// `CHIPPIN_IN` is "Chippin' In", `EYE_OF_THE_TIGER` is "Eye of the Tiger"
// (small words stay lowercase), and `RELEASE_v0.20.2.md` carries no codename
// at all yet is "v0.20.2 — Smoke on the Water".
//
// They live here rather than in the generated files because the generated
// files are output: previously the script read titles back out of its own
// destination, so the source of truth was the artifact. A from-scratch
// regeneration silently degraded 14 of 35 titles. Add a line here when you
// add a release; `deriveTitle` is the fallback, not the mechanism.
const TITLES = {
  "RELEASE_v0.10_HEX.md": "v0.10 — Hex",
  "RELEASE_v0.11_BLACK_DIAMOND.md": "v0.11 — Black Diamond",
  "RELEASE_v0.12_FIRESTARTER.md": "v0.12 — Firestarter",
  "RELEASE_v0.13_CHIPPIN_IN.md": "v0.13 — Chippin' In",
  "RELEASE_v0.14_THE_WARRIORS.md": "v0.14 — The Warriors",
  "RELEASE_v0.15_REBEL_YELL.md": "v0.15 — Rebel Yell",
  "RELEASE_v0.16_EYE_OF_THE_TIGER.md": "v0.16 — Eye of the Tiger",
  "RELEASE_v0.17_ATOMIC_PLAYBOYS.md": "v0.17 — Atomic Playboys",
  "RELEASE_v0.18_WELCOME_TO_THE_JUNGLE.md": "v0.18 — Welcome to the Jungle",
  "RELEASE_v0.19_PUSH_IT_TO_THE_LIMIT.md": "v0.19 — Push It to the Limit",
  "RELEASE_v0.20.2.md": "v0.20.2 — Smoke on the Water",
  "RELEASE_v0.20_SMOKE_ON_THE_WATER.md": "v0.20 — Smoke on the Water",
  "RELEASE_v0.21_RADAR_LOVE.md": "v0.21 — Radar Love",
  "RELEASE_v0.22_ALL_ALONG_THE_WATCHTOWER.md":
    "v0.22 — All Along the Watchtower",
  "RELEASE_v0.23_GIMME_SHELTER.md": "v0.23 — Gimme Shelter",
  "RELEASE_v0.24_MONEY_FOR_NOTHING.md": "v0.24 — Money For Nothing",
  "RELEASE_v0.25_SHOCK_TO_THE_SYSTEM.md": "v0.25 — Shock To The System",
  "RELEASE_v0.26_MONKEY_BUSINESS.md": "v0.26 — Monkey Business",
  "RELEASE_v0.27.1_PURPLE_RAIN.md": "v0.27.1 — Purple Rain",
  "RELEASE_v0.27.2_PURPLE_RAIN.md": "v0.27.2 — Purple Rain",
  "RELEASE_v0.27.3_PURPLE_RAIN.md": "v0.27.3 — Purple Rain",
  "RELEASE_v0.27.4_PURPLE_RAIN.md": "v0.27.4 — Purple Rain",
  "RELEASE_v0.27.5_PURPLE_RAIN.md": "v0.27.5 — Purple Rain",
  "RELEASE_v0.27.6_PURPLE_RAIN.md": "v0.27.6 — Purple Rain",
  "RELEASE_v0.27.7_PURPLE_RAIN.md": "v0.27.7 — Purple Rain",
  "RELEASE_v0.27_PURPLE_RAIN.md": "v0.27 — Purple Rain",
  "RELEASE_v0.28_ROUND_AND_ROUND.md": "v0.28.0 — Round and Round",
  "RELEASE_v0.29.1_SUMMER_OF_69.md": "v0.29.1 — Summer of '69",
  "RELEASE_v0.29_SUMMER_OF_69.md": "v0.29.0 — Summer of '69",
  "RELEASE_v0.30_FINAL_COUNTDOWN.md": "v0.30.0 — Final Countdown",
  "RELEASE_v0.31_HOLD_THE_LINE.md": "v0.31.0 — Hold the Line",
  "RELEASE_v0.32_SUMMER_MADNESS.md": "v0.32.0 — Summer Madness",
  "RELEASE_v0.33_CIRCUS_MAXIMUS.md": "v0.33.0 — Circus Maximus",
  "RELEASE_v0.8_KILLING_MOON.md": "v0.8 — Killing Moon",
  "RELEASE_v0.9_FIRST_BLOOD.md": "v0.9 — First Blood",
};

// `RELEASE_v0.27_PURPLE_RAIN` → `v0.27 — Purple Rain`. Fallback for a
// release with no entry in TITLES — correct for simple codenames, wrong for
// anything with an apostrophe or a small word, so prefer an explicit entry.
function deriveTitle(filename) {
  const m = /^RELEASE_v([\d.]+)_(.+)\.md$/i.exec(filename);
  if (!m) return filename.replace(/\.md$/, "");
  const words = m[2]
    .split("_")
    .map((w) => w.charAt(0) + w.slice(1).toLowerCase())
    .join(" ");
  return `v${m[1]} — ${words}`;
}

// `RELEASE_v0.29_SUMMER_OF_69.md` → `/docs/releases/release-v0.29-summer-of-69`
function releaseUrl(filename) {
  return `/docs/releases/${filename.replace(/\.md$/, "").toLowerCase().replace(/_/g, "-")}`;
}

function rewriteLinks(source, siblings) {
  let out = source;
  for (const [from, to] of LEGACY) out = out.split(`(${from}`).join(`(${to}`);

  // A bare sibling reference points at another release note, and those *are*
  // published — so it becomes a site link rather than a GitHub one. Handled
  // before the relative-path pass, which would otherwise send a reader off
  // to GitHub for a page that's one click away.
  out = out.replace(
    /(\[[^\]]*\]\()([A-Za-z0-9._]+\.md)(#[^)\s]*)?(\))/g,
    (all, pre, file, frag, post) =>
      siblings.has(file)
        ? `${pre}${releaseUrl(file)}${frag ?? ""}${post}`
        : all,
  );

  // Anything still relative resolves against the notes' directory in the
  // repo — `../plans/X.md` from `docs/releases/` is `docs/plans/X.md`.
  return out.replace(
    /(\[[^\]]*\]\()(\.{1,2}\/[^)\s]+)(\))/g,
    (_all, pre, href, post) => {
      const [target, frag] = href.split("#");
      const parts = ["net", "crates", "net", "docs", "releases"];
      for (const seg of target.split("/")) {
        if (seg === "" || seg === ".") continue;
        if (seg === "..") parts.pop();
        else parts.push(seg);
      }
      const url = `${GH}/${parts.join("/")}${frag ? `#${frag}` : ""}`;
      return `${pre}${url}${post}`;
    },
  );
}

// Every published note, so a bare `FOO.md` reference can be recognized as a
// sibling rather than guessed at.
const published = new Set(
  readdirSync(SRC).filter((n) => n.endsWith(".md") && !NOT_PUBLISHED.has(n)),
);

const check = process.argv.includes("--check");
const drifted = [];
let written = 0;

for (const name of readdirSync(SRC).sort()) {
  if (!name.endsWith(".md") || NOT_PUBLISHED.has(name)) continue;

  const body = rewriteLinks(readFileSync(join(SRC, name), "utf8"), published);
  const destPath = join(DEST, name);

  // Generated wholly from TITLES — never read back from the destination, so
  // the output is a pure function of the source plus this script.
  const title = TITLES[name] ?? deriveTitle(name);
  const frontmatter =
    `---\ntitle: "${title}"\n` +
    `description: "Release notes for Net ${title} — what shipped, what changed, ` +
    `and what it means for compatibility."\n---\n`;

  const next = frontmatter + body;
  const current = existsSync(destPath) ? readFileSync(destPath, "utf8") : null;
  if (current === next) continue;

  if (check) drifted.push(name);
  else {
    writeFileSync(destPath, next);
    written++;
  }
}

if (check) {
  if (drifted.length) {
    console.error(
      `✗ ${drifted.length} release note(s) out of sync with the crate:\n` +
        drifted.map((d) => `  ${d}`).join("\n") +
        `\n\nRun \`npm run sync:releases\` and commit the result.`,
    );
    process.exit(1);
  }
  console.error("✓ release notes in sync with the crate");
} else {
  console.error(
    written ? `✓ synced ${written} release note(s)` : "✓ already up to date",
  );
}
