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
# Run locally:  .github/scripts/check-docs.sh
# Exit 0 = the docs agree with the tree. Exit 1 = something drifted.

set -uo pipefail

cd "$(dirname "$0")/../.."
DOCS="web/src/content/docs"
EXCLUDE="/releases/"
fail=0

# A counter, not a flag — each section's success line compares `fail` against
# `before`, and as a 0/1 flag the first failure made every later section print a
# green tick it had not earned.
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail + 1)); }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

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
while read -r line; do
  [ -n "$line" ] && note "$line"
done < <(python3 "$(dirname "$0")/check-skill-refs.py" \
           --corpus "$DOCS" --exclude "$EXCLUDE" || true)
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
  done < <(grep -rnE "(\`|^|\\\$ )net ($VERBS)\b" $(docs_files) 2>/dev/null || true)
fi
[ "$fail" -eq "$before" ] && ok "all CLI invocations use net-mesh"

echo
if [ "$fail" -eq 0 ]; then
  echo "Docs agree with the tree."
  echo "(Release notes excluded — they are dated records, not current claims.)"
  exit 0
fi
echo "Docs drifted from the tree — $fail problem(s) above."
exit 1
