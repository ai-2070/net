#!/usr/bin/env python3
"""Compile the skill code snippets that opt in.

The other checks verify that a *name* exists. None of them verifies a signature,
an argument order, or a field type. This does, for the snippets that opt in.

**Opt-in, and the count is always printed.** Most snippets are fragments — of
187 language-tagged blocks, roughly a third carry their own imports and a
further 21 contain elisions. Extracting all of them would produce a wall of
failures that says nothing about correctness, and rewriting them to stand alone
would bloat the skills and hurt the reader. So a block is checked only when it
carries

    <!-- skill-check: compile -->

immediately above its fence, and the report always states how many were checked
out of how many exist. A bare "pass" that quietly covered 12% would be worse
than no check at all.

**A malformed or orphaned marker is an error, not a skip.** A marker that has
drifted away from its fence, or names an unknown directive, fails loudly —
otherwise the ratchet silently unwinds as files get edited.

**Preamble discipline.** Rust snippets are emitted into one generated crate, each
in its own `mod`, sharing a checked-in preamble. The preamble may contain
imports and harness scaffolding ONLY: no helper functions, no shims, and nothing
that defines a symbol the SDK is supposed to provide. Otherwise this proves the
harness rather than the documentation. The preamble is printed with any failure
so a reader can see exactly what was in scope.

Run:  .github/scripts/check-skill-snippets.py [--verbose]
"""

import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SKILLS = ROOT / ".claude/skills"
PREAMBLE = ROOT / ".github/skill-snippets/rust-preamble.rs"
OUT = ROOT / "target/skill-snippets/rust"

MARKER = re.compile(r"<!--\s*skill-check:\s*(.*?)\s*-->")
FENCE = re.compile(r"^```(\w+)\n(.*?)^```", re.S | re.M)
KNOWN_DIRECTIVES = {"compile"}


def scan():
    """(marked, totals, errors). `marked` is [(file, lang, body, line)]."""
    marked, totals, errors = [], {}, []
    for md in sorted(SKILLS.rglob("*.md")):
        text = md.read_text()
        rel = md.relative_to(ROOT)

        for m in FENCE.finditer(text):
            lang = {"typescript": "ts"}.get(m.group(1), m.group(1))
            if lang in {"rust", "python", "ts", "go", "c"}:
                totals[lang] = totals.get(lang, 0) + 1

        for m in MARKER.finditer(text):
            directive = m.group(1).split()[0] if m.group(1).split() else ""
            line = text[: m.start()].count("\n") + 1
            if directive not in KNOWN_DIRECTIVES:
                errors.append(
                    f"{rel}:{line}: unknown skill-check directive {directive!r} "
                    f"(known: {', '.join(sorted(KNOWN_DIRECTIVES))})"
                )
                continue
            # The fence must start on the very next line — a marker that has
            # drifted is a silent hole in the ratchet.
            rest = text[m.end():]
            fence = re.match(r"\n```(\w+)\n(.*?)^```", rest, re.S | re.M)
            if not fence:
                errors.append(
                    f"{rel}:{line}: orphaned skill-check marker — no fenced block "
                    f"on the next line"
                )
                continue
            lang = {"typescript": "ts"}.get(fence.group(1), fence.group(1))
            if lang != "rust":
                errors.append(
                    f"{rel}:{line}: skill-check marks a `{lang}` block; only "
                    f"`rust` is wired up so far — remove the marker or extend "
                    f"the checker rather than letting it pass silently"
                )
                continue
            marked.append((rel, lang, fence.group(2), line))
    return marked, totals, errors


