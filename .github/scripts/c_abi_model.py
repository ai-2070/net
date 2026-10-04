"""A shared type model of Net's C ABI: the shipped headers and the Rust
definitions behind them, parsed into one comparable form.

Used by `check-c-abi.py` (the C SDK audit, C1 of
docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md).

WHY A MODEL, NOT SHAPES. `check-rpc-abi-parity.py` normalises every type to
`ptr:<name>` / `val:<name>`, which cannot tell `uint8_t*` from `uint8_t**`,
compares a callback typedef by name rather than by its prototype, and needs a
hand-written alias table for every opaque handle. This model keeps:

  * the exact primitive, by width and signedness (`int` and `int32_t` are
    one type on every supported target; `char` is its own);
  * pointer depth, as nested `Ptr`;
  * function pointers expanded to `Fn(ret, args)`, through C typedefs and
    Rust `type` aliases and `Option<fn>`, so a callback is compared by its
    prototype;
  * non-primitive types (opaque handles, by-value structs) as `Named`. They
    are not matched by a table: `check-c-abi.py` requires each C name to
    correspond to ONE Rust type across every function, which is what catches
    two swapped handle arguments.

THE C SIDE is parsed with pycparser, after `cc -E` against stand-in
`stddef.h` / `stdint.h` / `stdbool.h` (tests/c_abi/fake_libc), so the parse
never sees a platform's system headers.

THE RUST SIDE is lexed, not compiled: comments and string contents are masked
(offsets preserved), so items, braces and attributes can be found reliably;
then each `extern "C" fn` is parsed with its attributes. Whether a definition
is COMPILED in a given build is decided by evaluating its `#[cfg]`
attributes, those of every enclosing inline module, and those on the `mod`
declarations that include its file, against the feature set `cargo tree`
resolves for that build.
"""

from __future__ import annotations

import os
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

# ------------------------------------------------------------------ types ----


@dataclass(frozen=True)
class Prim:
    name: str  # i8 u8 … i64 u64 isize usize f32 f64 bool void char long ulong

    def __str__(self) -> str:
        return self.name


@dataclass(frozen=True)
class Ptr:
    to: "Type"

    def __str__(self) -> str:
        return f"*{self.to}"


@dataclass(frozen=True)
class Named:
    name: str

    def __str__(self) -> str:
        return self.name


@dataclass(frozen=True)
class Fn:
    ret: "Type"
    args: tuple

    def __str__(self) -> str:
        return f"fn({', '.join(map(str, self.args))}) -> {self.ret}"


Type = Prim | Ptr | Named | Fn

VOID = Prim("void")

# C spellings -> canonical primitive. `int` is i32 on every target this ABI
# supports (LP64 Linux/macOS, LLP64 Windows); `long` is not, so it stays
# `long` and only ever equals itself.
C_PRIMS = {
    "void": "void",
    "_Bool": "bool",
    "bool": "bool",
    "char": "char",
    "signed char": "i8",
    "unsigned char": "u8",
    "short": "i16",
    "short int": "i16",
    "signed short": "i16",
    "unsigned short": "u16",
    "unsigned short int": "u16",
    "int": "i32",
    "signed": "i32",
    "signed int": "i32",
    "unsigned": "u32",
    "unsigned int": "u32",
    "long": "long",
    "long int": "long",
    "signed long": "long",
    "unsigned long": "ulong",
    "unsigned long int": "ulong",
    "long long": "i64",
    "long long int": "i64",
    "signed long long": "i64",
    "unsigned long long": "u64",
    "unsigned long long int": "u64",
    "float": "f32",
    "double": "f64",
    "int8_t": "i8",
    "uint8_t": "u8",
    "int16_t": "i16",
    "uint16_t": "u16",
    "int32_t": "i32",
    "uint32_t": "u32",
    "int64_t": "i64",
    "uint64_t": "u64",
    "intptr_t": "isize",
    "uintptr_t": "usize",
    "size_t": "usize",
    "ssize_t": "isize",
    "ptrdiff_t": "isize",
}

