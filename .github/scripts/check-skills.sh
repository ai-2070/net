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

# The corpus root. Overridable *only* so `check-skills-depth.sh` can point this
# same script at a throwaway copy: the depth guarantee needs a probe file, and a
# probe planted in the real corpus would be one interrupted run away from being
# rsynced to users. Everything else — cited paths, git resolution, the source
# tree — is still read relative to the repo root.
SKILLS="${SKILLS_DIR:-.claude/skills}"
fail=0

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# A counter, not a flag. Each section reports success with
# `[ "$fail" -eq "$before" ]`, which is only meaningful if `note` keeps
# incrementing — as a 0/1 flag the first failure made every *later* section
# print its green tick, because `before` and `fail` were both 1.
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail + 1)); }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

# Run one of the sibling Python checkers. Findings arrive on stdout, one per
# line, and each becomes a `note`.
#
# Anything on stderr is treated as a failure of the CHECKER, not of the corpus.
# Without that, a checker that dies before reporting anything writes zero
# findings, `fail` never moves, and the `[ "$fail" -eq "$before" ]` line below
# prints a green tick for a check that did not run. That is not hypothetical:
# on a cp1252 shell, `check-skill-vocab.py` and `check-skill-refs.py` raised
# UnicodeDecodeError on the first source file containing an em-dash — every
# run, reported as success. The `|| true` these calls need (a checker exits
# non-zero when it has findings, and `pipefail` is on) is exactly what hid it.
run_checker() {
  local script="$1" err out
  err="$TMP/$(basename "$script").err"
  out=$(python3 "$(dirname "$0")/$script" 2>"$err" || true)
  if [ -n "$out" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] && note "$line"
    done <<<"$out"
  fi
  if [ -s "$err" ]; then
    note "$script did not run to completion: $(tail -1 "$err")"
    sed 's/^/      /' "$err" >&2
  fi
}

# The corpus, at any depth. One definition, used by every check below, so a file
# is never visible to some checks and invisible to others — three of them used
# to stop at `*/*.md`, which a `bindings/rust.md` sits one level below.
#
# `find`, not `git ls-files`: publication is `rsync -a --delete` over the skill
# directory, so an untracked file on disk ships to users exactly like a tracked
# one. The checker has to see what the publisher would copy. (Cited *targets*
# are still resolved through git — that is the opposite direction and wants the
# opposite rule.)
skill_md() { find "$SKILLS" -type f -name '*.md' | sort; }

if [ -z "$(skill_md)" ]; then
  echo "  ✗ no markdown found under $SKILLS — the checker would pass vacuously" >&2
  exit 1
fi

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
  #
  # `encoding="utf-8"` is load-bearing, not decoration. Python's default
  # encoding is the platform's, so on a cp1252 Windows shell each em-dash in
  # these descriptions decoded as three characters instead of one — enough to
  # put net-event-bus 11 over budget locally while CI, reading UTF-8, saw it
  # 9 under. A checker whose verdict depends on the developer's locale is
  # worse than no checker: it trains people to ignore it.
  len=$(python3 - "$skill" <<'PY'
import re, sys
t = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'^description:\s*"(.*?)"\s*$', t, re.S | re.M)
print(len(m.group(1)) if m else 0)
PY
)
  [ "$len" -gt 3000 ] && note "$name: description is $len chars (budget 3000)"
done
[ "$fail" -eq 0 ] && ok "frontmatter keys, net-version, description budget"

