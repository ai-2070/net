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

The same comparison now runs over every FFI surface with a public C
header, not just nRPC. It was extended after
`net_org_set_handler_dispatcher` changed from `void` to `int` and
nothing failed: the nRPC gate caught its four siblings, and `net_org.h`
went stale in silence. A gate that covers one of several identical
surfaces mostly teaches you which one is covered.

The filename is historical — it predates the other surfaces.

Run locally:  .github/scripts/check-rpc-abi-parity.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import NamedTuple


class Surface(NamedTuple):
    """One (header, implementation) pair to compare."""

    name: str
    header: Path
    impl: Path
    #: Only symbols starting with this are compared. `go/net.h` carries
    #: several surfaces at once, so without it every unrelated
    #: declaration would read as "declared but not exported".
    prefix: str
    #: `#define <NAME> 0x…` in the header / `const <NAME>: u32 = …` in
    #: the impl. `None` for a surface with no versioned ABI.
    abi_const: str | None = None
    #: JSON fixture pinning the same number, if one exists.
    fixture: Path | None = None
    #: C typedef <-> Rust type, in either direction, mapped onto a
    #: common token. These are the same type under two naming
    #: conventions; what must agree is pointer-versus-value and
    #: argument position, and a rename does not change either.
    #:
    #: Writing them out is the point. An opaque handle whose Rust type
    #: is renamed shows up here as a failure until someone records the
    #: new correspondence, which is where a reader can find it.
    aliases: dict[str, str] = {}


SURFACES = (
    Surface(
        name="nrpc",
        header=Path("net/crates/net/include/net_rpc.h"),
        impl=Path("net/crates/net/bindings/go/rpc-ffi/src/lib.rs"),
        prefix="net_rpc_",
        abi_const="NET_RPC_ABI_VERSION",
        fixture=Path("net/crates/net/tests/cross_lang_nrpc/golden_vectors.json"),
        aliases={
            "NetRpcHeader": "header",
            "net_rpc_header_t": "header",
            "RpcHandlerFn": "handler_fn",
            "net_rpc_handler_fn": "handler_fn",
            "RpcStreamingHandlerFn": "streaming_handler_fn",
            "net_rpc_streaming_handler_fn": "streaming_handler_fn",
            "RpcCallbackFreeFn": "callback_free_fn",
            "CallbackFreeFn": "callback_free_fn",
            # `net_rpc_new` takes the node as an erased handle by
            # design — the header cannot name `Arc<MeshNode>`.
            "Arc<MeshNode>": "void",
        },
    ),
    Surface(
        name="org",
        header=Path("net/crates/net/include/net_org.h"),
        impl=Path("net/crates/net/bindings/go/org-ffi/src/lib.rs"),
        prefix="net_org_",
        abi_const="NET_ORG_ABI_VERSION",
        aliases={
            "NetOrgHandlerFn": "handler_fn",
            "OrgHandlerFn": "handler_fn",
            "NetOrgCallbackFreeFn": "callback_free_fn",
            "CallbackFreeFn": "callback_free_fn",
            # The mesh crosses as an erased pointer; the header cannot
            # name `Arc<MeshNode>`.
            "net_compute_mesh_arc_t": "void",
            "Arc<MeshNode>": "void",
        },
    ),
    Surface(
        name="meshos",
        header=Path("net/crates/net/include/net_meshos.h"),
        impl=Path("net/crates/net/bindings/go/meshos-ffi/src/lib.rs"),
        prefix="net_meshos_",
        aliases={
            "NetMeshOsUserCtxDestroyFn": "user_ctx_destroy_fn",
            "MeshOsUserCtxDestroyFn": "user_ctx_destroy_fn",
        },
    ),
    Surface(
        name="compute",
        header=Path("go/net.h"),
        impl=Path("net/crates/net/bindings/go/compute-ffi/src/lib.rs"),
        prefix="net_compute_",
        aliases={
            # Opaque handles: `<c name>_t` on one side, a Rust struct on
            # the other.
            "net_compute_runtime_t": "runtime",
            "DaemonRuntimeHandle": "runtime",
            "net_compute_daemon_handle_t": "daemon_handle",
            "DaemonHandleC": "daemon_handle",
            "net_compute_migration_handle_t": "migration_handle",
            "MigrationHandleC": "migration_handle",
            "net_compute_fork_group_t": "fork_group",
            "ForkGroupHandle": "fork_group",
            "net_compute_replica_group_t": "replica_group",
            "ReplicaGroupHandle": "replica_group",
            "net_compute_standby_group_t": "standby_group",
            "StandbyGroupHandle": "standby_group",
            "net_compute_outputs_t": "outputs",
            "OutputsVec": "outputs",
            # Erased across the boundary — the header cannot name a
            # generic Rust `Arc`.
            "net_compute_mesh_arc_t": "void",
            "Arc<MeshNode>": "void",
            "net_compute_cc_arc_t": "cc_arc",
            "Arc<ChannelConfigRegistry>": "cc_arc",
            # Dispatcher function pointers.
            "net_compute_process_fn": "process_fn",
            "ProcessFn": "process_fn",
            "net_compute_snapshot_fn": "snapshot_fn",
            "SnapshotFn": "snapshot_fn",
            "net_compute_restore_fn": "restore_fn",
            "RestoreFn": "restore_fn",
            "net_compute_free_fn": "free_fn",
            "FreeFn": "free_fn",
            "net_compute_factory_fn": "factory_fn",
            "FactoryFn": "factory_fn",
            "net_compute_daemon_caps_fn": "daemon_caps_fn",
            "DaemonCapsFn": "daemon_caps_fn",
            "net_compute_placement_filter_fn": "placement_filter_fn",
            "PlacementFilterFn": "placement_filter_fn",
            "net_compute_callback_free_fn": "callback_free_fn",
            "CallbackFreeFn": "callback_free_fn",
        },
    ),
)