RUST_PRIMS = {
    "u8": "u8", "u16": "u16", "u32": "u32", "u64": "u64", "usize": "usize",
    "i8": "i8", "i16": "i16", "i32": "i32", "i64": "i64", "isize": "isize",
    "f32": "f32", "f64": "f64", "bool": "bool",
    "c_char": "char", "c_schar": "i8", "c_uchar": "u8",
    "c_short": "i16", "c_ushort": "u16",
    "c_int": "i32", "c_uint": "u32",
    "c_long": "long", "c_ulong": "ulong",
    "c_longlong": "i64", "c_ulonglong": "u64",
    "c_float": "f32", "c_double": "f64",
    "c_void": "void",
    "size_t": "usize", "ssize_t": "isize",
}


# ----------------------------------------------------------------- C side ----


@dataclass
class CDecl:
    name: str
    sig: Fn
    header: str
    line: int


@dataclass
class CHeader:
    path: Path
    functions: dict[str, CDecl] = field(default_factory=dict)
    typedefs: dict[str, Type] = field(default_factory=dict)
    # NAME -> (int value, header line) for `#define NAME <int>` and enumerators.
    constants: dict[str, tuple[int, int]] = field(default_factory=dict)


FAKE_LIBC = {
    "stddef.h": (
        "#ifndef FAKE_STDDEF_H\n#define FAKE_STDDEF_H\n"
        "typedef unsigned long size_t;\ntypedef long ptrdiff_t;\n"
        "#define NULL ((void*)0)\n#endif\n"
    ),
    "stdint.h": (
        "#ifndef FAKE_STDINT_H\n#define FAKE_STDINT_H\n"
        "typedef signed char int8_t;\ntypedef unsigned char uint8_t;\n"
        "typedef short int16_t;\ntypedef unsigned short uint16_t;\n"
        "typedef int int32_t;\ntypedef unsigned int uint32_t;\n"
        "typedef long long int64_t;\ntypedef unsigned long long uint64_t;\n"
        "typedef long intptr_t;\ntypedef unsigned long uintptr_t;\n"
        "#define UINT64_MAX 18446744073709551615ULL\n"
        "#define INT32_MAX 2147483647\n"
        "#endif\n"
    ),
    "stdbool.h": (
        "#ifndef FAKE_STDBOOL_H\n#define FAKE_STDBOOL_H\n"
        "#define bool _Bool\n#define true 1\n#define false 0\n#endif\n"
    ),
}

# The stand-in libc typedefs collapse back to the names the header wrote, so
# `size_t` stays `usize` rather than whatever `unsigned long` maps to.
_LIBC_TYPEDEF_NAMES = {
    "size_t", "ptrdiff_t", "int8_t", "uint8_t", "int16_t", "uint16_t",
    "int32_t", "uint32_t", "int64_t", "uint64_t", "intptr_t", "uintptr_t",
}

_DEFINE_INT = re.compile(
    r"^[ \t]*#[ \t]*define[ \t]+(NET_[A-Z0-9_]+)[ \t]+\(?[ \t]*(-?[ \t]*(?:0[xX][0-9a-fA-F]+|\d+))[uUlL]*[ \t]*\)?[ \t]*(?:/\*.*)?$",
    re.M,
)


