#!/usr/bin/env python3
"""The public nRPC C header must match the implementation it describes.

`net_rpc.h` sat at `NET_RPC_ABI_VERSION 0x0002` while the implementation
was `0x0004`, and declared both cancellation functions without the
leading `MeshRpcHandle*` that `0x0004` added. A consumer compiled
against that header passed whatever happened to be in the first
argument register as a mesh pointer.

Nothing caught it. The compatibility helper was `runtime >= expected`,
so a stale `0x0002` header checking a `0x0004` library *passed* — the
one check that existed was blind to exactly the change it was meant to
catch. That helper is now exact equality, and this compares the two
surfaces mechanically:

- the ABI constant;
- which `net_rpc_*` functions exist;
- argument count and order;
- pointer versus value for each argument;
- return type.

C spellings and Rust spellings differ (`uint64_t` / `u64`,
`MeshRpcHandle*` / `*mut MeshRpcHandle`), so types are normalised to a
coarse shape — `ptr:<name>` or `val:<name>` — which is enough to catch
a missing or reordered argument without re-implementing a C parser.

Run locally:  .github/scripts/check-rpc-abi-parity.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HEADER = Path("net/crates/net/include/net_rpc.h")
IMPL = Path("net/crates/net/bindings/go/rpc-ffi/src/lib.rs")
FIXTURE = Path("net/crates/net/tests/cross_lang_nrpc/golden_vectors.json")

BLOCK_COMMENT = re.compile(r"(?s)/\*.*?\*/")
LINE_COMMENT = re.compile(r"//[^\n]*")

H_ABI = re.compile(r"#define\s+NET_RPC_ABI_VERSION\s+(0x[0-9a-fA-F]+|\d+)")
R_ABI = re.compile(r"NET_RPC_ABI_VERSION:\s*u32\s*=\s*(0x[0-9a-fA-F]+|\d+)")
J_ABI = re.compile(r'"abi_version_expected"\s*:\s*(\d+)')

# `uint64_t net_rpc_reserve_cancel_token(MeshRpcHandle* handle);`
H_FN = re.compile(
    r"(?P<ret>[A-Za-z_][A-Za-z0-9_ ]*?[\w*])\s+"
    r"(?P<name>net_rpc_\w+)\s*\((?P<args>[^;]*?)\)\s*;",
    re.S,
)
# `pub extern "C" fn net_rpc_x(a: *mut T, b: u64) -> u64 {`
R_FN = re.compile(
    r'pub\s+(?:unsafe\s+)?extern\s+"C"\s+fn\s+(?P<name>net_rpc_\w+)\s*'
    r"\((?P<args>[^)]*)\)(?:\s*->\s*(?P<ret>[^{\s][^{]*?))?\s*\{",
    re.S,
)


def shape(text: str) -> str:
    """Normalise a C or Rust type to `ptr:<base>` / `val:<base>`."""
    t = " ".join(text.split())
    if not t or t in {"void", "()"}:
        return "void"
    is_ptr = "*" in t
    base = t.replace("*", " ")
    for noise in ("const", "mut", "unsafe", "extern"):
        base = re.sub(rf"\b{noise}\b", " ", base)
    # Drop a trailing parameter name: `MeshRpcHandle handle` -> the type.
    parts = [p for p in base.split() if p]
    if not parts:
        return "void"
    known_c = {
        "uint8_t", "uint16_t", "uint32_t", "uint64_t", "size_t",
        "int", "char", "void", "float", "double",
    }
    if len(parts) > 1 and parts[0] in known_c:
        parts = parts[:1]
    elif len(parts) > 1:
        # `MeshRpcHandle handle` / `handle: MeshRpcHandle`
        parts = [max(parts, key=lambda p: sum(c.isupper() for c in p) or 0)]
    base = parts[0].strip(":,")
    alias = {
        # Primitive spellings.
        "u8": "uint8_t", "u16": "uint16_t", "u32": "uint32_t",
        "u64": "uint64_t", "usize": "size_t", "c_int": "int",
        "c_char": "char", "c_void": "void",
        # C typedef <-> Rust type. These are the same thing under two
        # naming conventions; the check that matters for them is
        # pointer-vs-value and argument position, which is preserved.
        "NetRpcHeader": "net_rpc_header_t",
        "RpcHandlerFn": "net_rpc_handler_fn",
        "RpcStreamingHandlerFn": "net_rpc_streaming_handler_fn",
        # `net_rpc_new` takes the node as an erased handle on the C
        # side by design — the header cannot name `Arc<MeshNode>`.
        "Arc<MeshNode>": "void",
    }
    base = alias.get(base, base)
    return f"{'ptr' if is_ptr else 'val'}:{base}"


def split_args(args: str) -> list[str]:
    args = args.strip()
    if not args or args == "void":
        return []
    out, depth, cur = [], 0, ""
    for ch in args:
        if ch in "(<":
            depth += 1
        elif ch in ")>":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return [a for a in out if a.strip()]


def header_surface() -> tuple[str, dict[str, tuple[str, list[str]]]]:
    raw = HEADER.read_text(encoding="utf-8")
    abi = H_ABI.search(raw)
    src = BLOCK_COMMENT.sub("", raw)
    fns: dict[str, tuple[str, list[str]]] = {}
    for m in H_FN.finditer(src):
        # Function-pointer typedefs and macros are not exports.
        if "typedef" in m.group(0):
            continue
        fns[m.group("name")] = (
            shape(m.group("ret")),
            [shape(a) for a in split_args(m.group("args"))],
        )
    return (abi.group(1) if abi else "?"), fns


def impl_surface() -> tuple[str, dict[str, tuple[str, list[str]]]]:
    raw = IMPL.read_text(encoding="utf-8")
    abi = R_ABI.search(raw)
    src = LINE_COMMENT.sub("", raw)
    fns: dict[str, tuple[str, list[str]]] = {}
    for m in R_FN.finditer(src):
        args = [shape(a.split(":", 1)[-1]) for a in split_args(m.group("args"))]
        fns[m.group("name")] = (shape(m.group("ret") or "void"), args)
    return (abi.group(1) if abi else "?"), fns


def main() -> int:
    problems: list[str] = []

    h_abi, h_fns = header_surface()
    r_abi, r_fns = impl_surface()

    if int(h_abi, 0) != int(r_abi, 0):
        problems.append(
            f"ABI constant: header says {h_abi}, implementation says {r_abi}"
        )

    fixture = J_ABI.search(FIXTURE.read_text(encoding="utf-8"))
    if not fixture:
        problems.append(f"{FIXTURE}: no abi_version_expected")
    elif int(fixture.group(1)) != int(r_abi, 0):
        problems.append(
            f"{FIXTURE}: abi_version_expected is {fixture.group(1)}, "
            f"implementation is {int(r_abi, 0)}"
        )

    # Only compare functions the header actually declares. The
    # implementation may export more than the public header exposes;
    # a declared symbol that disagrees is the hazard.
    for name, (h_ret, h_args) in sorted(h_fns.items()):
        if name not in r_fns:
            problems.append(f"{name}: declared in the header, not exported")
            continue
        r_ret, r_args = r_fns[name]
        if len(h_args) != len(r_args):
            problems.append(
                f"{name}: header takes {len(h_args)} arg(s) {h_args}, "
                f"implementation takes {len(r_args)} {r_args}"
            )
            continue
        for i, (a, b) in enumerate(zip(h_args, r_args)):
            if a != b:
                problems.append(
                    f"{name}: arg {i} is {a} in the header, {b} in the "
                    f"implementation"
                )
        if h_ret != r_ret:
            problems.append(
                f"{name}: returns {h_ret} in the header, {r_ret} in the "
                f"implementation"
            )

    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
