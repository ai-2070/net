#!/usr/bin/env bash
#
# Drift check for the agent skills in `.claude/skills/`.
#
# The skills are a shadow copy of the SDK's public surface: ~9k lines of prose
# asserting that specific symbols, paths, and CLI verbs exist. Nothing compiles
# them, so they rot silently while the code moves underneath. This script is the
# tripwire. What it caught when it was written:
#
#   - a CLI renamed `net` -> `net-mesh`, still documented the old way 57 times
#   - `net/README.md` cited 19 times; that file has never existed
#   - two source paths that had become files
#   - `payment_gate`, `RpcAppError`, `BlobRef::MAX_SIZE` and
#     `RedexFileConfig::with_blob_max_size` documented as API — none exist
#   - an nRPC error table where four of six variants were invented, and the
#     one real spelling (`Cancelled`) was given with one `l`
#
# Run locally:  .github/scripts/check-skills.sh
# Exit 0 = skills agree with the tree. Exit 1 = something drifted.

set -uo pipefail

cd "$(dirname "$0")/../.."
SKILLS=".claude/skills"
fail=0

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=1; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

# ---------------------------------------------------------------- frontmatter
echo "==> Frontmatter"
want_version=$(grep -m1 '^version' net/crates/net/sdk/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
for skill in "$SKILLS"/*/SKILL.md; do
  name=$(basename "$(dirname "$skill")")
  for key in name description allowed-tools; do
    grep -q "^$key:" "$skill" || note "$name: SKILL.md missing '$key:' in frontmatter"
  done
  got=$(grep -m1 '^  net-version:' "$skill" | awk '{print $2}')
  if [ -n "$got" ] && [ "$got" != "$want_version" ]; then
    note "$name: metadata.net-version is $got, workspace is $want_version"
  fi
  # A description is loaded into context for every session in every project
  # where the skill is installed. Keep it a budget, not a dumping ground.
  len=$(python3 - "$skill" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
m = re.search(r'^description:\s*"(.*?)"\s*$', t, re.S | re.M)
print(len(m.group(1)) if m else 0)
PY
)
  [ "$len" -gt 3000 ] && note "$name: description is $len chars (budget 3000)"
done
[ "$fail" -eq 0 ] && ok "frontmatter keys, net-version, description budget"

# --------------------------------------------------------- cross-file links
echo "==> Cross-references between skill files"
before=$fail
for dir in "$SKILLS"/*/; do
  while read -r f; do
    [ -n "$f" ] && [ ! -f "$dir$f" ] && note "$(basename "$dir"): references $f, which does not exist"
  done < <(grep -oh '`[a-z0-9-]*\.md`' "$dir"*.md 2>/dev/null | tr -d '`' | sort -u)
done
[ "$fail" -eq "$before" ] && ok "every referenced *.md resolves"

# --------------------------------------------------------- repo source paths
# Checked against git, not the filesystem. A developer's tree holds build output
# the repo does not — `bindings/node/*.js` is gitignored `tsc` output beside the
# tracked `.ts` — so an `[ -e ]` test passes locally and fails on a clean CI
# checkout, which is exactly backwards: it lets a citation to a generated file
# through on the machine where it was written.
echo "==> Source paths cited by the skills"
before=$fail
while read -r p; do
  git ls-files --error-unmatch "$p" >/dev/null 2>&1 && continue   # tracked file
  [ -n "$(git ls-files "$p" | head -1)" ] && continue             # tracked directory
  if [ -e "$p" ]; then
    note "cited path is not tracked by git (build artifact?): $p"
  else
    note "cited path does not exist: $p"
  fi
done < <(grep -ohE '`(net|go|web)/[A-Za-z0-9_/.-]+`' "$SKILLS"/*.md "$SKILLS"/*/*.md \
         | tr -d '`' | sed 's/:[0-9,-]*$//' | sort -u)
[ "$fail" -eq "$before" ] && ok "every cited repo path is tracked in git"

# ------------------------------------------------------------ symbol canaries
# Counts that the prose depends on. A drop means the SDK churned underneath the
# docs; a rise usually means a new variant nobody documented yet.
echo "==> API surface canaries"
canary() { # <label> <expected> <count-command...>
  local label=$1 expect=$2; shift 2
  local got; got=$("$@" 2>/dev/null || echo 0)
  if [ "$got" != "$expect" ]; then
    note "$label: expected $expect, found $got"
  else
    ok "$label = $expect"
  fi
}
count_re() { grep -cE "$1" "$2"; }

canary "sdk emit* methods (apis.md)" 5 \
  count_re '^\s*pub fn emit' net/crates/net/sdk/src/net.rs
canary "SdkError variants (apis.md, runtime.md, error-codes.md)" 13 \
  count_re '^\s*(Shutdown|Ingestion|Sampled|Unrouted|Poll|Adapter|Serialization|Config|NoMesh|Backpressure|NotConnected|ChannelRejected|Traversal)\b' \
  net/crates/net/sdk/src/error.rs

# Symbols the skills name as callable API. Each must exist as a definition.
echo "==> Symbols documented as callable"
before=$fail
for sym in \
  serve_rpc_typed call_typed find_best_node find_nodes_scoped \
  publish_island_topology match_islands reserve_island claim_island \
  serve_a2a submit_task cancel_task derive_child_seed serve_org \
  gated_invoke serve_payments wait_for_token fetch_blob store_dir
do
  grep -rqE "fn $sym\b" --include="*.rs" net/crates 2>/dev/null \
    || note "documented symbol has no Rust definition: $sym"
done
[ "$fail" -eq "$before" ] && ok "all documented symbols resolve"

# ------------------------------------------------- enum variants + identifiers
# Both checks come from one read of the source tree; see the script's docstring
# for what each catches and why symbol-existence alone was not enough.
echo "==> Enum variants and metric/config identifiers"
before=$fail
while read -r line; do
  [ -n "$line" ] && note "$line"
done < <(python3 "$(dirname "$0")/check-skill-refs.py" || true)
[ "$fail" -eq "$before" ] && ok "documented variants and identifiers all resolve"

# ------------------------------------------------------------------ CLI verbs
# The single installed binary is `net-mesh` (cli/Cargo.toml [[bin]]). A bare
# `net <verb>` in the skills is a command the user cannot run.
echo "==> CLI invocations"
before=$fail
while read -r hit; do
  [ -n "$hit" ] && note "bare 'net' CLI invocation (the binary is net-mesh): $hit"
done < <(grep -rnE '(^|[^-[:alnum:]/])net (wrap|mcp|forwarding|org|node adopt|typegen|transfer)\b' \
         "$SKILLS"/*.md "$SKILLS"/*/*.md || true)
[ "$fail" -eq "$before" ] && ok "all CLI invocations use net-mesh"

# ------------------------------------------------------- internal plan leakage
# The skills ship publicly via ai-2070/net-claude-skill. Roadmap vocabulary
# (P0/P1/P5 rungs, "Mode E", plan phase numbers, internal branch names) is
# meaningless to an external reader and must not leak in.
echo "==> Internal planning vocabulary"
before=$fail
while read -r hit; do
  [ -n "$hit" ] && note "internal planning reference: $hit"
done < <(grep -rnE '\b(P[0-9]|WS[0-9])\b|Mode E|bugfixes-[0-9]|docs/internal|_PLAN\.md|RELEASE_v' \
         "$SKILLS"/*.md "$SKILLS"/*/*.md || true)
[ "$fail" -eq "$before" ] && ok "no internal plan/release references"

echo
if [ "$fail" -eq 0 ]; then
  echo "Skills agree with the tree."
else
  echo "Skills drifted from the tree — see above."
fi
exit "$fail"
