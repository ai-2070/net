#!/usr/bin/env python3
"""Every API symbol the composed SDK spine names must exist in its binding.

WHY THIS EXISTS. Phase 3 converted one page and found three invented C symbols,
a version pin for an unpublished release, and the wrong entry-point class in two
languages — on the first page a self-serve reader opens. Phase 4 converted the
whole spine and found more of the same shape: `serve_tool(node, …)` in two
bindings where the function takes an RPC handle, `CallOptions::default()
.with_deadline(…)` where no such method exists, `serve_rpc_typed` called with two
arguments where it takes three, and a Go example passing `*MeshRpc` to a function
that wants `*TypedMeshRpc`.

None of those were caught by a human reading the page, because they all read
correctly. They were caught by opening the source. That is the check, and this is
it running in CI instead of once.

WHAT IT PROVES, AND WHAT IT DOES NOT. Every symbol listed here resolves in the
named binding's tree. That is evidence level `source-match`: the name exists. It
is NOT proof the call compiles, that the argument order is right, or that the
example runs. Claiming more would be the thing this file exists to prevent —
`docs/data/examples.yaml` carries the levels above this one, for code that is
actually executed.

The list is authored, not extracted. A regex over code fences cannot tell an SDK
symbol from a local variable, and a checker that guesses produces either noise
nobody reads or silence nobody trusts.

  .github/scripts/check-spine-symbols.py              # check
  .github/scripts/check-spine-symbols.py --self-test  # plant defects
"""

from __future__ import annotations

import os
import subprocess
import sys

# Every check in this suite prints its verdict with U+2713 / U+2717, and some
# of the identifiers it echoes carry em-dashes. Python picks stdout's encoding
# from the platform, so on a cp1252 console those characters raise
# UnicodeEncodeError mid-report — the checker dies partway through and its
# caller sees a truncated run rather than a verdict. Force UTF-8 so the output
# is the same everywhere the checker runs.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover
    sys.exit("PyYAML is required: python3 -m pip install pyyaml")

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
MANIFEST = os.environ.get("SPINE_SYMBOLS", "docs/data/spine-symbols.yaml")

# Same tree map the capability record's anchors resolve against, and for the
# same reason: a symbol has to exist SOMEWHERE in the binding. Whether it sits on
# the ergonomic wrapper or the layer underneath is carried editorially — by the
# record's `core-only` mode, and by the fragment prose that says which handle to
# reach for.
TREES = {
    "rust": (["net/crates/net/sdk/src", "net/crates/net/payments/src",
              "net/crates/net/src"], (".rs",)),
    # napi declares the Node surface in Rust, so .rs counts for TypeScript.
    "typescript": (["net/crates/net/sdk-ts/src", "net/crates/net/bindings/node"],
                   (".ts", ".rs")),
    "python": (["net/crates/net/sdk-py/src", "net/crates/net/bindings/python"],
               (".py", ".pyi", ".rs")),
    "go": (["go"], (".go",)),
}

RED, GREEN, DIM, OFF = "\033[31m", "\033[32m", "\033[2m", "\033[0m"


def blobs() -> dict[str, str]:
    listing = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT,
                             capture_output=True, text=True, check=True).stdout
    tracked = [p for p in listing.split("\0") if p]
    out: dict[str, str] = {}
    for lens, (roots, exts) in TREES.items():
        chunks = []
        for path in tracked:
            if not path.endswith(exts):
                continue
            if not any(path.startswith(r + "/") or path == r for r in roots):
                continue
            try:
                with open(os.path.join(ROOT, path), encoding="utf-8",
                          errors="replace") as fh:
                    chunks.append(fh.read())
            except OSError:
                pass
        out[lens] = "\n".join(chunks)
    return out


