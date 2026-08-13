#!/usr/bin/env python3
"""No Rust `libc::free` on a buffer the Go callback layer allocated.

The allocator that creates a callback-owned buffer must release it.

Go callbacks allocate their responses and error strings with `C.malloc`
/ `C.CString`, which resolve to the CRT linked into the CGO application
module. `libc::free` inside `net.dll` resolves to a *different* CRT on
Windows, where every module carries its own heap. Freeing across that
boundary corrupts the heap.

This is not theoretical. Application Verifier caught it at
`291280b85`:

    StopCode 0x6 — Corrupted heap pointer or using wrong heap
    block 0x1a (26 bytes) — exactly {"n":2,"servedBy":"go-s4"}
    ucrtbase!free_base <- net!net_org_serve

It terminated the process deterministically whenever an affected
handler returned a non-empty response, and was the root cause of every
`STATUS_HEAP_CORRUPTION` (0xC0000374) abort in the Go test suite.

The repair routes every such release through a deallocator Go
registers, implemented in the Go module's own C translation unit. This
gate keeps it that way: a `libc::free` *call* reintroduced into any Go
FFI crate fails here rather than in a Windows crash weeks later.

Prose is allowed — the surviving mentions explain why the call is
banned. Only call sites are rejected.

Exit 0 when clean, 1 with the offending lines otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_CRATES = [
    "net/crates/net/bindings/go/org-ffi/src",
    "net/crates/net/bindings/go/rpc-ffi/src",
    "net/crates/net/bindings/go/compute-ffi/src",
    "net/crates/net/bindings/go/meshos-ffi/src",
    "net/crates/net/bindings/go/meshdb-ffi/src",
    "net/crates/net/bindings/go/deck-ffi/src",
    "net/crates/net/bindings/go/mcp-ffi/src",
]

# `libc::free(` with an open paren is a call. `libc::free` inside a doc
# comment, a `//` comment, or a string is prose.
_CALL = re.compile(r"libc::free\s*\(")


def _is_prose(line: str) -> bool:
    stripped = line.lstrip()
    return stripped.startswith("//") or stripped.startswith("*")


def main() -> int:
    offenders: list[str] = []
    scanned = 0
    for rel in _CRATES:
        root = _ROOT / rel
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            scanned += 1
            for n, line in enumerate(
                path.read_text(encoding="utf-8").splitlines(), start=1
            ):
                if _is_prose(line):
                    continue
                if _CALL.search(line):
                    offenders.append(
                        f"  {path.relative_to(_ROOT)}:{n}: {line.strip()}"
                    )

    if not scanned:
        print("FAIL  no Go FFI crate sources found — this gate is scanning nothing")
        return 1

    if offenders:
        print("FAIL  Rust `libc::free` call in a Go FFI crate:\n")
        print("\n".join(offenders))
        print(
            "\nThese buffers are allocated by the Go module's CRT. On Windows "
            "`libc::free`\nhere runs against a different heap — confirmed heap "
            "corruption and\ndeterministic process termination (AppVerifier "
            "StopCode 0x6).\n\n"
            "Route the release through the Go-registered deallocator instead:\n"
            "    free_callback_buffer(ptr as *mut std::ffi::c_void);\n\n"
            "If the pointer is genuinely Rust-owned, it should not be reaching "
            "`libc::free`\neither — use the owning Rust type's drop."
        )
        return 1

    print(f"No Rust libc::free calls across {scanned} Go FFI source files.")
    print("Callback buffers are released by the allocator that made them.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