#: Primitive spellings that mean the same machine type in both
#: languages. Surface-specific typedefs live in each `Surface.aliases`.
PRIMITIVES = {
    "u8": "uint8_t",
    "u16": "uint16_t",
    "u32": "uint32_t",
    "u64": "uint64_t",
    "i8": "int8_t",
    "i16": "int16_t",
    "i32": "int32_t",
    "i64": "int64_t",
    "usize": "size_t",
    "isize": "ssize_t",
    "f32": "float",
    "f64": "double",
    "c_int": "int",
    "c_char": "char",
    "c_void": "void",
    "c_float": "float",
    "c_double": "double",
}

BLOCK_COMMENT = re.compile(r"(?s)/\*.*?\*/")
LINE_COMMENT = re.compile(r"//[^\n]*")

J_ABI = re.compile(r'"abi_version_expected"\s*:\s*(\d+)')


def h_abi_re(const: str) -> re.Pattern[str]:
    return re.compile(rf"#define\s+{const}\s+(0x[0-9a-fA-F]+|\d+)")


def r_abi_re(const: str) -> re.Pattern[str]:
    return re.compile(rf"{const}:\s*u32\s*=\s*(0x[0-9a-fA-F]+|\d+)")

# `uint64_t net_rpc_reserve_cancel_token(MeshRpcHandle* handle);`
def h_fn_re(prefix: str) -> re.Pattern[str]:
    return re.compile(
        r"(?P<ret>[A-Za-z_][A-Za-z0-9_ ]*?[\w*])\s+"
        rf"(?P<name>{prefix}\w+)\s*\((?P<args>[^;]*?)\)\s*;",
        re.S,
    )


# `pub extern "C" fn net_rpc_x(a: *mut T, b: u64) -> u64 {`
def r_fn_re(prefix: str) -> re.Pattern[str]:
    return re.compile(
        r'pub\s+(?:unsafe\s+)?extern\s+"C"\s+fn\s+'
        rf"(?P<name>{prefix}\w+)\s*"
        r"\((?P<args>[^)]*)\)(?:\s*->\s*(?P<ret>[^{\s][^{]*?))?\s*\{",
        re.S,
    )


def shape(text: str, aliases: dict[str, str] | None = None) -> str:
    """Normalise a C or Rust type to `ptr:<base>` / `val:<base>`.

    `aliases` maps either spelling onto a common token. A C typedef
    and the Rust type it names are the same thing under two naming
    conventions; what has to agree is pointer-versus-value and
    argument position, and those survive the rename.
    """
    t = " ".join(text.split())
    if not t or t in {"void", "()"}:
        return "void"
    # `Option<SomeFnPtr>` is ABI-identical to a nullable function
    # pointer — that is the null-pointer optimization, and it is how a
    # Rust `extern "C"` signature accepts NULL without an unsafe cast.
    # Unwrap it so it compares equal to the header's bare typedef.
    m = re.fullmatch(r"Option\s*<\s*(.+?)\s*>", t)
    if m:
        t = m.group(1)
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
    # `std::ffi::c_void` and `c_void` are the same type; a Rust
    # signature may write either. Keep the last path segment, unless
    # the name carries generics (`Arc<MeshNode>`), where the qualifier
    # is not a path separator worth splitting on.
    if "::" in base and "<" not in base:
        base = base.rsplit("::", 1)[-1]
    base = (aliases or {}).get(base, PRIMITIVES.get(base, base))
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