def write_fake_libc(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for name, text in FAKE_LIBC.items():
        (directory / name).write_text(text, encoding="utf-8")
    return directory


def preprocess(header: Path, include_dirs: list[Path], fake_libc: Path, cc: str = "gcc") -> str:
    # The header is included from a one-line TU so its own include guard and
    # relative includes behave exactly as they do for a consumer.
    tu = f'#include "{header.name}"\n'
    cmd = [
        cc, "-E", "-nostdinc", "-x", "c", "-std=c11",
        "-I", str(fake_libc),
        *[a for d in include_dirs for a in ("-I", str(d))],
        "-D__attribute__(x)=", "-D__extension__=", "-",
    ]
    run = subprocess.run(cmd, input=tu, capture_output=True, text=True)
    if run.returncode != 0:
        raise RuntimeError(f"{cc} -E {header} failed:\n{run.stderr}")
    return run.stdout


def parse_c_header(header: Path, include_dirs: list[Path], fake_libc: Path, cc: str = "gcc") -> CHeader:
    from pycparser import c_ast, c_parser  # imported here: CI installs it

    text = preprocess(header, include_dirs, fake_libc, cc)
    ast = c_parser.CParser().parse(text, filename=str(header))
    out = CHeader(path=header)
    raw_typedefs: dict[str, object] = {}
    struct_typedef: dict[str, str] = {}

    for ext in ast.ext:
        if isinstance(ext, c_ast.Typedef):
            raw_typedefs[ext.name] = ext.type
            inner = ext.type.type if isinstance(ext.type, c_ast.TypeDecl) else None
            if isinstance(inner, (c_ast.Struct, c_ast.Union)) and inner.name:
                struct_typedef.setdefault(inner.name, ext.name)

    def conv(node, depth: int = 0) -> Type:
        if depth > 40:
            raise RuntimeError("typedef cycle")
        if isinstance(node, c_ast.PtrDecl):
            return Ptr(conv(node.type, depth + 1))
        if isinstance(node, c_ast.ArrayDecl):
            return Ptr(conv(node.type, depth + 1))
        if isinstance(node, c_ast.FuncDecl):
            args = []
            for p in (node.args.params if node.args else []):
                if isinstance(p, c_ast.EllipsisParam):
                    args.append(Named("..."))
                    continue
                t = conv(p.type, depth + 1)
                args.append(t)
            if args == [VOID]:
                args = []
            return Fn(conv(node.type, depth + 1), tuple(args))
        if isinstance(node, c_ast.TypeDecl):
            return conv(node.type, depth + 1)
        if isinstance(node, c_ast.IdentifierType):
            spelled = " ".join(node.names)
            if spelled in _LIBC_TYPEDEF_NAMES or spelled in C_PRIMS and spelled not in raw_typedefs:
                return Prim(C_PRIMS[spelled])
            if spelled in raw_typedefs:
                target = raw_typedefs[spelled]
                inner = target.type if isinstance(target, c_ast.TypeDecl) else None
                if isinstance(inner, (c_ast.Struct, c_ast.Union)):
                    return Named(spelled)
                if isinstance(inner, c_ast.Enum):
                    return Prim("i32")
                return conv(target, depth + 1)
            if spelled in C_PRIMS:
                return Prim(C_PRIMS[spelled])
            return Named(spelled)
        if isinstance(node, (c_ast.Struct, c_ast.Union)):
            return Named(struct_typedef.get(node.name, f"struct {node.name}"))
        if isinstance(node, c_ast.Enum):
            return Prim("i32")
        raise RuntimeError(f"unhandled C type node {type(node).__name__}")

    for name, node in raw_typedefs.items():
        try:
            out.typedefs[name] = conv(node)
        except RuntimeError:
            pass

    header_name = header.name
    for ext in ast.ext:
        if isinstance(ext, c_ast.Decl) and isinstance(ext.type, c_ast.FuncDecl):
            coord = ext.coord
            if coord and coord.file and Path(coord.file).name != header_name:
                continue  # declared by an included header; it reports it itself
            sig = conv(ext.type)
            out.functions[ext.name] = CDecl(ext.name, sig, header_name, coord.line if coord else 0)
        if isinstance(ext, c_ast.Decl) and ext.type is not None:
            # Enumerators with constant values.
            t = ext.type.type if isinstance(ext.type, c_ast.TypeDecl) else ext.type
            if isinstance(t, c_ast.Enum) and t.values:
                _collect_enum(t, out)
        if isinstance(ext, c_ast.Typedef):
            t = ext.type.type if isinstance(ext.type, c_ast.TypeDecl) else None
            if isinstance(t, c_ast.Enum) and t.values:
                _collect_enum(t, out)

    raw = header.read_text(encoding="utf-8")
    for m in _DEFINE_INT.finditer(raw):
        value = int(m.group(2).replace(" ", ""), 0)
        out.constants.setdefault(m.group(1), (value, raw.count("\n", 0, m.start()) + 1))
    return out


def _collect_enum(enum_node, out: CHeader) -> None:
    from pycparser import c_ast

    nxt = 0
    for e in enum_node.values.enumerators:
        if e.value is not None:
            nxt = _eval_c_int(e.value, out)
        if e.name.startswith("NET_"):
            out.constants.setdefault(e.name, (nxt, e.coord.line if e.coord else 0))
        nxt += 1


def _eval_c_int(node, out: CHeader) -> int:
    from pycparser import c_ast

    if isinstance(node, c_ast.Constant):
        return int(node.value.rstrip("uUlL"), 0)
    if isinstance(node, c_ast.UnaryOp) and node.op == "-":
        return -_eval_c_int(node.expr, out)
    if isinstance(node, c_ast.UnaryOp) and node.op == "+":
        return _eval_c_int(node.expr, out)
    if isinstance(node, c_ast.ID) and node.name in out.constants:
        return out.constants[node.name][0]
    raise RuntimeError(f"cannot evaluate enumerator value {node}")


# -------------------------------------------------------------- Rust side ----


def mask_rust(src: str) -> str:
    """`src` with comments blanked and string/char literal CONTENTS blanked,
    every offset and newline preserved. Quotes stay, so `extern "C"` still
    reads as `extern " "`."""
    out = list(src)
    i, n = 0, len(src)

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and re.match(r'r#*"', src[i:i + 8]) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            m = re.match(r'r(#*)"', src[i:])
            hashes = m.group(1)
            start = i + len(m.group(0))
            end = src.find('"' + hashes, start)
            end = n if end < 0 else end
            blank(start, end)
            i = end + 1 + len(hashes)
        elif c == "b" and i + 1 < n and src[i + 1] == '"' and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            i += 1  # byte string: handled as a string next iteration
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i + 1, min(j, n))
            i = j + 1
        elif c == "'":
            m = re.match(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'", src[i:i + 12])
            if m:
                blank(i + 1, i + len(m.group(0)) - 1)
                i += len(m.group(0))
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def match_close(masked: str, open_at: int, open_ch: str = "(", close_ch: str = ")") -> int:
    depth = 0
    for k in range(open_at, len(masked)):
        ch = masked[k]
        if ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return k
    raise ValueError(f"unbalanced {open_ch} at {open_at}")


_ATTRS = r"(?P<attrs>(?:#\[[^\]]*\]\s*)*)"
_VIS = r"(?:pub(?:\s*\([^)]*\))?\s+)?"
_FN_ITEM = re.compile(_ATTRS + _VIS + r'(?:unsafe\s+)?extern\s+"\s*"\s+fn\s+(?P<name>\w+)\s*\(')
_MOD_ITEM = re.compile(_ATTRS + _VIS + r"mod\s+(?P<name>\w+)\s*(?P<kind>[{;])")
_TYPE_ALIAS = re.compile(_ATTRS + _VIS + r"type\s+(?P<name>\w+)\s*=\s*")
_ENUM_ITEM = re.compile(_ATTRS + _VIS + r"enum\s+(?P<name>\w+)")
_STRUCT_ITEM = re.compile(_ATTRS + _VIS + r"struct\s+(?P<name>\w+)\s*(?P<kind>[({;<])")
_CFG_ATTR = re.compile(r"#\[\s*cfg\s*\(")
# An inner attribute: `#![cfg(...)]` gates the module (file or inline block)
# that contains it, e.g. the `*_stubs.rs` files gate themselves this way.
_INNER_CFG = re.compile(r"#!\[\s*cfg\s*\(")
_REPR = re.compile(r"#\[\s*repr\s*\(\s*([^)]*)\)")


def cfg_exprs(attr_text: str, pattern: re.Pattern = _CFG_ATTR) -> list[str]:
    out = []
    for m in pattern.finditer(attr_text):
        open_at = attr_text.index("(", m.start())
        close = match_close(attr_text, open_at)
        out.append(attr_text[open_at + 1:close].strip())
    return out


def eval_cfg(expr: str, features: set[str], target_os: str) -> bool:
    """Evaluate a cfg predicate. Unknown predicates raise, so a new kind of
    gate is noticed rather than silently guessed."""
    expr = expr.strip()
    m = re.fullmatch(r"(all|any|not)\s*\((.*)\)", expr, re.S)
    if m:
        parts = _split_top(m.group(2))
        vals = [eval_cfg(p, features, target_os) for p in parts if p.strip()]
        if m.group(1) == "all":
            return all(vals)
        if m.group(1) == "any":
            return any(vals)
        if len(vals) != 1:
            raise ValueError(f"not() takes one predicate: {expr}")
        return not vals[0]
    m = re.fullmatch(r'feature\s*=\s*"([^"]+)"', expr)
    if m:
        return m.group(1) in features
    m = re.fullmatch(r'target_os\s*=\s*"([^"]+)"', expr)
    if m:
        return m.group(1) == target_os
    m = re.fullmatch(r'target_family\s*=\s*"([^"]+)"', expr)
    if m:
        return m.group(1) == ("windows" if target_os == "windows" else "unix")
    m = re.fullmatch(r'target_pointer_width\s*=\s*"(\d+)"', expr)
    if m:
        return m.group(1) == "64"
    if expr == "windows":
        return target_os == "windows"
    if expr == "unix":
        return target_os != "windows"
    if expr in ("test", "doc", "doctest", "loom", "miri", "debug_assertions", "fuzzing", "coverage", "kani"):
        return False
    m = re.fullmatch(r'target_arch\s*=\s*"([^"]+)"', expr)
    if m:
        return m.group(1) == "x86_64"
    raise ValueError(f"unknown cfg predicate: {expr}")


def _split_top(s: str) -> list[str]:
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch in "(<[":
            depth += 1
        elif ch in ")>]":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    out.append(cur)
    return [p for p in out if p.strip()]


@dataclass
class RustDef:
    name: str
    sig_text: tuple[list[str], str]  # (arg type texts, return type text)
    file: Path
    line: int
    crate: str
    cfgs: list[str]
    exported: bool
    sig: Fn | None = None
    active: bool | None = None


@dataclass
class Crate:
    name: str
    root: Path  # src/lib.rs
    files: dict[Path, list[str]] = field(default_factory=dict)  # file -> gating cfgs
    aliases: dict[str, str] = field(default_factory=dict)  # alias -> type text
    enums: dict[str, str] = field(default_factory=dict)  # name -> repr
    structs: dict[str, str] = field(default_factory=dict)  # name -> repr or ""
    transparent: dict[str, str] = field(default_factory=dict)  # newtype -> inner text


def discover_crate(name: str, root: Path) -> Crate:
    """Every module file reachable from `root`, with the cfgs gating it."""
    crate = Crate(name, root)

    def walk(path: Path, gates: list[str], mod_dir: Path) -> None:
        if path in crate.files or not path.is_file():
            return
        src = path.read_text(encoding="utf-8")
        masked = mask_rust(src)
        spans = inline_mod_spans(src, masked)
        gates = gates + file_inner_gates(src, masked, spans)
        crate.files[path] = gates
        for m in _MOD_ITEM.finditer(masked):
            if m.group("kind") != ";":
                continue
            attrs = src[m.start("attrs"):m.end("attrs")]
            child_gates = gates + inline_gates_at(spans, m.start()) + cfg_exprs(attrs)
            pm = re.search(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]', attrs)
            mod = m.group("name")
            if pm:
                child = (path.parent / pm.group(1)).resolve()
                walk(child, child_gates, child.parent / child.stem)
                continue
            cand = [mod_dir / f"{mod}.rs", mod_dir / mod / "mod.rs"]
            for c in cand:
                if c.is_file():
                    walk(c, child_gates, c.parent if c.name == "mod.rs" else c.parent / c.stem)
                    break

    walk(root, [], root.parent)
    for f in crate.files:
        src = f.read_text(encoding="utf-8")
        masked = mask_rust(src)
        for m in _TYPE_ALIAS.finditer(masked):
            end = masked.index(";", m.end())
            crate.aliases.setdefault(m.group("name"), src[m.end():end].strip())
        for m in _ENUM_ITEM.finditer(masked):
            rep = _REPR.search(src[m.start("attrs"):m.end("attrs")])
            if rep:
                crate.enums[m.group("name")] = rep.group(1).strip()
        for m in _STRUCT_ITEM.finditer(masked):
            attrs = src[m.start("attrs"):m.end("attrs")]
            rep = _REPR.search(attrs)
            r = rep.group(1).strip() if rep else ""
            crate.structs.setdefault(m.group("name"), r)
            if r == "transparent" and m.group("kind") == "(":
                open_at = m.end("kind") - 1
                close = match_close(masked, open_at)
                crate.transparent[m.group("name")] = src[open_at + 1:close].strip().removeprefix("pub ").strip()
    return crate


def inline_mod_spans(src: str, masked: str) -> list[tuple[int, int, list[str]]]:
    spans = []
    for m in _MOD_ITEM.finditer(masked):
        if m.group("kind") != "{":
            continue
        open_at = m.end("kind") - 1
        close = match_close(masked, open_at, "{", "}")
        spans.append((open_at, close, cfg_exprs(src[m.start("attrs"):m.end("attrs")])))
    # Inner `#![cfg]` attributes gate the innermost block that holds them.
    for m in _INNER_CFG.finditer(masked):
        holders = [k for k, (a, b, _) in enumerate(spans) if a < m.start() < b]
        if not holders:
            continue  # file level: see file_inner_gates
        k = min(holders, key=lambda j: spans[j][1] - spans[j][0])
        open_at = masked.index("(", m.start())
        close = match_close(masked, open_at)
        spans[k][2].append(src[open_at + 1:close].strip())
    return spans


def file_inner_gates(src: str, masked: str, spans: list[tuple[int, int, list[str]]]) -> list[str]:
    """`#![cfg(...)]` attributes at file level (outside any inline module)."""
    out = []
    for m in _INNER_CFG.finditer(masked):
        if any(a < m.start() < b for a, b, _ in spans):
            continue
        open_at = masked.index("(", m.start())
        close = match_close(masked, open_at)
        out.append(src[open_at + 1:close].strip())
    return out


def inline_gates_at(spans: list[tuple[int, int, list[str]]], pos: int) -> list[str]:
    out = []
    for a, b, cfgs in spans:
        if a < pos < b:
            out += cfgs
    return out


def rust_defs(crate: Crate) -> list[RustDef]:
    defs = []
    for path, gates in crate.files.items():
        src = path.read_text(encoding="utf-8")
        masked = mask_rust(src)
        spans = inline_mod_spans(src, masked)
        for m in _FN_ITEM.finditer(masked):
            attrs = src[m.start("attrs"):m.end("attrs")]
            exported = bool(re.search(r"#\[\s*(?:unsafe\s*\(\s*)?(?:no_mangle|export_name)", attrs))
            open_at = m.end() - 1
            close = match_close(masked, open_at)
            # Types carry no string literals, so the masked text is the
            # source minus its comments, which is what a type needs.
            args = [a.split(":", 1)[1].strip() if ":" in a else a.strip()
                    for a in _split_top(masked[open_at + 1:close])]
            rest = masked[close + 1:]
            rm = re.match(r"\s*->\s*", rest)
            ret = "()"
            if rm:
                stop = re.search(r"[{;]|\bwhere\b", rest[rm.end():])
                ret = masked[close + 1 + rm.end(): close + 1 + rm.end() + stop.start()].strip()
            defs.append(RustDef(
                name=m.group("name"),
                sig_text=(args, ret),
                file=path,
                line=src.count("\n", 0, m.start("name")) + 1,
                crate=crate.name,
                cfgs=gates + inline_gates_at(spans, m.start()) + cfg_exprs(attrs),
                exported=exported,
            ))
    return defs


# Rust type parsing --------------------------------------------------------

_RTOK = re.compile(r'\s*(->|::|"[^"]*"|[A-Za-z_][A-Za-z0-9_]*|\d+|[*&<>(),:;\[\]!\'])')


class RustTypeParser:
    def __init__(self, crate: Crate, crates: dict[str, Crate]):
        self.crate = crate
        self.crates = crates
        self._resolving: set[str] = set()

    def parse(self, text: str) -> Type:
        self.toks = [t for t in _RTOK.findall(text)]
        self.i = 0
        t = self._type()
        if self.i != len(self.toks):
            raise ValueError(f"trailing tokens in Rust type {text!r}: {self.toks[self.i:]}")
        return t

    def _peek(self, k: int = 0) -> str | None:
        j = self.i + k
        return self.toks[j] if j < len(self.toks) else None

    def _eat(self, tok: str | None = None) -> str:
        t = self._peek()
        if t is None or (tok is not None and t != tok):
            raise ValueError(f"expected {tok!r}, got {t!r} in {self.toks}")
        self.i += 1
        return t

    def _type(self) -> Type:
        t = self._peek()
        if t == "*":
            self._eat()
            if self._peek() in ("mut", "const"):
                self._eat()
            return Ptr(self._type())
        if t == "&":
            self._eat()
            if self._peek() and self._peek().startswith("'"):
                self._eat()
            if self._peek() == "mut":
                self._eat()
            return Ptr(self._type())
        if t == "(":
            self._eat()
            self._eat(")")
            return VOID
        if t == "!":
            self._eat()
            return VOID
        if t in ("unsafe", "extern", "fn"):
            return self._fnptr()
        return self._path_type()

    def _fnptr(self) -> Type:
        if self._peek() == "unsafe":
            self._eat()
        if self._peek() == "extern":
            self._eat()
            if self._peek() and self._peek().startswith('"'):
                self._eat()
        self._eat("fn")
        self._eat("(")
        args = []
        while self._peek() != ")":
            # optional `name:` before the type
            if self._peek(1) == ":" and self._peek(2) != ":":
                self._eat()
                self._eat(":")
            args.append(self._type())
            if self._peek() == ",":
                self._eat()
        self._eat(")")
        ret: Type = VOID
        if self._peek() == "->":
            self._eat()
            ret = self._type()
        return Ptr(Fn(ret, tuple(args)))

    def _path_type(self) -> Type:
        segs = [self._eat()]
        while self._peek() == "::":
            self._eat()
            segs.append(self._eat())
        generics: list[Type] = []
        raw_generic = ""
        if self._peek() == "<":
            start = self.i
            self._eat()
            while self._peek() != ">":
                generics.append(self._type())
                if self._peek() == ",":
                    self._eat()
            self._eat(">")
            raw_generic = "".join(self.toks[start:self.i])
        name = segs[-1]
        if name == "Option" and len(generics) == 1:
            return generics[0]  # nullable pointer / fn pointer: same ABI
        if name == "NonNull" and len(generics) == 1:
            return Ptr(generics[0])
        if generics:
            return Named(name + raw_generic)
        return self._resolve(name)

    def _resolve(self, name: str) -> Type:
        if name in RUST_PRIMS:
            return Prim(RUST_PRIMS[name])
        for crate in [self.crate, *self.crates.values()]:
            if name in crate.enums:
                rep = crate.enums[name]
                if rep in RUST_PRIMS:
                    return Prim(RUST_PRIMS[rep])
                return Prim("i32")  # repr(C) enum: C int
            if name in crate.transparent:
                return self._alias(name, crate.transparent[name], crate)
            if name in crate.aliases:
                return self._alias(name, crate.aliases[name], crate)
        return Named(name)

    def _alias(self, name: str, text: str, crate: Crate) -> Type:
        if name in self._resolving:
            return Named(name)
        self._resolving.add(name)
        try:
            sub = RustTypeParser(crate, self.crates)
            sub._resolving = self._resolving
            return sub.parse(text)
        finally:
            self._resolving.discard(name)


# ------------------------------------------------------------- features ----


def resolved_features(crate_dir: Path, package: str, extra_features: list[str]) -> dict[str, set[str]]:
    """package name -> features cargo resolves for building `package`."""
    cmd = ["cargo", "tree", "-p", package, "-e", "normal", "-f", "{p}|{f}", "--prefix", "none"]
    if extra_features:
        cmd += ["--features", ",".join(extra_features)]
    out = subprocess.run(cmd, cwd=crate_dir, capture_output=True, text=True, check=True).stdout
    feats: dict[str, set[str]] = {}
    for line in out.splitlines():
        if "|" not in line:
            continue
        left, f = line.split("|", 1)
        pkg = left.split()[0]
        feats.setdefault(pkg, set()).update(x for x in f.replace("(*)", "").strip().split(",") if x)
    return feats


def package_name(cargo_toml: Path) -> str:
    m = re.search(r'^\[package\][^\[]*?^name\s*=\s*"([^"]+)"', cargo_toml.read_text(encoding="utf-8"), re.M | re.S)
    if not m:
        raise ValueError(f"no package name in {cargo_toml}")
    return m.group(1)


def target_os_of(triple: str) -> str:
    if "windows" in triple:
        return "windows"
    if "apple" in triple or "darwin" in triple:
        return "macos"
    return "linux"


def host_triple(crate_dir: Path) -> str:
    out = subprocess.run(["rustc", "-vV"], cwd=crate_dir, capture_output=True, text=True, check=True).stdout
    return re.search(r"^host: (\S+)", out, re.M).group(1)


def default_cc() -> str:
    return os.environ.get("CC", "gcc")


# -------------------------------------------------------------- constants ----

_RUST_CONST = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?const\s+(?P<name>NET_[A-Z0-9_]+)\s*:\s*(?P<ty>[^=]+?)\s*=\s*(?P<val>[^;]+);"
)
_COMMENT = re.compile(r"/\*.*?\*/", re.S)
_NET_TOKEN = re.compile(r"\bNET_[A-Z0-9_]*[A-Z0-9]\b")


