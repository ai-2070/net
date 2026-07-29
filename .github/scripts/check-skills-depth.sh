#!/usr/bin/env bash
#
# Regression test for the *depth* of `check-skills.sh`'s corpus discovery.
#
# Why this exists: three of that script's checks used to glob
# `"$SKILLS"/*.md "$SKILLS"/*/*.md`, which stops one level above a binding
# companion at `net-event-bus/bindings/rust.md`. Nothing failed — they simply
# stopped seeing the file. A checker that silently inspects less is worse than
# no checker, because the green tick is still there.
#
# The fix is one `find`-based `skill_md()`. A glob is one edit away from
# regressing, so the guarantee is pinned here rather than trusted.
#
# HOW IT AVOIDS SHIPPING ITS OWN PROBE
# A permanently-broken file under `.claude/skills/net-event-bus/bindings/` would
# be published: `skills.yml` does `rsync -a --delete` over the whole skill
# directory, so anything on disk reaches users. Nor is a temporary file safe —
# an interrupted run leaves it behind, and the next push mirrors it.
#
# So the probe is planted in a *copy* of the corpus under $TMPDIR and
# `check-skills.sh` is pointed at it with SKILLS_DIR. That still exercises the
# production discovery path — same script, same `skill_md()`, same greps — while
# the real `.claude/skills/` is never written to at all.
#
# Run locally:  .github/scripts/check-skills-depth.sh

set -uo pipefail

cd "$(dirname "$0")/../.."

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

CORPUS="$TMP/skills"
cp -R .claude/skills "$CORPUS"

# Depth 3 relative to the corpus root — exactly where Phase 2's companions land,
# and exactly what the old two-level glob could not see.
PROBE="$CORPUS/net-event-bus/bindings/_depth_probe.md"
mkdir -p "$(dirname "$PROBE")"
cat > "$PROBE" <<'EOF'
# depth probe

Four planted defects, one per check that used to stop at two levels.

Cited path: `net/crates/net/sdk/src/_depth_probe_missing.rs`
Cross-reference: see `depth-probe-no-such-companion.md`
CLI invocation: `net wrap ./server.py`
Internal planning: tracked in docs/internal/plans/_DEPTH_PROBE_PLAN.md
EOF

out=$(SKILLS_DIR="$CORPUS" .github/scripts/check-skills.sh 2>&1)
rc=$?

fail=0
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=1; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

echo "==> Nested content is reachable by every corpus-level check"

if [ "$rc" -eq 0 ]; then
  note "check-skills.sh exited 0 against a corpus containing four planted defects"
fi

# Each expectation names something unique to the probe, so an unrelated
# pre-existing failure in the real tree cannot satisfy one by accident. Matched
# as regexes with the line number left open — asserting `:8:` would make the
# test fail whenever someone adds a line to the probe, which teaches people to
# edit the assertion rather than believe it.
while IFS='|' read -r label pattern; do
  [ -z "$label" ] && continue
  if printf '%s' "$out" | grep -qE "$pattern"; then
    ok "$label"
  else
    note "$label — nested defect not reported (expected to match: $pattern)"
  fi
done <<'EOF'
cited repo paths reach depth 3|cited path does not exist: net/crates/net/sdk/src/_depth_probe_missing\.rs
cross-references reach depth 3|references depth-probe-no-such-companion\.md
CLI invocations reach depth 3|bindings/_depth_probe\.md:[0-9]+:CLI invocation
internal plan vocabulary reaches depth 3|bindings/_depth_probe\.md:[0-9]+:Internal planning
EOF

echo
if [ "$fail" -eq 0 ]; then
  echo "Discovery reaches nested skill content."
else
  echo "Discovery has regressed — nested skill content is invisible to some checks."
  echo
  echo "--- full output of the probed run ---"
  printf '%s\n' "$out"
fi
exit "$fail"