# --------------------------------------------------------- cross-file links
# A reference resolves as a sibling of the citing file or at its skill root —
# the two places a reader would look. For a top-level file those are the same
# directory, so this is unchanged for the flat corpus and correct for a nested
# one, where `bindings/rust.md` naming `coverage.md` means its sibling.
echo "==> Cross-references between skill files"
before=$fail
while read -r f; do
  [ -z "$f" ] && continue
  dir=$(dirname "$f")
  root=$(printf '%s' "$f" | sed -E "s#^($SKILLS/[^/]+)/.*#\1#")
  [ -d "$root" ] || root="$SKILLS"
  while read -r ref; do
    [ -z "$ref" ] && continue
    [ -f "$dir/$ref" ] && continue
    [ -f "$root/$ref" ] && continue
    # A slashed reference is only ours if its leading directory exists in this
    # skill. `bindings/coverage.md` is a corpus reference and gets checked;
    # `specs/x402-specification-v2.md` names a file in the x402-foundation repo
    # at a pinned commit, and no amount of looking will find it here. Checking
    # by whether the directory exists keeps external citations out without an
    # allowlist — at the cost that a typo in the directory name reads as
    # external. Better than the previous rule, which extracted no slashed
    # reference at all.
    case "$ref" in
      */*)
        lead=${ref%%/*}
        [ -d "$dir/$lead" ] || [ -d "$root/$lead" ] || continue
        ;;
    esac
    # The corpus index sits at `$SKILLS/README.md` and routes *into* the skills,
    # so its references resolve one level down. It cannot say which skill —
    # `gotchas.md`, `concepts.md` and `testing.md` each exist in both — so at the
    # corpus root the rule relaxes to "exists somewhere in the corpus". Weaker
    # than the rule inside a skill, and only applied where a skill root is not a
    # meaningful frame of reference.
    if [ "$dir" = "$SKILLS" ] &&
       [ -n "$(find "$SKILLS" -type f -name "$ref" -print -quit)" ]; then
      continue
    fi
    note "${f#$SKILLS/}: references $ref, which is neither a sibling nor at the skill root"
    # `_` and `/` are in the class deliberately. Without `_` a reference to an
    # underscored filename was never extracted; without `/` the same was true of
    # every nested reference — `bindings/coverage.md` is the first of those, and
    # it would have been an invisible citation rather than a checked one.
  done < <(grep -oh '`[a-z0-9_/-]*\.md`' "$f" 2>/dev/null | tr -d '`' | sort -u)
done < <(skill_md)
[ "$fail" -eq "$before" ] && ok "every referenced *.md resolves"

# --------------------------------------------------------- repo source paths
# Checked against git, not the filesystem. A developer's tree holds build output
# the repo does not — `bindings/node/*.js` is gitignored `tsc` output beside the
# tracked `.ts` — so an `[ -e ]` test passes locally and fails on a clean CI
# checkout, which is exactly backwards: it lets a citation to a generated file
# through on the machine where it was written.
#
# Files a build produces are the one legitimate exception, and the list of them
# lives in `check-skill-source-paths.py` (GENERATED) so there is a single record
# with a reason attached to each. Before this was shared, the same file got two
# verdicts: cited with a line anchor it passed here (the regex below cannot match
# a `:`), cited without one it failed.
echo "==> Source paths cited by the skills"
before=$fail
GENERATED=$(python3 "$(dirname "$0")/check-skill-source-paths.py" --generated)
while read -r p; do
  git ls-files --error-unmatch "$p" >/dev/null 2>&1 && continue   # tracked file
  [ -n "$(git ls-files "$p" | head -1)" ] && continue             # tracked directory
  printf '%s\n' "$GENERATED" | grep -qxF "$p" && continue         # build-generated
  if [ -e "$p" ]; then
    note "cited path is not tracked by git (build artifact?): $p"
  else
    note "cited path does not exist: $p"
  fi
done < <(grep -ohE '`(net|go|web)/[A-Za-z0-9_/.-]+`' $(skill_md) \
         | tr -d '`' | sed 's/:[0-9,-]*$//' | sort -u)
[ "$fail" -eq "$before" ] && ok "every cited repo path is tracked in git"

# ------------------------------------------------------------ workflow wiring
# Two ways this whole apparatus can be real and still useless: never running, or
# running and not blocking. So the workflow itself is checked — every path the
# skills cite must be watched by a trigger, and every job must gate `publish` (or
# say why it does not). A check that fires on nothing, or that goes red while the
# mirror publishes anyway, reads as coverage while providing none.
echo "==> Workflow wiring (triggers + publish gating)"
before=$fail
run_checker check-skill-workflow.py
[ "$fail" -eq "$before" ] && ok "triggers cover every cited path; every job gates publish"

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
run_checker check-skill-refs.py
[ "$fail" -eq "$before" ] && ok "documented variants and identifiers all resolve"

# ------------------------------------------------- cross-language vocabularies
# Frozen string vocabularies that are single-sourced across four bindings and
# reproduced as tables in the skills. Readers pattern-match on these, and they
# drift silently — the nRPC kind table was missing two real wire kinds.
echo "==> Cross-language vocabularies"
before=$fail
run_checker check-skill-vocab.py
[ "$fail" -eq "$before" ] && ok "documented vocabularies match every binding"

# ------------------------------------------------------------ coverage matrices
# The per-skill binding matrices are the one place "does binding X support
# operation Y" is maintained. This verifies their declared evidence anchors and
# holds the status/mode vocabulary closed — it does not, and cannot, verify
# completeness. See the script's docstring for why absence is not inferred.
#
# The matrices are now GENERATED from `docs/data/capabilities/*.yaml`, so this
# defers to `capability_records.py --check`, which validates the record and then
# proves each skill's copy still matches it. Two structural checks the old
# file-only checker made — that both tables carry the same columns, and the same
# operations in the same order — became impossible to violate rather than
# unchecked: one record renders both tables.
echo "==> Binding coverage matrices"
before=$fail
if ! python3 "$(dirname "$0")/capability_records.py" --check >/tmp/cap.$$ 2>&1; then
  sed 's/^/  /' /tmp/cap.$$
  fail=$((fail + 1))
fi
rm -f /tmp/cap.$$
[ "$fail" -eq "$before" ] && ok "coverage records validate; every generated copy matches"

# ------------------------------------------------------------------ CLI verbs
# The single installed binary is `net-mesh` (cli/Cargo.toml [[bin]]). A bare
# `net <verb>` in the skills is a command the user cannot run.
echo "==> CLI invocations"
before=$fail
while read -r hit; do
  [ -n "$hit" ] && note "bare 'net' CLI invocation (the binary is net-mesh): $hit"
done < <(grep -nE '(^|[^-[:alnum:]/])net (wrap|mcp|forwarding|org|node adopt|typegen|transfer)\b' \
         $(skill_md) || true)
[ "$fail" -eq "$before" ] && ok "all CLI invocations use net-mesh"

# ------------------------------------------------------- internal plan leakage
# The skills ship publicly via ai-2070/net-claude-skill. Roadmap vocabulary
# (P0/P1/P5 rungs, "Mode E", plan phase numbers, internal branch names) is
# meaningless to an external reader and must not leak in.
echo "==> Internal planning vocabulary"
before=$fail
while read -r hit; do
  [ -n "$hit" ] && note "internal planning reference: $hit"
done < <(grep -nE '\b(P[0-9]|WS[0-9])\b|Mode E|bugfixes-[0-9]|docs/internal|_PLAN\.md|RELEASE_v' \
         $(skill_md) || true)
[ "$fail" -eq "$before" ] && ok "no internal plan/release references"

echo
if [ "$fail" -eq 0 ]; then
  echo "Skills agree with the tree."
  exit 0
fi
echo "Skills drifted from the tree — $fail problem(s) above."
exit 1
