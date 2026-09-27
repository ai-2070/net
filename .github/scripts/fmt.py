#!/usr/bin/env python3
"""`cargo fmt --all`, in batches — for Windows.

    python .github/scripts/fmt.py            # format the workspace
    python .github/scripts/fmt.py --check    # what `cargo fmt --all -- --check` checks

Run from `net/crates/net` (the workspace root) or pass `--manifest-path`.

Why this exists: `cargo fmt --all` hands every target root of a package
(`lib.rs`, each `tests/*.rs`, benches, examples) to ONE rustfmt command.
The root crate has well over a hundred integration-test targets, and on
Windows that command line exceeds the OS limit — `cargo fmt` fails with
"The filename or extension is too long. (os error 206)" before checking
anything. This asks `cargo metadata` for the same target roots and runs
rustfmt over them in batches, per package edition, so the result is what
`cargo fmt --all` would do. rustfmt follows each root's modules itself and
reads `rustfmt.toml` from the file's directory upward, as cargo fmt does.

On Linux and macOS `cargo fmt --all` works and is what CI runs; this is the
same check where it does not.
"""

import argparse
import json
import subprocess
import sys

BATCH = 40


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--check", action="store_true", help="report, do not rewrite")
    parser.add_argument("--manifest-path", default=None)
    args = parser.parse_args()

    command = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
    if args.manifest_path:
        command += ["--manifest-path", args.manifest_path]
    metadata = json.loads(subprocess.run(command, check=True, capture_output=True, text=True).stdout)
    members = set(metadata["workspace_members"])

    by_edition: dict[str, list[str]] = {}
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        for target in package["targets"]:
            # `cargo fmt` skips build scripts; so does this.
            if "custom-build" in target["kind"]:
                continue
            by_edition.setdefault(package["edition"], []).append(target["src_path"])

    failed = False
    for edition, files in sorted(by_edition.items()):
        files = sorted(set(files))
        for start in range(0, len(files), BATCH):
            batch = files[start : start + BATCH]
            rustfmt = ["rustfmt", "--edition", edition] + (["--check"] if args.check else []) + batch
            if subprocess.run(rustfmt).returncode != 0:
                failed = True
    if failed:
        print("fmt.py: rustfmt reported differences" if args.check else "fmt.py: rustfmt failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
