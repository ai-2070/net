# Release process

This folder holds the **release process** docs. The public release notes
themselves live in the docs site and are no longer mirrored here.

| Doc | What it is |
|---|---|
| [`RELEASE_STEPS.md`](RELEASE_STEPS.md) | The ordered checklist for cutting a release |
| [`BETA_NOTES.md`](BETA_NOTES.md) | Package-version matrix for beta cuts |
| [`RELEASE_v0.8_NOTES.md`](RELEASE_v0.8_NOTES.md) | The v0.8 tagging commands (historical) |

## Release notes

Published notes for every version live at
[`web/src/content/docs/releases/`](../../../../../web/src/content/docs/releases/)
and render on the docs site under **Releases**.

That directory is the **single source of truth**. Notes were previously
maintained in both places by hand; the two copies had already diverged
structurally (the site copies carry frontmatter and absolute links) and
nothing kept them in sync. Write the note once, in `web/`.

When adding a note, also register it in
[`web/src/docs.order.ts`](../../../../../web/src/docs.order.ts) under
`folders.releases` (newest first) and `labels` — otherwise it renders but
sorts alphabetically in the sidebar.
