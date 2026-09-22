#!/usr/bin/env python3
"""An ABI change lands Rust, both headers and a Go ABI test in ONE commit.

WHY THIS EXISTS. The rule (AGENTS.md, "Single cdylib rule (cgo)"): changing
any `extern "C"` signature or `NET_*` constant touches Rust, `include/*.h`,
`go/net.h` and the Go side in the same commit. The -117 constant
(`NET_ERR_MESH_SESSION_SUPERSEDED`) landed across three commits
(3c9fa8bde -> 70b008909 -> 0aa63e69f) with no commit touching the Go ABI
tests: `header_parity_test.go` was red between the first two, and until the
third a Go send receiving -117 surfaced as "mesh unknown error (code -117)".
History cannot be rewritten, and a bisect or revert inside such a window
yields a header pair that silently drifts — so the closure is this guard:
the next such commit fails loudly instead of passing in pieces.

WHAT TRIGGERS IT. A commit's diff changing a `NET_*` constant DEFINITION —
a Rust `const`/`static`, a `#define`, or an enum member — or an `extern
"C"` SIGNATURE under the FFI surface: the `extern "C"` line itself, any
continuation line of a multi-line signature (a parameter or return edit
such as `len: u32` -> `len: u64`, which changes no line that says
`extern "C"` at all), or a declaration inside an `extern "C" { … }`
block. Continuation lines are attributed to their signature through the
file text at the commit (`_extern_c_spans`), not the diff's context
lines, so an opener further than the context radius from the edit still
triggers. Mentions in comments and prose do not trigger, and neither do
`extern "C"` changes outside the FFI surface.

WHAT IT REQUIRES. The same commit must touch all four groups:

  1. the Rust FFI   - net/crates/net/src/ffi/**, bindings/go/net-ffi/**
  2. a C header     - net/crates/net/include/*.h
  3. a Go mirror    - go/net.h, go/net_cortex.h
  4. a Go ABI test  - go/abi_stability*_test.go, go/header_parity_test.go

WHAT IT DOES NOT CHECK. That the two headers agree constant-for-constant
(`header_parity_test.go` owns that), that one value is identical everywhere,
or that a new extern function is witnessed — only that the groups move
TOGETHER. Merge commits are skipped: the commits they merge are checked
individually (they are in the same range), but a conflict resolution that
introduces an ABI change of its own is not detected. A constant whose homes
grow beyond the file lists above needs those lists extended in the same
commit.

RANGE. `--range A..B` (default `HEAD^..HEAD`); in CI the step passes the
push's `before..sha`. When the base is unresolvable (a new branch or a
force push) the guard checks the tip commit and says so — an unevaluable
base must not silently retire the check.

Self-test: `check-abi-commit.py --self-test` drives every rejection path
against synthetic diffs.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

RUST_FFI_PREFIXES = (
    "net/crates/net/src/ffi/",
    "net/crates/net/bindings/go/net-ffi/",
)
C_HEADER_PREFIX = "net/crates/net/include/"
GO_HEADER_RE = re.compile(r"^go/[^/]+\.h$")
GO_ABI_TEST_RE = re.compile(r"^go/(abi_stability.*|header_parity)_test\.go$")

_RUST_CONST = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+(NET_[A-Z0-9_]+)\b"
)
_C_DEFINE = re.compile(r"^\s*#\s*define\s+(NET_[A-Z0-9_]+)\b")
_C_ENUM = re.compile(r"^\s*(NET_[A-Z0-9_]+)\s*=")
_EXTERN_C = re.compile(r'\bextern\s+"C"')
_EXTERN_C_BLOCK = re.compile(r'\bextern\s+"C"\s*\{\s*$')
_COMMENT_LINE = re.compile(r"^\s*(?://|/\*|\*)")
_HUNK = re.compile(r"^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@")

_GROUP_LABELS = {
    "rust": "the Rust FFI (net/crates/net/src/ffi/**, bindings/go/net-ffi/**)",
    "headers": "a C header (net/crates/net/include/*.h)",
    "go_headers": "a Go mirror header (go/net.h, go/net_cortex.h)",
    "go_tests": "a Go ABI test (go/abi_stability*_test.go, go/header_parity_test.go)",
}


def _error(message: str, *detail: str) -> None:
    print(f"::error::{message}")
    for line in detail:
        print(line)


def _diff_path(field: str) -> str | None:
    path = field.strip().split("\t", 1)[0]
    if path == "/dev/null":
        return None
    if path.startswith(("a/", "b/")):
        path = path[2:]
    return path or None


def _extern_c_spans(text: str) -> set[int]:
    """1-based line numbers inside an `extern "C"` signature or block.

    A function signature spans its `extern "C"` opener through the line
    that opens the body (`{`) or ends the declaration (`;`); an
    `extern "C" { … }` block spans its braces. Comment lines are not part
    of a span — a prose mention of `extern "C"` must never arm it.
    """
    spans: set[int] = set()
    sig = False
    block = False
    depth = 0
    for lineno, line in enumerate(text.splitlines(), 1):
        if _COMMENT_LINE.match(line):
            continue
        if block:
            spans.add(lineno)
            depth += line.count("{") - line.count("}")
            if depth <= 0:
                block = False
            continue
        if sig:
            spans.add(lineno)
            depth += line.count("(") - line.count(")")
            if depth <= 0:
                depth = 0
                if "{" in line or ";" in line:
                    sig = False
            continue
        if _EXTERN_C_BLOCK.search(line):
            spans.add(lineno)
            block = True
            depth = line.count("{") - line.count("}")
        elif _EXTERN_C.search(line):
            spans.add(lineno)
            sig = True
            depth = line.count("(") - line.count(")")
            if depth <= 0 and ("{" in line or ";" in line):
                sig = False
    return spans


def diff_changes(
    patch: str,
    sources: dict[str, tuple[str | None, str | None]] | None = None,
) -> tuple[set[str], set[str], bool]:
    """(touched files, changed `NET_*` constant names, ffi `extern "C"` hit).

    `sources` maps a path to its (pre-commit, post-commit) text. A changed
    line that lands INSIDE an `extern "C"` signature or block of the
    version it belongs to triggers even when the line itself never says
    `extern "C"` — a parameter on a continuation line (`len: u32` ->
    `len: u64`) changes no matched line and must still demand the four
    groups. Attribution runs on the file text rather than the diff's
    context lines, so an opener further from the edit than the context
    radius still reaches its continuation lines. Without `sources`
    (synthetic diffs) the trigger degrades to the line-local match.
    """
    files: set[str] = set()
    consts: set[str] = set()
    extern_c = False
    current = ""
    spans = {
        (path, side): _extern_c_spans(text or "")
        for path, (old, new) in (sources or {}).items()
        for side, text in (("old", old), ("new", new))
        if text is not None
    }
    old_ln = new_ln = 0
    for line in patch.splitlines():
        if line.startswith("--- "):
            old = _diff_path(line[4:])
            if old:
                files.add(old)
                current = old
            continue
        if line.startswith("+++ "):
            new = _diff_path(line[4:])
            if new:
                files.add(new)
                current = new
            continue
        hunk = _HUNK.match(line)
        if hunk:
            old_ln, new_ln = int(hunk.group(1)), int(hunk.group(2))
            continue
        if line.startswith(("---", "+++")) or line[:1] not in ("+", "-", " "):
            continue
        added = line[0] == "+"
        if line[0] == " ":
            old_ln += 1
            new_ln += 1
            continue
        lineno = (new_ln + 1) if added else (old_ln + 1)
        if added:
            new_ln += 1
        else:
            old_ln += 1
        body = line[1:]
        if _COMMENT_LINE.match(body):
            continue
        for rx in (_RUST_CONST, _C_DEFINE, _C_ENUM):
            m = rx.match(body)
            if m:
                consts.add(m.group(1))
        if not current.startswith(RUST_FFI_PREFIXES):
            continue
        if _EXTERN_C.search(body):
            extern_c = True
            continue
        side = "new" if added else "old"
        if lineno in spans.get((current, side), ()):
            extern_c = True
    return files, consts, extern_c


def violations(consts: set[str], extern_c: bool, touched: set[str]) -> list[str]:
    """Rule verdict for one commit; empty means it landed atomically."""
    if not consts and not extern_c:
        return []
    groups = {
        "rust": any(f.startswith(RUST_FFI_PREFIXES) for f in touched),
        "headers": any(
            f.startswith(C_HEADER_PREFIX) and f.endswith(".h") for f in touched
        ),
        "go_headers": any(bool(GO_HEADER_RE.match(f)) for f in touched),
        "go_tests": any(bool(GO_ABI_TEST_RE.match(f)) for f in touched),
    }
    missing = [label for key, label in _GROUP_LABELS.items() if not groups[key]]
    if not missing:
        return []
    what = []
    if consts:
        what.append("NET_* constant(s) " + ", ".join(sorted(consts)))
    if extern_c:
        what.append('an `extern "C"` signature')
    return [
        f"the commit changes {' and '.join(what)} but does not touch:",
        *(f"  {m}" for m in missing),
        '  The ABI rule: an extern "C" / NET_* change lands Rust, the headers and',
        "  the Go ABI tests in ONE commit. Landed in pieces, every commit between",
        "  the pieces is red, or silently wrong under a bisect.",
    ]


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, cwd=ROOT, check=True
    ).stdout


def _show_at(rev: str, path: str) -> str | None:
    try:
        return _git("show", f"{rev}:{path}")
    except subprocess.CalledProcessError:
        return None


def ffi_sources(sha: str, patch: str) -> dict[str, tuple[str | None, str | None]]:
    """(pre-commit, post-commit) text of every changed FFI `.rs` file at `sha`.

    The diff alone cannot attribute a continuation-line edit to its
    `extern "C"` signature when the opener is outside the hunk's context;
    the file can. A side that cannot be read (root commit, deleted file)
    is `None`, and that side degrades to the line-local trigger.
    """
    out: dict[str, tuple[str | None, str | None]] = {}
    for line in patch.splitlines():
        if not line.startswith("+++ "):
            continue
        path = _diff_path(line[4:])
        if path and path.endswith(".rs") and path.startswith(RUST_FFI_PREFIXES):
            out[path] = (_show_at(f"{sha}^", path), _show_at(sha, path))
    return out


def default_range() -> str:
    before = os.environ.get("GITHUB_EVENT_BEFORE", "")
    sha = os.environ.get("GITHUB_SHA", "HEAD")
    if before and before != "0" * 40:
        return f"{before}..{sha}"
    return "HEAD^..HEAD"


def resolve_commits(range_spec: str) -> tuple[list[str], list[str]]:
    """(commit SHAs oldest-first, warnings) for `A..B`, degrading loudly."""
    try:
        return _git("rev-list", "--reverse", range_spec).split(), []
    except subprocess.CalledProcessError:
        head = range_spec.rsplit("..", 1)[-1] or "HEAD"
        return [head], [
            f"range {range_spec} is not resolvable (new branch or force push); "
            f"checking only the tip commit {head[:9]}"
        ]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--range",
        default=None,
        help="commits to check, A..B (default: $GITHUB_EVENT_BEFORE..$GITHUB_SHA, "
        "else HEAD^..HEAD)",
    )
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    spec = args.range or default_range()
    commits, warnings = resolve_commits(spec)
    for w in warnings:
        print(f"::warning::{w}")
    if not commits:
        print(f"no commits in {spec}")
        return 0

    failed = 0
    for sha in commits:
        try:
            parents = _git("rev-list", "--parents", "-n", "1", sha).split()[1:]
            if len(parents) > 1:
                print(f"  .. {sha[:9]} merge commit — its own commits carry the change")
                continue
            patch = _git("show", "--format=", "--patch", "--no-color", sha)
            subject = _git("log", "-1", "--format=%s", sha).strip()
        except subprocess.CalledProcessError as exc:
            _error(f"could not read commit {sha}: {exc}")
            return 1
        files, consts, extern_c = diff_changes(patch, ffi_sources(sha, patch))
        problems = violations(consts, extern_c, files)
        if problems:
            failed = 1
            _error(f"commit {sha[:9]} breaks the ABI one-commit rule", *problems)
        else:
            print(f"  ok {sha[:9]} {subject}")
    return failed


# --------------------------------------------------------------------------
# Self-test: every rejection path, against synthetic diffs.
# --------------------------------------------------------------------------


def self_test() -> int:
    print("==> self-test")
    bad: list[str] = []

    def expect(label: str, condition: bool) -> None:
        print(f"  {'ok  ' if condition else 'FAIL'} {label}")
        if not condition:
            bad.append(label)

    expect(
        "nothing changed demands nothing",
        violations(set(), False, set()) == [],
    )

    rust_change = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -1,2 +1,2 @@\n"
        "-pub(crate) const NET_ERR_FOO: c_int = -116;\n"
        "+pub(crate) const NET_ERR_FOO: c_int = -117;\n"
    )
    files, consts, extern_c = diff_changes(rust_change)
    expect(
        "a changed Rust const definition is detected",
        consts == {"NET_ERR_FOO"}
        and not extern_c
        and files == {"net/crates/net/src/ffi/mesh.rs"},
    )
    expect(
        "Rust alone violates the rule (headers and Go ABI test missing)",
        violations(consts, extern_c, files) != [],
    )
    both_headers = files | {"net/crates/net/include/net.go.h", "go/net.h"}
    expect(
        "both headers without a Go ABI test still violates",
        violations(consts, extern_c, both_headers) != [],
    )
    expect(
        "all four groups landing together passes",
        violations(
            consts,
            extern_c,
            both_headers | {"go/header_parity_test.go"},
        )
        == [],
    )

    define = (
        "--- a/go/net.h\n"
        "+++ b/go/net.h\n"
        "@@ -1 +1 @@\n"
        "-#define NET_STREAM_TIMEOUT 5\n"
        "+#define NET_STREAM_TIMEOUT 30\n"
    )
    _, consts, extern_c = diff_changes(define)
    expect("a changed #define is detected", consts == {"NET_STREAM_TIMEOUT"})

    enum = (
        "--- a/net/crates/net/include/net.go.h\n"
        "+++ b/net/crates/net/include/net.go.h\n"
        "@@ -1 +1 @@\n"
        "-    NET_ERR_FOO = -116,\n"
        "+    NET_ERR_FOO = -117,\n"
    )
    _, consts, extern_c = diff_changes(enum)
    expect("a changed enum member is detected", consts == {"NET_ERR_FOO"})

    prose = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -1 +1 @@\n"
        "-/// Returns NET_ERR_FOO on failure.\n"
        "+/// Returns NET_ERR_FOO (see the header) on failure.\n"
    )
    _, consts, extern_c = diff_changes(prose)
    expect(
        "a prose mention of a NET_* name does not trigger",
        not consts
        and not extern_c
        and violations(consts, extern_c, set()) == [],
    )

    extern = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -1 +1 @@\n"
        '-pub unsafe extern "C" fn net_mesh_close(h: *mut H) -> c_int {\n'
        '+pub unsafe extern "C" fn net_mesh_close(h: *mut H, why: c_int) -> c_int {\n'
    )
    files, consts, extern_c = diff_changes(extern)
    expect(
        'a changed `extern "C"` line in the FFI surface triggers',
        extern_c and not consts,
    )
    expect(
        "the extern-C-only diff drives its rejection end to end",
        violations(consts, extern_c, files) != [],
    )
    expect(
        'violations(set(), True, {ffi-file}) is non-empty — an '
        'extern-"C"-ONLY change with just the Rust FFI file is rejected '
        "(a regression that returns [] when consts is empty fails here)",
        violations(set(), True, {"net/crates/net/src/ffi/mesh.rs"}) != [],
    )

    outside = (
        "--- a/net/crates/net/tests/ffi.rs\n"
        "+++ b/net/crates/net/tests/ffi.rs\n"
        "@@ -1 +1 @@\n"
        '-    let f: extern "C" fn() = a;\n'
        '+    let f: extern "C" fn() = b;\n'
    )
    _, consts, extern_c = diff_changes(outside)
    expect(
        '`extern "C"` outside the FFI surface does not trigger',
        not extern_c and not consts,
    )

    fn_old = (
        "#[no_mangle]\n"
        'pub unsafe extern "C" fn net_mesh_send(\n'
        "    h: *mut Handle,\n"
        "    buf: *const u8,\n"
        "    n: usize,\n"
        "    len: u32,\n"
        ") -> c_int {\n"
        "    0\n"
        "}\n"
    )
    fn_new = fn_old.replace("len: u32", "len: u64")
    srcs = {"net/crates/net/src/ffi/mesh.rs": (fn_old, fn_new)}
    # The `extern "C"` opener is on file line 2; the edit is on line 6 —
    # outside this hunk entirely (the hunk starts at line 3). A line-local
    # trigger sees a bare parameter edit and stays silent.
    continuation = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -3,5 +3,5 @@\n"
        "     h: *mut Handle,\n"
        "     buf: *const u8,\n"
        "     n: usize,\n"
        "-    len: u32,\n"
        "+    len: u64,\n"
        " ) -> c_int {\n"
    )
    files, consts, extern_c = diff_changes(continuation, srcs)
    expect(
        "a parameter edit on a CONTINUATION line of a multi-line `extern \"C\"` "
        "signature triggers (the opener is outside the hunk)",
        extern_c and not consts and files == {"net/crates/net/src/ffi/mesh.rs"},
    )
    expect(
        "the continuation-line signature change alone violates the one-commit rule",
        violations(consts, extern_c, files) != [],
    )

    body_edit = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -8 +8 @@\n"
        "-    0\n"
        "+    1\n"
    )
    _, consts, extern_c = diff_changes(body_edit, srcs)
    expect(
        "a body edit outside every extern-C span does not trigger",
        not consts and not extern_c,
    )

    block_old = (
        'extern "C" {\n'
        "    pub fn net_mesh_len(h: *mut Handle, len: u32) -> c_int;\n"
        "}\n"
    )
    block_new = block_old.replace("len: u32", "len: u64")
    block_edit = (
        "--- a/net/crates/net/src/ffi/mesh.rs\n"
        "+++ b/net/crates/net/src/ffi/mesh.rs\n"
        "@@ -2 +2 @@\n"
        "-    pub fn net_mesh_len(h: *mut Handle, len: u32) -> c_int;\n"
        "+    pub fn net_mesh_len(h: *mut Handle, len: u64) -> c_int;\n"
    )
    _, consts, extern_c = diff_changes(
        block_edit, {"net/crates/net/src/ffi/mesh.rs": (block_old, block_new)}
    )
    expect(
        'a declaration edit inside an `extern "C" { … }` block triggers',
        extern_c and not consts,
    )

    if bad:
        _error(f"self-test failed: {len(bad)} predicate(s) wrong", *bad)
        return 1
    print("self-test: every rejection path fires")
    return 0


if __name__ == "__main__":
    sys.exit(main())