def header_surface(sf: Surface) -> tuple[str | None, dict[str, tuple[str, list[str]]]]:
    raw = sf.header.read_text(encoding="utf-8")
    abi = h_abi_re(sf.abi_const).search(raw) if sf.abi_const else None
    src = BLOCK_COMMENT.sub("", raw)
    fns: dict[str, tuple[str, list[str]]] = {}
    for m in h_fn_re(sf.prefix).finditer(src):
        # Function-pointer typedefs and macros are not exports.
        if "typedef" in m.group(0):
            continue
        fns[m.group("name")] = (
            shape(m.group("ret"), sf.aliases),
            [shape(a, sf.aliases) for a in split_args(m.group("args"))],
        )
    return (abi.group(1) if abi else None), fns


def impl_surface(sf: Surface) -> tuple[str | None, dict[str, tuple[str, list[str]]]]:
    raw = sf.impl.read_text(encoding="utf-8")
    abi = r_abi_re(sf.abi_const).search(raw) if sf.abi_const else None
    src = LINE_COMMENT.sub("", raw)
    fns: dict[str, tuple[str, list[str]]] = {}
    for m in r_fn_re(sf.prefix).finditer(src):
        args = [
            shape(a.split(":", 1)[-1], sf.aliases) for a in split_args(m.group("args"))
        ]
        fns[m.group("name")] = (shape(m.group("ret") or "void", sf.aliases), args)
    return (abi.group(1) if abi else None), fns


