#!/usr/bin/env python3
"""Audit a staged C SDK bundle against the Rust code that implements it.

C1 of docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md. The bundle is
what a consumer compiles and links against (`make-c-bundle.py`): its
`include/` headers and the export set of its one library. This checks that
pair against every `extern "C"` definition in the Rust tree, under the
feature set that bundle was actually built with.

CHECKS.

  declared  Every function a shipped header declares is exported by the
            bundle's library, and is backed by exactly one Rust definition
            compiled in this profile, reported as `real` or as a `stub`
            (a `*_stubs.rs` body). A stub satisfies the linker and provides
            no behaviour; only a consumer program proves a feature works.
  exported  Every symbol the library exports is declared by a shipped
            header, or is on the allowlist with a reason.
  signature Each declaration matches EVERY Rust definition of that name,
            compiled here or not (a stub must agree with the real body):
            argument count and order, primitive width and signedness,
            pointer depth, and callback prototypes expanded through C
            typedefs, Rust `type` aliases and `Option<fn>`.
  handles   Opaque and by-value types are not matched by a table. Each C
            type name must correspond to ONE Rust type across every
            function; a C name meeting two Rust types is how two swapped
            handle arguments show up. A `void*` on either side matches
            anything (an erased handle) and is not counted.
  seams     The production export set carries no test seam.

Findings that are known and accepted live in tests/c_abi/allowlist.toml,
each with a reason. Anything else fails.

  python3 .github/scripts/check-c-abi.py --bundle <staged bundle>
  python3 .github/scripts/check-c-abi.py --self-test

Needs Python 3.11+ (tomllib), pycparser, and a C preprocessor (`$CC -E`,
default `gcc`).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import c_abi_model as M  # noqa: E402

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "net" / "crates" / "net"
ALLOWLIST = CRATE / "tests" / "c_abi" / "allowlist.toml"
LAYOUT = CRATE / "tests" / "c_abi" / "layout.json"
SEAM_PATTERNS = ("_test_", "net_blob_test_barrier_")
PROFILE_FEATURES = {"production": [], "helper": ["net-ffi/test-helpers"]}


@dataclass
class Finding:
    check: str
    key: str  # what the allowlist matches on
    message: str


# ---------------------------------------------------------------- compare ----

CHAR_FAMILY = {"char", "i8", "u8"}


class Comparer:
    def __init__(self) -> None:
        # C name -> {Rust name -> [where]}
        self.pairs: dict[str, dict[str, list[str]]] = defaultdict(lambda: defaultdict(list))
        # (C side, Rust side, where) for each named type meeting a `void`:
        # ABI-identical, but outside the handle check, so it is counted.
        self.erased: list[tuple[str, str, str]] = []

    def diff(self, c: M.Type, r: M.Type, where: str) -> list[tuple[str, str]]:
        """(kind, description) for each disagreement. kind is `abi` or `char`."""
        if isinstance(c, M.Named) and isinstance(r, M.Named):
            self.pairs[c.name][r.name].append(where)
            return []
        if isinstance(c, M.Named) and r == M.VOID or c == M.VOID and isinstance(r, M.Named):
            self.erased.append((str(c), str(r), where))
            return []  # an erased handle: same ABI, invisible to the handle check
        if isinstance(c, M.Prim) and isinstance(r, M.Prim):
            if c == r:
                return []
            if c.name in CHAR_FAMILY and r.name in CHAR_FAMILY:
                return [("char", f"{where}: {c} in C, {r} in Rust")]
            return [("abi", f"{where}: {c} in C, {r} in Rust")]
        if isinstance(c, M.Ptr) and isinstance(r, M.Ptr):
            return self.diff(c.to, r.to, where + "*")
        if isinstance(c, M.Arr) and isinstance(r, M.Arr):
            if c.n != r.n:
                return [("abi", f"{where}: {c} in C, {r} in Rust")]
            return self.diff(c.of, r.of, where + "[]")
        if isinstance(c, M.Fn) and isinstance(r, M.Fn):
            if len(c.args) != len(r.args):
                return [("abi", f"{where}: callback takes {len(c.args)} arg(s) in C ({c}), {len(r.args)} in Rust ({r})")]
            out = []
            for i, (a, b) in enumerate(zip(c.args, r.args)):
                out += self.diff(a, b, f"{where}.arg{i}")
            out += self.diff(c.ret, r.ret, f"{where}.ret")
            return out
        return [("abi", f"{where}: {c} in C, {r} in Rust")]

    def signature(self, name: str, c: M.Fn, r: M.Fn) -> list[tuple[str, str]]:
        if len(c.args) != len(r.args):
            return [("abi", f"{name}: takes {len(c.args)} arg(s) in C ({c}), {len(r.args)} in Rust ({r})")]
        out = []
        for i, (a, b) in enumerate(zip(c.args, r.args)):
            out += self.diff(a, b, f"{name}.arg{i}")
        out += self.diff(c.ret, r.ret, f"{name}.ret")
        return out


# ------------------------------------------------------------------ model ----


@dataclass
class Model:
    headers: dict[str, M.CHeader]
    decls: dict[str, list[M.CDecl]]
    defs: dict[str, list[M.RustDef]]
    exports: set[str]
    profile: str
    # Rust NET_* consts of the FFI crates, by name.
    rust_consts: dict[str, list[M.RustConst]] = field(default_factory=dict)
    # header name -> {NET_* name a comment mentions -> line}
    mentions: dict[str, dict[str, int]] = field(default_factory=dict)
    include_dir: Path | None = None
    # C struct (typedef name or `struct tag`) -> (header, line, [(field, Type)])
    c_structs: dict[str, tuple[str, int, list]] = field(default_factory=dict)
    crates: dict[str, M.Crate] = field(default_factory=dict)
    lib_names: dict[str, str] = field(default_factory=dict)  # Rust lib name -> package
    # The reviewed layout fixture, tests/c_abi/layout.json; None skips the check.
    layout: dict | None = None


def ffi_crates(crate_dir: Path) -> dict[str, M.Crate]:
    crates = {"net-mesh": M.discover_crate("net-mesh", crate_dir / "src" / "lib.rs")}
    for d in sorted((crate_dir / "bindings" / "go").glob("*-ffi")):
        name = M.package_name(d / "Cargo.toml")
        crates[name] = M.discover_crate(name, d / "src" / "lib.rs")
    return crates


def load_rust_consts(crate_dir: Path, crates: dict[str, M.Crate]) -> dict[str, list[M.RustConst]]:
    """NET_* consts of the FFI code only: the codes the C ABI can return."""
    ffi_root = (crate_dir / "src" / "ffi").resolve()
    out: dict[str, list[M.RustConst]] = defaultdict(list)
    for crate in crates.values():
        for c in M.rust_consts(crate):
            path = c.file.resolve()
            if crate.name == "net-mesh" and ffi_root not in path.parents:
                continue
            out[c.name].append(c)
    return out


def load_rust(crate_dir: Path, profile: str, target_os: str) -> dict[str, list[M.RustDef]]:
    crates = ffi_crates(crate_dir)
    feats = M.resolved_features(crate_dir, "net-ffi", PROFILE_FEATURES[profile])
    defs: dict[str, list[M.RustDef]] = defaultdict(list)
    for crate in crates.values():
        parser = M.RustTypeParser(crate, crates)
        for d in M.rust_defs(crate):
            if not d.exported:
                continue
            d.active = all(M.eval_cfg(e, feats.get(d.crate, set()), target_os) for e in d.cfgs)
            d.sig = M.Fn(parser.parse(d.sig_text[1]), tuple(parser.parse(a) for a in d.sig_text[0]))
            defs[d.name].append(d)
    return defs


def load_model(bundle: Path, crate_dir: Path = CRATE) -> Model:
    prov = {}
    for line in (bundle / "PROVENANCE").read_text(encoding="utf-8").splitlines():
        if ": " in line:
            k, v = line.split(": ", 1)
            prov[k] = v
    profile = prov["profile"]
    target_os = M.target_os_of(prov["target"])
    inc = bundle / "include"
    fake = M.write_fake_libc(Path(tempfile.mkdtemp()) / "fake_libc")
    headers = {h.name: M.parse_c_header(h, [inc], fake, M.default_cc()) for h in sorted(inc.glob("*.h"))}
    decls: dict[str, list[M.CDecl]] = defaultdict(list)
    for h in headers.values():
        for d in h.functions.values():
            decls[d.name].append(d)
    exports = {
        line.strip()
        for line in (bundle / "EXPORTS").read_text(encoding="utf-8").splitlines()
        if line.strip()
    }
    model = Model(headers, decls, load_rust(crate_dir, profile, target_os), exports, profile)
    crates = ffi_crates(crate_dir)
    model.rust_consts = load_rust_consts(crate_dir, crates)
    model.crates = crates
    model.lib_names = lib_names(crate_dir)
    model.c_structs = {
        name: (h.path.name, line, fields)
        for h in headers.values()
        for name, (line, fields) in h.structs.items()
    }
    if LAYOUT.exists():
        model.layout = json.loads(LAYOUT.read_text(encoding="utf-8"))
    model.mentions = {h.name: M.comment_mentions(h) for h in sorted(inc.glob("*.h"))}
    model.include_dir = inc
    return model


# ----------------------------------------------------------------- checks ----


def run_checks(model: Model, compile_assertions: bool = True) -> tuple[list[Finding], dict]:
    findings: list[Finding] = []
    stats = {"declared": len(model.decls), "real": 0, "stub": 0, "erased": 0, "handle_types": 0}
    cmp = Comparer()

    for name in sorted(model.decls):
        defs = model.defs.get(name, [])
        active = [d for d in defs if d.active]
        where = ", ".join(sorted({f"{d.header}:{d.line}" for d in model.decls[name]}))
        if name in model.exports:
            if len(active) != 1:
                findings.append(Finding("declared", name,
                    f"{name} ({where}) is exported, but {len(active)} Rust definitions are compiled in "
                    f"this profile: {[_loc(d) for d in active] or 'none'}"))
            else:
                kind = "stub" if active[0].file.name.endswith("_stubs.rs") else "real"
                stats[kind] += 1
        elif active:
            findings.append(Finding("declared", name,
                f"{name} ({where}) is compiled ({_loc(active[0])}) but not exported by the library"))
        elif defs:
            gates = sorted({" && ".join(d.cfgs) or "(none)" for d in defs})
            findings.append(Finding("declared", name,
                f"{name} ({where}) is declared, but no definition is compiled in the {model.profile} "
                f"profile; gated by: {gates}"))
        else:
            findings.append(Finding("declared", name,
                f"{name} ({where}) is declared, but no Rust definition exists"))

        for decl in model.decls[name]:
            for d in defs:
                for kind, msg in cmp.signature(name, decl.sig, d.sig):
                    findings.append(Finding("signature" if kind == "abi" else "char",
                        f"{name}", f"{msg}  [{decl.header}:{decl.line} vs {_loc(d)}]"))

    stats["helper_seams"] = []
    for name in sorted(model.exports - set(model.decls)):
        if model.profile == "helper" and any(p in name for p in SEAM_PATTERNS):
            # Test seams of the helper build. Only the repair seams need a C
            # declaration (net_test_helpers.h, for repair.c); the rest are
            # declared by the Go test helpers that drive them. Listed, not
            # failed, so one cannot vanish unnoticed.
            stats["helper_seams"].append(name)
            continue
        findings.append(Finding("exported", name,
            f"{name} is exported but declared in no shipped header"
            + (f" (defined at {_loc(model.defs[name][0])})" if model.defs.get(name) else "")))

    findings += run_layout_checks(model, cmp, compile_assertions)
    stats["layout_structs"] = len(model.c_structs)
    stats["erased"] = len(cmp.erased)
    stats["erased_list"] = cmp.erased
    stats["handle_types"] = len(cmp.pairs)
    for cname, rmap in sorted(cmp.pairs.items()):
        if len(rmap) > 1:
            detail = "; ".join(f"{r} at {', '.join(w[:3])}{' …' if len(w) > 3 else ''}"
                               for r, w in sorted(rmap.items()))
            findings.append(Finding("handles", cname,
                f"C type {cname} corresponds to {len(rmap)} Rust types: {detail}"))

    if model.profile == "production":
        for name in sorted(model.exports):
            if any(p in name for p in SEAM_PATTERNS):
                findings.append(Finding("seams", name, f"test seam {name} is in the production export set"))
    findings += run_constant_checks(model, compile_assertions)
    return findings, stats


_CODE_NAME = re.compile(r"^NET_(?:[A-Z0-9]+_)?ERR_|^NET_SUCCESS$|^NET_[A-Z0-9]+_OK$")


def family(name: str) -> str:
    """The return domain a code belongs to: every `NET_ERR_*` (and
    `NET_SUCCESS`) is one domain, the shared `net_error_t` space every surface
    returning `NetError` draws from; `NET_RPC_*`, `NET_ORG_*` and so on are
    each their own."""
    if name.startswith("NET_ERR_") or name == "NET_SUCCESS":
        return "NET_ERR"
    m = re.match(r"^(NET_[A-Z0-9]+)_", name)
    return m.group(1) if m else name


def declared_constants(model: Model) -> dict[str, list[tuple[int, str, int]]]:
    out: dict[str, list[tuple[int, str, int]]] = defaultdict(list)
    for hname, h in model.headers.items():
        for name, (value, line) in h.constants.items():
            out[name].append((value, hname, line))
    return out


def run_constant_checks(model: Model, compile_assertions: bool = True) -> list[Finding]:
    findings: list[Finding] = []
    declared = declared_constants(model)

    # One name, one value, in every header that declares it.
    for name, decls in sorted(declared.items()):
        if len({v for v, _, _ in decls}) > 1:
            findings.append(Finding("constants", name,
                f"{name} is declared with different values: "
                + ", ".join(f"{v} ({h}:{ln})" for v, h, ln in decls)))

    # Every code the Rust FFI defines is declared, with the Rust value.
    for name, consts in sorted(model.rust_consts.items()):
        rvalues = {c.value for c in consts}
        where = ", ".join(sorted({_loc_const(c) for c in consts}))
        if None in rvalues:
            findings.append(Finding("constants", name,
                f"{name} ({where}) has a non-literal initialiser {consts[0].expr!r}; "
                "the audit cannot compare it"))
            continue
        if len(rvalues) > 1:
            findings.append(Finding("constants", name,
                f"{name} has different values in Rust: {sorted(rvalues)} ({where})"))
            continue
        rv = rvalues.pop()
        if name not in declared:
            findings.append(Finding("constants", name,
                f"{name} = {rv} ({where}) is defined by the Rust FFI but declared in no shipped header"))
            continue
        hv = {v for v, _, _ in declared[name]}
        if hv != {rv}:
            findings.append(Finding("constants", name,
                f"{name} is {sorted(hv)} in the headers but {rv} in Rust ({where})"))

    # A name a header's comments tell the reader to compare against exists.
    for hname, mentioned in sorted(model.mentions.items()):
        for name, line in sorted(mentioned.items()):
            if name in declared or name.endswith("_H"):
                continue
            findings.append(Finding("mentioned", name,
                f"{hname}:{line} mentions {name}, which no shipped header declares"))

    # Two meanings with one value in one return domain.
    by_value: dict[tuple[str, int], set[str]] = defaultdict(set)
    for name, decls in declared.items():
        if _CODE_NAME.search(name):
            for v, _, _ in decls:
                by_value[(family(name), v)].add(name)
    for name, consts in model.rust_consts.items():
        if _CODE_NAME.search(name) and consts[0].value is not None:
            by_value[(family(name), consts[0].value)].add(name)
    for (fam, v), names in sorted(by_value.items()):
        names = sorted(names)
        for i in range(len(names)):
            for j in range(i + 1, len(names)):
                findings.append(Finding("collision", f"{names[i]}|{names[j]}",
                    f"{names[i]} and {names[j]} are both {v} in the {fam} return domain"))

    if compile_assertions and model.include_dir is not None:
        findings += compile_constant_assertions(model)
    return findings


def constant_assertions(model: Model, headers: list[str]) -> str:
    """A C translation unit asserting, through `headers`, every constant
    they declare that has a Rust value."""
    lines = ["/* Generated by check-c-abi.py: header constants equal their Rust values. */"]
    lines += [f'#include "{h}"' for h in headers]
    names: set[str] = set()
    for h in headers:
        names |= set(model.headers[h].constants)
    for name in sorted(names):
        consts = model.rust_consts.get(name)
        if consts and consts[0].value is not None:
            v = consts[0].value
            lines.append(f'_Static_assert(({name}) == ({v}), "{name} is not {v}");')
    return "\n".join(lines) + "\n"


def compile_constant_assertions(model: Model) -> list[Finding]:
    """`net.h` and `net.go.h` share an include guard, so each gets its own
    translation unit, with every other header."""
    findings = []
    others = [h for h in model.headers if h not in ("net.h", "net.go.h")]
    # One unit per guard-sharing base; with neither present, one unit of all.
    units = [[b, *others] for b in ("net.h", "net.go.h") if b in model.headers] or [others]
    with tempfile.TemporaryDirectory() as td:
        for headers in units:
            base = headers[0]
            tu = Path(td) / ("abi_constants_" + base.replace(".", "_") + ".c")
            tu.write_text(constant_assertions(model, headers), encoding="utf-8")
            run = subprocess.run(
                [M.default_cc(), "-std=c11", "-fsyntax-only", "-I", str(model.include_dir), str(tu)],
                capture_output=True, text=True,
            )
            if run.returncode != 0:
                out = (run.stdout + run.stderr).splitlines()[:20]
                findings.append(Finding("compiled", base,
                    f"constant assertions through {base} do not compile:\n"
                    + "\n".join("      " + line for line in out)))
    return findings


def lib_names(crate_dir: Path) -> dict[str, str]:
    """Rust lib name (what a path starts with) -> package name."""
    # Paths are as abi_layout.rs (in net-ffi) writes them, where `net::` is the
    # net-mesh dependency. net-ffi's own lib is also named `net`; it defines
    # no structs, so it is left out rather than shadowing net-mesh.
    out = {"net": "net-mesh"}
    for d in sorted((crate_dir / "bindings" / "go").glob("*-ffi")):
        if d.name == "net-ffi":
            continue
        toml = (d / "Cargo.toml").read_text(encoding="utf-8")
        pkg = M.package_name(d / "Cargo.toml")
        m = re.search(r'^\[lib\][^\[]*?^name\s*=\s*"([^"]+)"', toml, re.M | re.S)
        out[m.group(1) if m else pkg.replace("-", "_")] = pkg
    return out


def module_path(crate: M.Crate, file: Path) -> list[str]:
    parts = list(file.resolve().relative_to(crate.root.parent.resolve()).with_suffix("").parts)
    return parts[:-1] if parts and parts[-1] in ("mod", "lib") else parts


def c_type_spelling(name: str) -> str:
    return name  # a typedef name, or already `struct tag`


def run_layout_checks(model: Model, cmp: "Comparer", compile_assertions: bool = True) -> list[Finding]:
    """Check 4. The fixture is the meeting point: the Rust test
    (bindings/go/net-ffi/tests/abi_layout.rs) requires rustc's layout to
    equal it, and the C assertions compiled here require the headers' layout
    to equal it. This function makes sure the fixture covers exactly the
    published structs and that each entry pairs the right fields."""
    if model.layout is None:
        return []
    findings: list[Finding] = []
    entries: dict = model.layout.get("structs", {})
    for name in sorted(set(model.c_structs) - set(entries)):
        h, line, _ = model.c_structs[name]
        findings.append(Finding("layout", name,
            f"{name} ({h}:{line}) is published with a body but has no entry in tests/c_abi/layout.json"))
    for name in sorted(set(entries) - set(model.c_structs)):
        findings.append(Finding("layout", name,
            f"tests/c_abi/layout.json has {name}, which no shipped header defines"))

    for name in sorted(set(entries) & set(model.c_structs)):
        entry = entries[name]
        h, line, cfields = model.c_structs[name]
        path = entry["rust"].split("::")
        pkg = model.lib_names.get(path[0])
        crate = model.crates.get(pkg) if pkg else None
        if crate is None:
            findings.append(Finding("layout", name, f"{name}: fixture path {entry['rust']} names no FFI crate"))
            continue
        found = M.rust_struct_fields(crate, path[-1])
        if found is None:
            findings.append(Finding("layout", name, f"{name}: no `struct {path[-1]}` in {pkg}"))
            continue
        rfile, rline, rfields = found
        if module_path(crate, rfile) != path[1:-1]:
            findings.append(Finding("layout", name,
                f"{name}: fixture says {entry['rust']}, but {path[-1]} is in "
                f"{'::'.join([path[0], *module_path(crate, rfile)])}"))
        if len(cfields) != len(rfields):
            findings.append(Finding("layout", name,
                f"{name}: {len(cfields)} fields in {h}:{line}, {len(rfields)} in {path[-1]} ({_rel(rfile)}:{rline})"))
            continue
        pairs = [[cf, rf] for (cf, _), (rf, _) in zip(cfields, rfields)]
        if [f[:2] for f in entry["fields"]] != pairs:
            findings.append(Finding("layout", name,
                f"{name}: fixture field pairs {[f[:2] for f in entry['fields']]} differ from the "
                f"definitions' {pairs}"))
        parser = M.RustTypeParser(crate, model.crates)
        for (cf, ct), (rf, rt) in zip(cfields, rfields):
            for kind, msg in cmp.diff(ct, parser.parse(rt), f"{name}.{cf}"):
                findings.append(Finding("layout" if kind == "abi" else "char", name,
                    f"{msg}  [{h}:{line} vs {_rel(rfile)}:{rline}]"))

    if compile_assertions and model.include_dir is not None:
        findings += compile_layout_assertions(model, entries)
    return findings


def layout_assertions(model: Model, headers: list[str], entries: dict) -> str:
    lines = ["/* Generated by check-c-abi.py: header layouts equal tests/c_abi/layout.json. */",
             "#include <stddef.h>"]
    lines += [f'#include "{h}"' for h in headers]
    for name, entry in sorted(entries.items()):
        if name not in model.c_structs or model.c_structs[name][0] not in headers:
            continue
        t = c_type_spelling(name)
        lines.append(f'_Static_assert(sizeof({t}) == {entry["size"]}, "sizeof({t}) is not {entry["size"]}");')
        lines.append(f'_Static_assert(_Alignof({t}) == {entry["align"]}, "_Alignof({t}) is not {entry["align"]}");')
        for cf, _, off in entry["fields"]:
            lines.append(f'_Static_assert(offsetof({t}, {cf}) == {off}, "offsetof({t}, {cf}) is not {off}");')
    return "\n".join(lines) + "\n"


def compile_layout_assertions(model: Model, entries: dict) -> list[Finding]:
    findings = []
    others = [h for h in model.headers if h not in ("net.h", "net.go.h")]
    units = [[b, *others] for b in ("net.h", "net.go.h") if b in model.headers] or [others]
    with tempfile.TemporaryDirectory() as td:
        for headers in units:
            tu = Path(td) / ("abi_layout_" + headers[0].replace(".", "_") + ".c")
            tu.write_text(layout_assertions(model, headers, entries), encoding="utf-8")
            run = subprocess.run(
                [M.default_cc(), "-std=c11", "-fsyntax-only", "-I", str(model.include_dir), str(tu)],
                capture_output=True, text=True,
            )
            if run.returncode != 0:
                out = (run.stdout + run.stderr).splitlines()[:20]
                findings.append(Finding("compiled", "layout:" + headers[0],
                    f"layout assertions through {headers[0]} do not compile:\n"
                    + "\n".join("      " + line for line in out)))
    return findings


def emit_layout_table(model: Model) -> str:
    """The `measure!` table for abi_layout.rs, one line per published struct,
    with the Rust counterpart found through the same pairing the handle rule
    uses (signatures first, then fields of already-paired structs)."""
    cmp = Comparer()
    for n, ds in model.decls.items():
        for d in ds:
            for r in model.defs.get(n, []):
                cmp.signature(n, d.sig, r.sig)
    lines = []
    done: set[str] = set()
    progress = True
    while progress:
        progress = False
        for name, (h, line, cfields) in sorted(model.c_structs.items()):
            if name in done or len(cmp.pairs.get(name, {})) != 1:
                continue
            rname = next(iter(cmp.pairs[name]))
            for lib, pkg in model.lib_names.items():
                crate = model.crates.get(pkg)
                found = M.rust_struct_fields(crate, rname) if crate else None
                if not found:
                    continue
                rfile, _, rfields = found
                parser = M.RustTypeParser(crate, model.crates)
                for (cf, ct), (rf, rt) in zip(cfields, rfields):
                    cmp.diff(ct, parser.parse(rt), f"{name}.{cf}")
                path = "::".join([lib, *module_path(crate, rfile), rname])
                fields = ", ".join(f'"{cf}": {rf}' for (cf, _), (rf, _) in zip(cfields, rfields))
                lines.append(f'        measure!("{name}" => {path} {{ {fields} }}),')
                break
            done.add(name)
            progress = True
    missing = sorted(set(model.c_structs) - done)
    for name in missing:
        lines.append(f"        // {name}: no unique Rust counterpart found")
    return "\n".join(lines)


def _rel(path: Path) -> str:
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return path.as_posix()


def _loc_const(c: M.RustConst) -> str:
    try:
        rel = c.file.resolve().relative_to(ROOT)
    except ValueError:
        rel = c.file
    return f"{rel.as_posix()}:{c.line}"


def _loc(d: M.RustDef) -> str:
    try:
        rel = d.file.resolve().relative_to(ROOT)
    except ValueError:
        rel = d.file
    return f"{rel.as_posix()}:{d.line}"


def load_allowlist(path: Path) -> dict[str, dict[str, str]]:
    if not path.exists():
        return {}
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    return {k: dict(v) for k, v in data.items()}


def apply_allowlist(findings: list[Finding], allow: dict[str, dict[str, str]]) -> tuple[list[Finding], list[str]]:
    """(findings not allowlisted, allowlist entries that matched nothing)."""
    used: set[tuple[str, str]] = set()
    left = []
    for f in findings:
        if f.key in allow.get(f.check, {}):
            used.add((f.check, f.key))
        else:
            left.append(f)
    stale = [f"{c}.{k}" for c, entries in allow.items() for k in entries if (c, k) not in used]
    return left, stale


def report(findings: list[Finding], stats: dict, stale: list[str], profile: str, verbose: bool = False) -> int:
    print(f"==> {profile}: {stats['declared']} declared functions — "
          f"{stats['real']} real, {stats['stub']} stubs; {stats['handle_types']} C handle/struct "
          f"types matched one-to-one; {stats['erased']} positions where one side is void* "
          f"(ABI-identical, not handle-checked)")
    if stats.get("helper_seams"):
        print(f"    {len(stats['helper_seams'])} undeclared test seams in the helper build "
              f"(declared by their Go test helpers): {', '.join(stats['helper_seams'])}")
    if verbose:
        for c, r, where in stats["erased_list"]:
            print(f"    void*: {where}: C {c}, Rust {r}")
    by = defaultdict(list)
    for f in findings:
        by[f.check].append(f)
    for check in ("declared", "exported", "signature", "char", "handles", "seams",
                  "constants", "mentioned", "collision", "layout", "compiled"):
        items = by.get(check, [])
        mark = "✓" if not items else "✗"
        print(f"{mark} {check}: {len(items)} finding(s)")
        for f in items:
            print(f"    {f.message}")
    for s in stale:
        print(f"✗ allowlist entry {s} matched nothing; remove it")
    return 1 if findings or stale else 0


# -------------------------------------------------------------- self-test ----

_ST_HEADER = """\
#include <stdint.h>
#include <stddef.h>
typedef struct demo_s demo_t;
typedef struct other_s other_t;
typedef int (*demo_cb)(uint64_t id, const uint8_t* data, size_t len);
#define NET_DEMO_OK 0
#define NET_DEMO_ERR_CLOSED -1
/* Returns NET_DEMO_OK, or NET_DEMO_ERR_CLOSED. */
int net_demo_open(demo_t** out);
int net_demo_serve(demo_t* d, other_t* o, demo_cb cb);
void net_demo_free(demo_t* d);
uint32_t net_demo_count(const demo_t* d);
typedef struct {
    uint64_t id;
    uint32_t len;
} demo_pair_t;
uint32_t net_demo_pair_len(const demo_pair_t* p);
"""

_ST_RUST = """\
pub const NET_DEMO_OK: c_int = 0;
pub const NET_DEMO_ERR_CLOSED: c_int = -1;
pub struct DemoHandle { x: u8 }
pub struct OtherHandle { y: u8 }
pub type DemoCb = Option<unsafe extern "C" fn(id: u64, data: *const u8, len: usize) -> c_int>;
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_open(out: *mut *mut DemoHandle) -> c_int { 0 }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_serve(d: *mut DemoHandle, o: *mut OtherHandle, cb: DemoCb) -> c_int { 0 }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_free(d: *mut DemoHandle) { }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_count(d: *const DemoHandle) -> u32 { 0 }
#[repr(C)]
pub struct DemoPair {
    pub id: u64,
    pub len: u32,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_pair_len(p: *const DemoPair) -> u32 { 0 }
#[cfg(feature = "absent")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_gated(d: *mut DemoHandle) -> c_int { 0 }
"""

_ST_EXPORTS = {"net_demo_open", "net_demo_serve", "net_demo_free", "net_demo_count", "net_demo_pair_len"}
_ST_LAYOUT = {"structs": {"demo_pair_t": {
    "rust": "demo::DemoPair", "size": 16, "align": 8,
    "fields": [["id", "id", 0], ["len", "len", 8]],
}}}


def _self_test_model(tmp: Path, header: str, rust: str, exports: set[str], profile: str = "production",
                     layout: dict | None = None) -> Model:
    inc = tmp / "include"
    src = tmp / "crate" / "src"
    inc.mkdir(parents=True, exist_ok=True)
    src.mkdir(parents=True, exist_ok=True)
    (inc / "net_demo.h").write_text(header, encoding="utf-8")
    (src / "lib.rs").write_text(rust, encoding="utf-8")
    fake = M.write_fake_libc(tmp / "fake_libc")
    h = M.parse_c_header(inc / "net_demo.h", [inc], fake, M.default_cc())
    crate = M.discover_crate("demo", src / "lib.rs")
    parser = M.RustTypeParser(crate, {"demo": crate})
    defs: dict[str, list[M.RustDef]] = defaultdict(list)
    for d in M.rust_defs(crate):
        d.active = all(M.eval_cfg(e, set(), "linux") for e in d.cfgs)
        d.sig = M.Fn(parser.parse(d.sig_text[1]), tuple(parser.parse(a) for a in d.sig_text[0]))
        defs[d.name].append(d)
    decls = defaultdict(list)
    for d in h.functions.values():
        decls[d.name].append(d)
    model = Model({"net_demo.h": h}, decls, defs, set(exports), profile)
    consts: dict[str, list[M.RustConst]] = defaultdict(list)
    for c in M.rust_consts(crate):
        consts[c.name].append(c)
    model.rust_consts = consts
    model.mentions = {"net_demo.h": M.comment_mentions(inc / "net_demo.h")}
    model.include_dir = inc
    model.crates = {"demo": crate}
    model.lib_names = {"demo": "demo"}
    model.c_structs = {name: ("net_demo.h", line, fields) for name, (line, fields) in h.structs.items()}
    model.layout = _ST_LAYOUT if layout is None else layout
    return model


def self_test() -> int:
    H, R, E = _ST_HEADER, _ST_RUST, _ST_EXPORTS
    cases = [
        ("a matching pair is clean", H, R, E, None),
        ("declared, exported nowhere", H + "int net_demo_ghost(void);\n", R, E, ("declared", "no Rust definition")),
        ("declared, gated out of this profile", H + "int net_demo_gated(demo_t* d);\n", R, E,
         ("declared", "no definition is compiled")),
        ("exported but undeclared", H, R, E | {"net_demo_hidden"}, ("exported", "net_demo_hidden")),
        ("swapped argument order (same arity)",
         H.replace("net_demo_serve(demo_t* d, other_t* o", "net_demo_serve(other_t* o, demo_t* d"), R, E,
         ("handles", "corresponds to 2 Rust types")),
        ("changed width (uint32_t -> uint64_t)", H.replace("uint32_t net_demo_count", "uint64_t net_demo_count"), R, E,
         ("signature", "u64 in C, u32 in Rust")),
        ("changed pointer depth (demo_t** -> demo_t*)", H.replace("net_demo_open(demo_t** out)", "net_demo_open(demo_t* out)"), R, E,
         ("signature", "net_demo_open.arg0")),
        ("callback prototype changed under the same name",
         H.replace("(uint64_t id, const uint8_t* data, size_t len)", "(uint32_t id, const uint8_t* data, size_t len)"), R, E,
         ("signature", "arg2*.arg0")),
        ("callback arity changed under the same name",
         H.replace("(uint64_t id, const uint8_t* data, size_t len)", "(uint64_t id, const uint8_t* data)"), R, E,
         ("signature", "callback takes 2 arg(s)")),
        ("a pointer became a value", H.replace("void net_demo_free(demo_t* d)", "void net_demo_free(uint64_t d)"), R, E,
         ("signature", "net_demo_free.arg0")),
        ("a test seam in production exports", H, R, E | {"net_demo_test_seam"}, ("seams", "net_demo_test_seam")),
        ("a Rust code declared in no header", H, R + "pub const NET_DEMO_ERR_GONE: c_int = -2;\n", E,
         ("constants", "NET_DEMO_ERR_GONE = -2")),
        ("a header value that differs from Rust", H.replace("NET_DEMO_ERR_CLOSED -1", "NET_DEMO_ERR_CLOSED -3"), R, E,
         ("constants", "is [-3] in the headers but -1 in Rust")),
        ("the same mismatch fails the compiled assertions", H.replace("NET_DEMO_ERR_CLOSED -1", "NET_DEMO_ERR_CLOSED -3"), R, E,
         ("compiled", "static assertion failed")),
        ("a comment names a code nobody declares", H.replace("or NET_DEMO_ERR_CLOSED.", "or NET_DEMO_ERR_GHOST."), R, E,
         ("mentioned", "NET_DEMO_ERR_GHOST")),
        ("two codes share a value in one domain", H + "#define NET_DEMO_ERR_TWIN -1\n", R, E,
         ("collision", "NET_DEMO_ERR_CLOSED and NET_DEMO_ERR_TWIN are both -1")),
        ("a published struct with no layout entry", H, R, E, ("layout", "has no entry"), {"structs": {}}),
        ("a layout entry with a wrong offset fails the compiled assertions", H, R, E,
         ("compiled", "offsetof(demo_pair_t, len) is not 4"),
         {"structs": {"demo_pair_t": {**_ST_LAYOUT["structs"]["demo_pair_t"],
                                       "fields": [["id", "id", 0], ["len", "len", 4]]}}}),
        ("a struct field whose width changed",
         H.replace("    uint32_t len;\n} demo_pair_t;", "    uint64_t len;\n} demo_pair_t;"), R, E,
         ("layout", "demo_pair_t.len: u64 in C, u32 in Rust")),
        ("struct fields reordered",
         H.replace("    uint64_t id;\n    uint32_t len;", "    uint32_t len;\n    uint64_t id;"), R, E,
         ("layout", "fixture field pairs")),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as td:
        for i, (label, header, rust, exports, expect, *layout) in enumerate(cases):
            tmp = Path(td) / f"c{i}"
            model = _self_test_model(tmp, header, rust, exports, layout=layout[0] if layout else None)
            findings, _ = run_checks(model)
            if expect is None:
                ok = not findings
            else:
                check, text = expect
                ok = any(f.check == check and text in f.message for f in findings)
            print(f"{'✓' if ok else '✗'} self-test: {label}"
                  + ("" if ok else f"  (got: {[f'{f.check}: {f.message}' for f in findings]})"))
            failures += 0 if ok else 1
        # Allowlist: an entry suppresses exactly its finding, and a stale entry fails.
        f = [Finding("exported", "net_x", "m")]
        left, stale = apply_allowlist(f, {"exported": {"net_x": "reason", "net_gone": "reason"}})
        ok = not left and stale == ["exported.net_gone"]
        print(f"{'✓' if ok else '✗'} self-test: the allowlist suppresses its entry and reports a stale one")
        failures += 0 if ok else 1
        # cfg evaluation refuses an unknown predicate instead of guessing.
        try:
            M.eval_cfg('sanitize = "address"', set(), "linux")
            ok = False
        except ValueError:
            ok = True
        print(f"{'✓' if ok else '✗'} self-test: an unknown cfg predicate is an error, not a guess")
        failures += 0 if ok else 1
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bundle", type=Path, help="staged bundle (make-c-bundle.py)")
    ap.add_argument("--allowlist", type=Path, default=ALLOWLIST)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--verbose", action="store_true", help="list every position where one side is void*")
    ap.add_argument("--emit-layout-table", action="store_true",
                    help="print the measure! table for bindings/go/net-ffi/tests/abi_layout.rs")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if not args.bundle:
        ap.error("--bundle is required")
    model = load_model(args.bundle.resolve())
    if args.emit_layout_table:
        print(emit_layout_table(model))
        return 0
    findings, stats = run_checks(model)
    left, stale = apply_allowlist(findings, load_allowlist(args.allowlist))
    return report(left, stats, stale, model.profile, args.verbose)


if __name__ == "__main__":
    sys.exit(main())
