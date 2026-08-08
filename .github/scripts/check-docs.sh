#!/usr/bin/env bash
#
# API-correctness checks for the docs site (`web/src/content/docs/`).
#
# `web.yml` already checks that internal links resolve and that the Astro site
# type-checks. Neither says anything about whether the *documented API is real*.
# This does — the same three cheap checks that found real defects in the agent
# skills, pointed at a corpus twice the size.
#
# It was written after measuring: current docs came back 0/39 on enum variants
# and 0 on cited paths, i.e. much healthier than the skills were. But not clean.
# In one sitting these found:
#
#   - a C signature with two invented out-params (`net_ingest_raw_batch`)
#   - `net cap query --tag gpu --tag vram:24`, wrong in three ways at once
#   - three more `net <verb>` invocations naming a binary called `net-mesh`
#
# RELEASE NOTES ARE EXCLUDED, deliberately. They record what was true at the
# time; `BlobRef::MAX` was real at v0.15 and has since been renamed. A check
# that forces edits to them corrupts the record rather than fixing anything.
#
# ONE NARROW EXCEPTION: verbs that never coexisted with the old binary name. The
# rename to `net-mesh` landed 2026-05-19 (38647c447); the MCP-bridge verbs
# arrived with v0.31 on 2026-07-04, seven weeks later. So `net wrap` is not a
# dated record of anything — it was never runnable at any version — while
# `net admin` in the v0.18 note IS correct for its date and must stay. The v0.31
# note carried the unrunnable form in ten places, including its own "New CLI"
# line and its upgrade steps, and this blanket exclusion is why it survived a
# full audit cycle. POST_RENAME_VERBS below closes exactly that gap, nothing
# wider.
#
# Run locally:  .github/scripts/check-docs.sh
# Exit 0 = the docs agree with the tree. Exit 1 = something drifted.

set -uo pipefail

cd "$(dirname "$0")/../.."
# Relative to the repo root we just entered — see `check-skills.sh` for why an
# absolute path breaks the Python checkers under Git Bash.
CHECKER_DIR=".github/scripts"
DOCS="web/src/content/docs"
EXCLUDE="/releases/"
# `note`, `ok`, `fail`, `$TMP`, a resolved `$PYTHON`, and `run_checker`.
. "$CHECKER_DIR/lib/checker.sh"

docs_files() {
  find "$DOCS" -name "*.md" -o -name "*.mdx" | grep -v "$EXCLUDE"
}

# ------------------------------------------------------------ cited repo paths
# Resolved through git, not the filesystem: a developer's tree holds build
# output the repo does not, so an `[ -e ]` test passes locally and fails on a
# clean checkout — backwards for a guard.
echo "==> Source paths cited by the docs"
before=$fail
while read -r p; do
  [ -z "$p" ] && continue
  git ls-files --error-unmatch "$p" >/dev/null 2>&1 && continue
  [ -n "$(git ls-files "$p" | head -1)" ] && continue
  if [ -e "$p" ]; then
    note "cited path is not tracked by git (build artifact?): $p"
  else
    note "cited path does not exist: $p"
  fi
done < <(grep -ohE '`(net|go|web)/[A-Za-z0-9_/.-]+`' $(docs_files) 2>/dev/null \
         | tr -d '`' | sed 's/:[0-9,-]*$//' | sort -u)
[ "$fail" -eq "$before" ] && ok "every cited repo path is tracked in git"

# ------------------------------------------------- enum variants + identifiers
# Shares the skills' checker, pointed at this corpus with its own allowlist, so
# one document set's deliberate absence cannot silence another's real defect.
echo "==> Enum variants and metric/config identifiers"
before=$fail
run_checker check-skill-refs.py --corpus "$DOCS" --exclude "$EXCLUDE"
[ "$fail" -eq "$before" ] && ok "documented variants and identifiers all resolve"

# ------------------------------------------------------------------ CLI verbs
# The installed binary is `net-mesh` (cli/Cargo.toml `[[bin]]`). The verb list
# is derived from the CLI's own `Command` enum rather than hand-maintained, so a
# new subcommand is covered the day it lands.
#
# Matched only inside backticks or at the start of a line, because several verbs
# are ordinary English next to "net": `net daemon`, `net log`, `net node`. A
# looser pattern flags the prose "talks only to the local net daemon".
echo "==> CLI invocations"
before=$fail
VERBS=$(grep -oE "^    [A-Z][A-Za-z]*\(" net/crates/net/cli/src/main.rs \
        | tr -d ' (' | tr 'A-Z' 'a-z' | sort -u | tr '\n' '|' | sed 's/|$//')
if [ -z "$VERBS" ]; then
  note "could not derive CLI verbs from cli/src/main.rs — the enum moved"
else
  while read -r hit; do
    [ -n "$hit" ] && note "bare 'net' CLI invocation (the binary is net-mesh): $hit"
  done < <(grep -rnE "(\`|^|\\$ )net ($VERBS)\b" $(docs_files) 2>/dev/null || true)
fi

# The check above excludes release notes and keeps excluding them for every verb
# that predates the rename. These three do not: they shipped with the MCP bridge
# in v0.31, so no note can legitimately pair them with the old binary name, and
# no allowlist is needed.
POST_RENAME_VERBS="wrap|mcp|forwarding"
while read -r hit; do
  [ -n "$hit" ] && note "post-rename verb with the old binary name: $hit"
done < <(grep -rnE "(\`|^|\$ )net ($POST_RENAME_VERBS)\b" \
           "$DOCS"/releases/*.md net/crates/net/docs/releases/*.md \
           2>/dev/null || true)
[ "$fail" -eq "$before" ] && ok "all CLI invocations use net-mesh (post-rename verbs checked in releases too)"

# ------------------------------------------------- PermissionToken wire size
# Scans the whole tracked tree, not just `$DOCS` — the stale sizes lived in
# binding docstrings and C headers, where nothing else looks. Release notes,
# internal plans, and audits are excluded inside the checker for the same
# reason this file excludes them: they are dated records.
echo "==> PermissionToken wire size"
before=$fail
run_checker check-token-wire-size.py
[ "$fail" -eq "$before" ] && ok "every documented token size matches WIRE_SIZE"

echo
if [ "$fail" -eq 0 ]; then
  echo "Docs agree with the tree."
  echo "(Release notes excluded — they are dated records, not current claims.)"
  exit 0
fi
echo "Docs drifted from the tree — $fail problem(s) above."
exit 1
