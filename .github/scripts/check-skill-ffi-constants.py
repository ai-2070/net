#!/usr/bin/env python3
"""Every `NET_*` constant named in the net-event-bus skill docs must exist.

The skill docs hand readers C-ABI names to match on — a C or cgo
consumer writes `switch (rc)` arms for `NET_ERR_BLOB_BACKEND` and
friends. A fabricated or retired name compiles nowhere and breaks
nothing: the reader's arm simply never fires and they misclassify the
failure. One shipped — `dataforts.md` documented
`NET_ERR_BLOB_VTABLE_INVALID`, which never existed; the partial-vtable
null check in `src/ffi/blob.rs` returns `NET_ERR_BLOB_BACKEND` — and
nothing caught it, because a constant name in prose is caller-contract
drift, not a compile error.

This scans the top-level `.claude/skills/net-event-bus/*.md` docs for
every `NET_*` constant they name and resolves each against the two
places a consumer can really get it from:

  * `net/crates/net/src/ffi/**` — the Rust `const` bands the headers
    mirror, and
  * `net/crates/net/include/*.h` — the `#define`s and enum entries a C
    consumer actually includes (`#include <net_transport.h>` and kin).

A name found in neither fails the check.

Environment variables are NOT constants and live in neither source:
the documented `NET_MESH_*` CLI variables and the cross-language
`NET_*_BUILT` test gates are listed in ENVIRONMENT_VARIABLES with the
surface that reads each; a newly documented env var joins that list
deliberately. Wildcard mentions (`NET_ERR_*`) and bare prefixes
(`NET_MESH_`) name no single constant and are not matched.

Run locally:  .github/scripts/check-skill-ffi-constants.py
Exit 0 = every constant the skill docs name is defined in the tree.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

SKILL_DIR = Path(".claude/skills/net-event-bus")
FFI_DIR = Path("net/crates/net/src/ffi")
INCLUDE_DIR = Path("net/crates/net/include")

# A standalone constant name: starts `NET_`, ends on an alphanumeric —
# so `NET_ERR_*` wildcard mentions and `NET_MESH_` bare-prefix mentions
# (trailing `_`) match nothing — and cannot be the tail of a longer
# identifier (`ALL_NET_ERRORS` never yields `NET_ERRORS`).
NAME_RE = re.compile(r"(?<![A-Za-z0-9_])NET_[A-Z0-9_]*[A-Z0-9](?![A-Za-z0-9_])")

# Rust definitions: `pub const NAME: T = …`, `pub(crate) const NAME: T = …`,
# bare `const` / `static` items.
RUST_DEF_RE = re.compile(
    r"(?:pub(?:\s*\(\s*crate\s*\))?\s+)?(?:const|static)\s+(NET_[A-Z0-9_]+)\s*:"
)

# C definitions: `#define NAME …` (constants and include guards) and enum
# entries (`NET_SUCCESS = 0,` / `NET_ERR_UNKNOWN = -99,`).
C_DEFINE_RE = re.compile(r"#\s*define\s+(NET_[A-Z0-9_]+)")
C_ENUM_ENTRY_RE = re.compile(r"^\s*(NET_[A-Z0-9_]+)\s*[,=]", re.MULTILINE)

# Names the docs mention that are environment variables / test gates,
# not FFI constants — defined in neither `src/ffi/**` nor `include/*.h`.
ENVIRONMENT_VARIABLES = {
    "NET_MESH_CONFIG": "cli: profile-file selection (the NET_MESH_ env prefix)",
    "NET_MESH_PROFILE": "cli: profile-name selection",
    "NET_MESH_INSECURE_CONFIG_PERMISSIONS": "cli: profile permission gate",
    "NET_MESH_LOG": "cli: tracing env-filter override",
    "NET_NODE_BUILT": "cross-language nRPC interop test gate",
    "NET_PYTHON_BUILT": "cross-language nRPC interop test gate",
}


def defined_constants() -> set[str]:
    names: set[str] = set()
    for rs in sorted(FFI_DIR.rglob("*.rs")):
        src = rs.read_text(encoding="utf-8")
        names.update(RUST_DEF_RE.findall(src))
    for hdr in sorted(INCLUDE_DIR.glob("*.h")):
        src = hdr.read_text(encoding="utf-8")
        names.update(C_DEFINE_RE.findall(src))
        names.update(C_ENUM_ENTRY_RE.findall(src))
    return names


def main() -> int:
    defined = defined_constants()
    problems = 0
    for doc in sorted(SKILL_DIR.glob("*.md")):
        for lineno, line in enumerate(doc.read_text(encoding="utf-8").splitlines(), 1):
            for name in NAME_RE.findall(line):
                if name in ENVIRONMENT_VARIABLES or name in defined:
                    continue
                print(
                    f"{doc}:{lineno}: {name} is named as a constant but is "
                    f"defined in neither {FFI_DIR}/** nor {INCLUDE_DIR}/*.h"
                )
                problems += 1
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