def build_crate(marked):
    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / "src").mkdir(parents=True)

    preamble = PREAMBLE.read_text()
    mods, index = [], []
    for i, (rel, _lang, body, line) in enumerate(marked):
        # Everything is wrapped in a function body, unconditionally. Rust allows
        # items (struct/impl/fn) inside a function, so one rule covers all three
        # shapes — pure items, pure statements, and the mixed snippets that are
        # common in docs (declare a type, then use it). An earlier
        # "emit items as-is, wrap statements" heuristic split exactly wrong on
        # `nrpc.md`'s observer example, which does both.
        #
        # `use` is hoisted out: it is legal inside a function, but keeping it at
        # module level means an unused-import warning points at the snippet's own
        # line rather than the wrapper's.
        # Honour rustdoc's hidden-line convention: a line starting `# ` inside a
        # rust block is scaffolding the reader is not meant to see. The skills
        # use it (nrpc.md wraps an example in a hidden `async fn`), and since
        # these are plain markdown files it renders literally — worth fixing in
        # the prose separately, but the extractor must read it as Rust does.
        # `#[derive(...)]` is untouched: the rule is hash-space, not hash-bracket.
        body = "\n".join(
            re.sub(r"^#\s?", "", l) if re.match(r"^#(\s|$)", l) else l
            for l in body.splitlines()
        )

        # Hoist whole `use` statements, braces and all. A line-based split breaks
        # the multi-line form the skills use constantly:
        #     use net_sdk::capabilities::{
        #         p, evaluate_predicate,
        #     };
        # taking the first line and leaving the continuation behind, which
        # produces a parse error pointing at the wrapper rather than the snippet.
        uses, rest = [], body
        while True:
            m = re.search(r"^[ \t]*use [^;]*;[ \t]*$", rest, re.M | re.S)
            if not m:
                break
            uses.append(m.group(0).strip())
            rest = rest[: m.start()] + rest[m.end():]
        inner = (
            "\n".join(uses)
            + "\n#[allow(unused)]\nasync fn _snippet() -> Result<(), Box<dyn std::error::Error>> {\n"
            + rest
            + "\n    Ok(())\n}\n"
        )
        mods.append(
            f"// {rel}:{line}\n#[allow(unused_imports, dead_code, unused_variables)]\n"
            f"mod snippet_{i} {{\n{preamble}\n{inner}\n}}\n"
        )
        index.append((i, rel, line))

    # Record where each mod starts in the generated file so a cargo diagnostic
    # pointing at `src/lib.rs:N` can be reported against the skill file the
    # snippet actually came from. An error that names only the generated file
    # makes the reader do the mapping by hand.
    lib, offsets, cursor = [], [], 1
    for (i, rel, line), text in zip(index, mods):
        offsets.append((cursor, cursor + text.count("\n"), i, rel, line))
        lib.append(text)
        cursor += text.count("\n") + 1
    (OUT / "src/lib.rs").write_text("\n".join(lib))
    build_crate.offsets = offsets
    (OUT / "Cargo.toml").write_text(
        "[workspace]\n\n"
        "[package]\nname = \"skill-snippets\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n"
        "[dependencies]\n"
        f"net-sdk = {{ package = \"net-mesh-sdk\", path = \"{ROOT}/net/crates/net/sdk\", "
        "features = [\"net\", \"cortex\", \"redis\", \"macros\"] }\n"
        f"net = {{ package = \"net-mesh\", path = \"{ROOT}/net/crates/net\", features = [\"dataforts\"] }}\n"
        f"net-payments = {{ path = \"{ROOT}/net/crates/net/payments\", "
        "features = [\"mesh\", \"http-facilitator\", \"unsafe-dev-signer\"] }\n"
        "serde = { version = \"1\", features = [\"derive\"] }\n"
        "serde_json = \"1\"\n"
        "tokio = { version = \"1\", features = [\"rt\", \"macros\", \"time\"] }\n"
        "futures = \"0.3\"\n"
        # Third-party crates the snippets legitimately import. These are
        # the real crates, not shims — a reader following the snippet would
        # add exactly these to their own manifest.
        "bytes = \"1\"\n"
        "schemars = \"1\"\n"
        "prometheus = \"0.14\"\n"
    )
    return index


def main():
    verbose = "--verbose" in sys.argv
    marked, totals, errors = scan()

    for e in errors:
        print(e)

    total_rust = totals.get("rust", 0)
    if not marked:
        print(f"0/{total_rust} rust snippets marked for compilation")
        return 1 if errors else 0

    index = build_crate(marked)
    proc = subprocess.run(
        ["cargo", "check", "--quiet"], cwd=OUT, capture_output=True, text=True
    )

    if proc.returncode != 0:
        print(f"\nsnippet compilation FAILED ({len(marked)} marked):\n")
        out = proc.stderr
        # Blame only each error's PRIMARY span. Iterating every `--> src/lib.rs:N`
        # in the output also picks up `note:` and `help:` spans, which frequently
        # point into a *different* snippet (or into the SDK) — that over-blames
        # wildly: 8 errors reported as 28 failing snippets, which would make the
        # ratchet unmark snippets that compile fine.
        blamed, seen = [], set()
        for block in re.split(r"^(?=error(?:\[E\d+\])?: )", out, flags=re.M):
            if not block.startswith("error"):
                continue
            m = re.search(r"--> src/lib\.rs:(\d+):", block)
            if not m:
                continue
            gen_line = int(m.group(1))
            for start, end, i, rel, line in build_crate.offsets:
                if start <= gen_line <= end:
                    if i not in seen:
                        seen.add(i)
                        blamed.append((rel, line, i, gen_line))
                    break
        for rel, line, i, gen_line in blamed:
            print(f"  {rel}:{line}  (snippet {i}, generated line {gen_line})")
        blamed = seen
        if not blamed:
            for i, rel, line in index:
                if f"snippet_{i}" in out:
                    print(f"  from {rel}:{line}  (mod snippet_{i})")
        print("\n--- preamble in scope for every snippet ---")
        print("\n".join("  " + l for l in PREAMBLE.read_text().splitlines()))
        print("\n--- cargo ---")
        print("\n".join("  " + l for l in out.splitlines()[:60]))
        return 1

    pct = 100 * len(marked) // total_rust if total_rust else 0
    print(f"rust snippets: {len(marked)}/{total_rust} marked and compiling ({pct}%)")
    for lang, n in sorted(totals.items()):
        if lang != "rust":
            print(f"  {lang}: 0/{n} — not wired up")
    if verbose:
        for _i, rel, line in index:
            print(f"    ok  {rel}:{line}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