@dataclass
class RustConst:
    name: str
    value: int | None  # None when the initialiser is not an integer literal
    expr: str
    file: Path
    line: int


def rust_consts(crate: Crate) -> list[RustConst]:
    out = []
    for path in crate.files:
        src = path.read_text(encoding="utf-8")
        masked = mask_rust(src)
        for m in _RUST_CONST.finditer(masked):
            expr = masked[m.start("val"):m.end("val")].strip()
            try:
                value = int(expr.replace("_", ""), 0)
            except ValueError:
                value = None
            out.append(RustConst(m.group("name"), value, expr, path,
                                 src.count("\n", 0, m.start("name")) + 1))
    return out


def comment_mentions(header: Path) -> dict[str, int]:
    """NET_* names a header's comments mention, with the first line of each.
    A name ending in `_` (`NET_ERR_BLOB_*` written as a family) is a prefix,
    not a name, and is skipped by the pattern."""
    raw = header.read_text(encoding="utf-8")
    out: dict[str, int] = {}
    for c in _COMMENT.finditer(raw):
        for t in _NET_TOKEN.finditer(c.group(0)):
            if raw[c.start() + t.end():c.start() + t.end() + 1] in ("_", "*"):
                continue  # `NET_ERR_BLOB_*` / `NET_ERR_TOKEN_x`-style families
            out.setdefault(t.group(0), raw.count("\n", 0, c.start() + t.start()) + 1)
    return out