def check(sf: Surface) -> list[str]:
    """Every disagreement between one header and its implementation."""
    problems: list[str] = []
    h_abi, h_fns = header_surface(sf)
    r_abi, r_fns = impl_surface(sf)

    # A surface that declares an ABI constant must declare it on both
    # sides; a missing one is drift too, not an excuse to skip.
    if sf.abi_const:
        if h_abi is None:
            problems.append(f"{sf.header}: no {sf.abi_const}")
        elif r_abi is None:
            problems.append(f"{sf.impl}: no {sf.abi_const}")
        elif int(h_abi, 0) != int(r_abi, 0):
            problems.append(
                f"{sf.abi_const}: header says {h_abi}, implementation says {r_abi}"
            )

    if sf.fixture is not None and r_abi is not None:
        fixture = J_ABI.search(sf.fixture.read_text(encoding="utf-8"))
        if not fixture:
            problems.append(f"{sf.fixture}: no abi_version_expected")
        elif int(fixture.group(1)) != int(r_abi, 0):
            problems.append(
                f"{sf.fixture}: abi_version_expected is {fixture.group(1)}, "
                f"implementation is {int(r_abi, 0)}"
            )

    # A surface that matched nothing means the regex or the path
    # regressed. Silence would read as success.
    if not h_fns:
        problems.append(
            f"{sf.header}: no {sf.prefix}* declarations matched — "
            f"the header parser or the path regressed"
        )
    if not r_fns:
        problems.append(
            f"{sf.impl}: no {sf.prefix}* exports matched — "
            f"the impl parser or the path regressed"
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
    return problems


_SELF_TEST_HEADER = """
#define NET_DEMO_ABI_VERSION 0x0007
typedef int (*DemoHandlerFn)(uint64_t id);
int      net_demo_serve(DemoHandle* h, uint64_t id, DemoHandlerFn cb);
void     net_demo_free(DemoHandle* h);
"""

_SELF_TEST_IMPL = """
pub const NET_DEMO_ABI_VERSION: u32 = 0x0007;
pub extern "C" fn net_demo_serve(h: *mut DemoHandleRs, id: u64, cb: Option<DemoCb>) -> c_int { }
pub extern "C" fn net_demo_free(h: *mut DemoHandleRs) { }
"""

#: The two spellings of each self-test type, as a real surface would
#: declare them.
_SELF_TEST_ALIASES = {
    "DemoHandle": "handle",
    "DemoHandleRs": "handle",
    "DemoHandlerFn": "cb",
    "DemoCb": "cb",
}


def _demo_surface(tmp: Path, header: str, impl: str) -> Surface:
    h = tmp / "demo.h"
    r = tmp / "demo.rs"
    h.write_text(header, encoding="utf-8")
    r.write_text(impl, encoding="utf-8")
    return Surface(
        name="self-test",
        header=h,
        impl=r,
        prefix="net_demo_",
        abi_const="NET_DEMO_ABI_VERSION",
        aliases=_SELF_TEST_ALIASES,
    )


def self_test() -> int:
    """Prove the comparison fails on the drift it exists to catch.

    A parity gate that only ever prints `ok` is indistinguishable from
    one whose regexes quietly stopped matching — and that is not
    hypothetical here, since this checker was written *because* an
    earlier compatibility helper passed a stale header.

    Each case is a real drift. If any stops being reported, every `ok`
    the checker prints is worthless, so this fails loudly rather than
    reporting a clean run.
    """
    import tempfile

    ghost = _SELF_TEST_HEADER + "int net_demo_ghost(void);\n"
    reordered = _SELF_TEST_HEADER.replace(
        "DemoHandle* h, uint64_t id, DemoHandlerFn cb",
        "uint64_t id, DemoHandle* h, DemoHandlerFn cb",
    )
    cases = [
        # (label, header, impl, substring expected in a reported problem)
        ("a matching pair is clean", _SELF_TEST_HEADER, _SELF_TEST_IMPL, ""),
        (
            "ABI constant drift",
            _SELF_TEST_HEADER.replace("0x0007", "0x0006"),
            _SELF_TEST_IMPL,
            "NET_DEMO_ABI_VERSION",
        ),
        (
            "return type void -> int",
            _SELF_TEST_HEADER.replace(
                "void     net_demo_free", "int      net_demo_free"
            ),
            _SELF_TEST_IMPL,
            "returns",
        ),
        (
            "dropped leading handle argument",
            _SELF_TEST_HEADER.replace(
                "net_demo_serve(DemoHandle* h, uint64_t id",
                "net_demo_serve(uint64_t id",
            ),
            _SELF_TEST_IMPL,
            "arg(s)",
        ),
        ("reordered arguments", reordered, _SELF_TEST_IMPL, "arg 0"),
        (
            "pointer became a value",
            _SELF_TEST_HEADER.replace("DemoHandle* h", "DemoHandle h"),
            _SELF_TEST_IMPL,
            "arg 0",
        ),
        ("declared but not exported", ghost, _SELF_TEST_IMPL, "not exported"),
        (
            "header matched nothing (parser regression)",
            "#define NET_DEMO_ABI_VERSION 0x0007\n",
            _SELF_TEST_IMPL,
            "no net_demo_* declarations matched",
        ),
        (
            "impl matched nothing (parser regression)",
            _SELF_TEST_HEADER,
            "pub const NET_DEMO_ABI_VERSION: u32 = 0x0007;\n",
            "no net_demo_* exports matched",
        ),
    ]

    failures = 0
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        for label, header, impl, expect in cases:
            problems = check(_demo_surface(tmp, header, impl))
            joined = " | ".join(problems)
            if not expect:
                if problems:
                    print(f"FAIL  self-test [{label}]: expected clean, got {joined}")
                    failures += 1
                else:
                    print(f"ok    self-test [{label}]")
                continue
            if expect not in joined:
                print(
                    f"FAIL  self-test [{label}]: expected a problem mentioning "
                    f"{expect!r}, got {joined or '<nothing reported>'}"
                )
                failures += 1
            else:
                print(f"ok    self-test [{label}]")
    if failures:
        print()
        print(
            "The checker no longer detects drift it is meant to detect. Every "
            "`ok` it prints is worthless until this passes."
        )
    return 1 if failures else 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()

    failed = False
    for sf in SURFACES:
        missing = [p for p in (sf.header, sf.impl) if not p.is_file()]
        if missing:
            print(f"FAIL  {sf.name}: missing {', '.join(str(p) for p in missing)}")
            failed = True
            continue
        problems = check(sf)
        if problems:
            failed = True
            print(f"FAIL  {sf.name} ({sf.header} vs {sf.impl}):")
            for p in problems:
                print(f"  {p}")
        else:
            print(f"ok    {sf.name}: {sf.header} matches {sf.impl}")
    if failed:
        print()
        print(
            "A public C header that disagrees with its implementation hands "
            "consumers the wrong signature. Fix the header, or the export, "
            "so they agree."
        )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