def load(path: str) -> dict:
    with open(path, encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def run(manifest_path: str, docs_dir: str) -> int:
    fail = 0
    doc = load(manifest_path)
    trees = blobs()
    pages = doc.get("pages") or {}

    print(f"==> Spine symbols ({os.path.relpath(manifest_path, ROOT)})")

    total = resolved = 0
    for page, lenses in sorted(pages.items()):
        for lens, symbols in sorted((lenses or {}).items()):
            if lens not in TREES:
                print(f"  {RED}✗{OFF} {page}/{lens}: unknown lens "
                      f"(closed set: {', '.join(sorted(TREES))})")
                fail += 1
                continue
            # A fragment must exist for the symbols to be claimed FROM. A
            # manifest entry for a fragment nobody wrote is a stale claim.
            frag = os.path.join(docs_dir, "sdk", page, f"{lens}.md")
            if not os.path.exists(frag):
                print(f"  {RED}✗{OFF} {page}/{lens}: no such fragment "
                      f"({os.path.relpath(frag, ROOT)})")
                fail += 1
                continue
            frag_src = open(frag, encoding="utf-8").read()
            for symbol in symbols or []:
                total += 1
                if symbol not in trees[lens]:
                    print(f"  {RED}✗{OFF} {page}/{lens}: `{symbol}` resolves "
                          f"nowhere in the {lens} tree")
                    fail += 1
                    continue
                # And the page has to actually name it. A manifest that drifts
                # ahead of the prose proves a symbol nobody is being shown.
                if symbol not in frag_src:
                    print(f"  {RED}✗{OFF} {page}/{lens}: `{symbol}` is declared "
                          f"but the fragment never mentions it")
                    fail += 1
                    continue
                resolved += 1

    if fail == 0:
        print(f"  {GREEN}✓{OFF} {resolved}/{total} symbols resolve in their "
              f"binding and appear on their page")
        print(f"  {DIM}    evidence level: source-match — the name exists. Not "
              f"a claim that the example compiles or runs.{OFF}")
        print()
        print("Every symbol the spine names is a symbol the binding has.")
        return 0
    print()
    print(f"{fail} spine-symbol problem(s).")
    return 1


def self_test() -> int:
    """Plant defects in a scratch manifest and require each to be reported."""
    import shutil
    import tempfile

    print("==> Self-test — planting defects in a scratch manifest")
    src = load(os.path.join(ROOT, MANIFEST))
    docs = os.path.join(ROOT, "web/src/content/docs")

    cases = [
        ("a symbol that resolves nowhere", "resolves nowhere",
         lambda d: d["pages"]["announce"]["rust"].append("net_totally_invented")),
        ("a symbol the page never mentions", "never mentions it",
         # `Mesh` exists in the Rust tree but the announce fragment does not
         # name it, so this is the manifest drifting ahead of the prose.
         lambda d: d["pages"]["announce"]["rust"].append("MeshBlobAdapter")),
        ("an unknown lens", "unknown lens",
         lambda d: d["pages"]["announce"].update({"cobol": ["MOVE"]})),
        ("a fragment that does not exist", "no such fragment",
         lambda d: d["pages"].update({"nonexistent-page": {"rust": ["Mesh"]}})),
    ]

    failures = 0
    for label, expect, mutate in cases:
        import copy
        scratch = copy.deepcopy(src)
        mutate(scratch)
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "spine-symbols.yaml")
            with open(path, "w", encoding="utf-8") as fh:
                yaml.safe_dump(scratch, fh, sort_keys=False, allow_unicode=True)
            import io
            import contextlib
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = run(path, docs)
            out = buf.getvalue()
        if code != 0 and expect in out:
            print(f"  {GREEN}✓{OFF} reported {label}")
        else:
            print(f"  {RED}✗{OFF} did NOT report {label}")
            failures += 1

    # And the unmodified manifest still passes, so the cases above are testing
    # the planted defect rather than a manifest that was already broken.
    import io
    import contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        code = run(os.path.join(ROOT, MANIFEST), docs)
    if code == 0:
        print(f"  {GREEN}✓{OFF} the unmodified manifest passes")
    else:
        print(f"  {RED}✗{OFF} the unmodified manifest FAILS")
        print(buf.getvalue())
        failures += 1

    print()
    if failures:
        print(f"{failures} self-test failure(s).")
        return 1
    print("The checker reports every planted defect.")
    return 0


def main() -> int:
    os.chdir(ROOT)
    if "--self-test" in sys.argv:
        return self_test()
    return run(os.path.join(ROOT, MANIFEST),
               os.path.join(ROOT, "web/src/content/docs"))


if __name__ == "__main__":
    sys.exit(main())
