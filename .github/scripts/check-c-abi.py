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
import sys
import tempfile
import tomllib
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import c_abi_model as M  # noqa: E402

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / "net" / "crates" / "net"
ALLOWLIST = CRATE / "tests" / "c_abi" / "allowlist.toml"
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


def load_rust(crate_dir: Path, profile: str, target_os: str) -> dict[str, list[M.RustDef]]:
    crates = {"net-mesh": M.discover_crate("net-mesh", crate_dir / "src" / "lib.rs")}
    for d in sorted((crate_dir / "bindings" / "go").glob("*-ffi")):
        name = M.package_name(d / "Cargo.toml")
        crates[name] = M.discover_crate(name, d / "src" / "lib.rs")
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
    return Model(headers, decls, load_rust(crate_dir, profile, target_os), exports, profile)


# ----------------------------------------------------------------- checks ----


def run_checks(model: Model) -> tuple[list[Finding], dict[str, int]]:
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
    return findings, stats


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
    for check in ("declared", "exported", "signature", "char", "handles", "seams"):
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
int net_demo_open(demo_t** out);
int net_demo_serve(demo_t* d, other_t* o, demo_cb cb);
void net_demo_free(demo_t* d);
uint32_t net_demo_count(const demo_t* d);
"""

_ST_RUST = """\
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
#[cfg(feature = "absent")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn net_demo_gated(d: *mut DemoHandle) -> c_int { 0 }
"""

_ST_EXPORTS = {"net_demo_open", "net_demo_serve", "net_demo_free", "net_demo_count"}


def _self_test_model(tmp: Path, header: str, rust: str, exports: set[str], profile: str = "production") -> Model:
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
    return Model({"net_demo.h": h}, decls, defs, set(exports), profile)


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
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as td:
        for i, (label, header, rust, exports, expect) in enumerate(cases):
            tmp = Path(td) / f"c{i}"
            model = _self_test_model(tmp, header, rust, exports)
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
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if not args.bundle:
        ap.error("--bundle is required")
    model = load_model(args.bundle.resolve())
    findings, stats = run_checks(model)
    left, stale = apply_allowlist(findings, load_allowlist(args.allowlist))
    return report(left, stats, stale, model.profile, args.verbose)


if __name__ == "__main__":
    sys.exit(main())
