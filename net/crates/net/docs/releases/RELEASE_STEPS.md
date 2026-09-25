0. Update docs

1. Update version

   Every published artifact carries ONE version (`X` below, e.g. `0.37.0`).

   - Rust: change `[workspace.package] version` in `net/crates/net/Cargo.toml`
     — every workspace crate inherits it (`version.workspace = true`).
   - Still written out, because those ecosystems cannot inherit it: every
     `version = "..."` on a path dependency onto one of our own crates (in
     every `Cargo.toml`, standalone projects included), `leaf/Cargo.toml`,
     the npm `package.json`s and their `@net-mesh/*` pins, the Python
     `pyproject.toml`s (version + `net-*` dependency bounds) and both
     `__version__` strings.
   - Edit those fields, NOT a global find-and-replace: history, fixtures and
     test data also mention versions and must not move.
   - Refresh the lockfiles (`cargo update -w` in `net/crates/net/` and in each
     standalone project that has its own `Cargo.lock`).
   - Verify: `python3 .github/scripts/check-versions.py` must print
     "All published versions agree" (CI runs it too).
   - Install pages (`web/src/content/docs/start/install/`) name the newest
     release TAG, not the candidate: bumping them before the `vX` tag exists
     fails `web.yml` (`check-install-version.py`). Update them with the
     release, and expect that check red until step 6's first tag is pushed.

2. Update codename

3. Merge version branch

4. Draft release note

   `net/crates/net/docs/releases/RELEASE_vX.Y_<CODENAME>.md`, then mirror it
   to the website: `npm run sync:releases` from `web/` (`npm run
   check:releases` gates CI).

5. Pull master

   Tag from master AFTER the version branch is merged: the install-page check
   only counts a tag reachable from the deployed docs commit, and the release
   workflows build what the tag points at.

6. Run commands

`X` is the FULL version — `0.37.0`, not `0.37` — so the tags are `v0.37.0`,
`crates-v0.37.0`, `cli-v0.37.0`, …

**Order matters for two of them:** `cli-vX` and `deck-vX` publish crates that
depend on `net-mesh-sdk`, `net-mesh` and `net-mesh-mcp` at `X`, which must
already be on crates.io. Push `crates-vX` and wait for its workflow to finish
(and the crates.io index to show `X`) BEFORE pushing `cli-vX` and `deck-vX`.
The other tags are independent of each other.

```bash
# Main release. Also runs install-version.yml, which checks the install
# pages against this tag.
git tag vX && git push origin vX

# Rust crates → crates.io. One tag, one workflow, publishing in
# dependency order: net-mesh-wire (the tokio-free wire layer, added
# in Stage 2 — net-mesh depends on it by version, so it MUST go
# first), then net-mesh + net-mesh-sdk-macros, then net-mesh-sdk,
# then net-mesh-mcp.
git tag crates-vX && git push origin crates-vX

# Python binding wheels → PyPI (`net-mesh`)
git tag python-vX && git push origin python-vX

# Python SDK → PyPI (`net-mesh-sdk`)
git tag pypi-sdk-vX && git push origin pypi-sdk-vX

# Node binding → npm (`@net-mesh/core`)
git tag node-vX && git push origin node-vX

# TS SDK → npm (`@net-mesh/sdk`)
git tag npm-sdk-vX && git push origin npm-sdk-vX

# ── ONLY after the crates-vX workflow has finished (see above) ──

# CLI → crates.io (`net-cli`) + GitHub Release tarballs +
# npm (`@net-mesh/cli`) + PyPI (`net-mesh-cli`).
# One tag fans out to four parallel workflows.
git tag cli-vX && git push origin cli-vX

# Deck → crates.io (`net-deck`) + GitHub Release tarballs +
# npm (`@net-mesh/deck`) + PyPI (`net-deck`).
# One tag fans out to four parallel workflows.
git tag deck-vX && git push origin deck-vX
```

Not tagged, on purpose: `@net-mesh/browser`, `net-mesh-leaf`, `net-payments`
and `net-aggregator-daemon` are version-bumped with everything else but have no
release workflow and have never been published. Nothing needs to be done for
them here; publishing any of them is a new workflow, not a new tag.


If anything goes wrong during build:
```bash
# Delete a release tag — use the FULL tag name, e.g. cli-v0.37.0
git tag -d <tag> && git push origin --delete <tag>
```
