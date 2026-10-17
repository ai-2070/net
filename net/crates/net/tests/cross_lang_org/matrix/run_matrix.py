#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Run the cross-process, cross-language org matrix (see PROTOCOL.md).

Every caller language against every provider language, each pair as two OS
processes over a real mesh, one cross-org Granted call per shape. Each run gets
a fresh scenario from the `gen_org_scenario` example.

    python run_matrix.py \
        --gen <path to gen_org_scenario binary> \
        --driver rust=<org_matrix_peer binary> \
        --driver go=<org-matrix-peer binary> \
        --driver "python=<python> <.../driver.py>" \
        --driver "node=node <.../driver.mjs>" \
        [--langs rust,go,python,node] [--shapes unary,server-streaming] \
        [--report matrix.json] [--logs <dir>]

A `--driver` value is split shell-style into the command prefix. Exits 0 only
when every requested cell passed.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shlex
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from typing import Optional

SHAPES = ("unary", "server-streaming")
STEP_TIMEOUT = 30.0  # one protocol line
CALL_TIMEOUT = 150.0  # the caller's RESULT (discovery can take a while)


class CellFailure(Exception):
    pass


@dataclass
class Peer:
    name: str
    proc: subprocess.Popen
    lines: "queue.Queue[Optional[str]]" = field(default_factory=queue.Queue)

    def start_reader(self) -> None:
        def pump() -> None:
            assert self.proc.stdout is not None
            for raw in self.proc.stdout:
                self.lines.put(raw.rstrip("\r\n"))
            self.lines.put(None)

        threading.Thread(target=pump, daemon=True).start()

    def expect(self, prefix: str, timeout: float) -> str:
        """The next stdout line, which must start with `prefix`."""
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                raise CellFailure(f"{self.name}: no {prefix!r} line within {timeout:.0f}s")
            try:
                line = self.lines.get(timeout=left)
            except queue.Empty:
                raise CellFailure(f"{self.name}: no {prefix!r} line within {timeout:.0f}s")
            if line is None:
                raise CellFailure(
                    f"{self.name}: exited (code {self.proc.poll()}) before {prefix!r}"
                )
            if line.startswith(prefix):
                return line
            raise CellFailure(f"{self.name}: expected {prefix!r}, got {line!r}")

    def send(self, text: str) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(text + "\n")
        self.proc.stdin.flush()

    def finish(self, timeout: float = 30.0) -> int:
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            raise CellFailure(f"{self.name}: did not exit within {timeout:.0f}s")

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass


def spawn(name: str, cmd: list, log_path: str) -> Peer:
    log = open(log_path, "w", encoding="utf-8")
    try:
        proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
    except OSError as e:
        raise CellFailure(f"{name}: could not start {cmd[0]!r}: {e}")
    peer = Peer(name, proc)
    peer.start_reader()
    return peer


def gen_scenario(gen: list, root: str) -> str:
    out = tempfile.mkdtemp(prefix="scenario-", dir=root)
    try:
        done = subprocess.run(gen + [out], capture_output=True, text=True, timeout=120)
    except OSError as e:
        raise CellFailure(f"could not start {gen[0]!r}: {e}")
    if done.returncode != 0:
        raise CellFailure(f"gen_org_scenario failed: {done.stderr.strip()[-400:]}")
    return out


def run_cell(
    drivers: dict, gen: list, caller_lang: str, provider_lang: str, shape: str, logs: str
) -> None:
    scenario = gen_scenario(gen, logs)
    tag = f"{caller_lang}-to-{provider_lang}-{shape}"
    caller = spawn(
        f"{caller_lang} caller",
        drivers[caller_lang] + ["caller", "--manifest", scenario, "--shape", shape],
        os.path.join(logs, f"{tag}.caller.log"),
    )
    provider: Optional[Peer] = None
    try:
        node_id = caller.expect("ID ", STEP_TIMEOUT).split()[1]
        provider = spawn(
            f"{provider_lang} provider",
            drivers[provider_lang]
            + [
                "provider",
                "--manifest",
                scenario,
                "--shape",
                shape,
                "--caller-node-id",
                node_id,
            ],
            os.path.join(logs, f"{tag}.provider.log"),
        )
        ready = provider.expect("READY ", STEP_TIMEOUT).split()
        if len(ready) != 4:
            raise CellFailure(f"malformed READY line: {' '.join(ready)!r}")
        caller.send("PROVIDER " + " ".join(ready[1:]))
        result = caller.expect("RESULT ", CALL_TIMEOUT)
        if result != "RESULT ok":
            raise CellFailure(f"caller: {result}")
        facts = provider.expect("FACTS ", STEP_TIMEOUT)
        if facts != "FACTS ok":
            raise CellFailure(f"provider: {facts}")
        provider.send("DRAINED")
        presult = provider.expect("RESULT ", STEP_TIMEOUT)
        if presult != "RESULT ok calls=1":
            raise CellFailure(f"provider: {presult}")
        for peer in (caller, provider):
            code = peer.finish()
            if code != 0:
                raise CellFailure(f"{peer.name} exited {code}")
    finally:
        caller.kill()
        if provider is not None:
            provider.kill()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--gen", required=True, help="gen_org_scenario command")
    ap.add_argument("--driver", action="append", default=[], help="LANG=COMMAND")
    ap.add_argument("--langs", default="rust,go,python,node")
    ap.add_argument("--shapes", default=",".join(SHAPES))
    ap.add_argument("--report", help="write a JSON report here")
    ap.add_argument("--logs", help="keep per-cell logs here (default: a temp dir)")
    args = ap.parse_args()

    drivers = {}
    for spec in args.driver:
        lang, _, cmd = spec.partition("=")
        if not cmd:
            ap.error(f"--driver {spec!r} must be LANG=COMMAND")
        drivers[lang] = shlex.split(cmd, posix=os.name != "nt") if os.name != "nt" else cmd.split()
    langs = [l for l in args.langs.split(",") if l]
    shapes = [s for s in args.shapes.split(",") if s]
    missing = [l for l in langs if l not in drivers]
    if missing:
        ap.error(f"no --driver for: {', '.join(missing)}")
    for s in shapes:
        if s not in SHAPES:
            ap.error(f"unknown shape {s}")
    logs = args.logs or tempfile.mkdtemp(prefix="org-matrix-")
    os.makedirs(logs, exist_ok=True)
    gen = shlex.split(args.gen, posix=os.name != "nt") if os.name != "nt" else args.gen.split()

    results = {}
    for shape in shapes:
        for caller_lang in langs:
            for provider_lang in langs:
                key = f"{caller_lang}->{provider_lang} {shape}"
                started = time.monotonic()
                try:
                    run_cell(drivers, gen, caller_lang, provider_lang, shape, logs)
                    results[key] = {"ok": True}
                    verdict = "ok"
                except CellFailure as e:
                    results[key] = {"ok": False, "detail": str(e)}
                    verdict = f"FAIL {e}"
                took = time.monotonic() - started
                results[key]["secs"] = round(took, 1)
                print(f"{key:<36} {verdict} ({took:.1f}s)", flush=True)

    print()
    for shape in shapes:
        print(f"{shape}: caller (rows) x provider (columns)")
        print(" " * 8 + "".join(f"{p:>8}" for p in langs))
        for c in langs:
            row = "".join(
                f"{('ok' if results[f'{c}->{p} {shape}']['ok'] else 'FAIL'):>8}" for p in langs
            )
            print(f"{c:>8}{row}")
        print()
    failed = [k for k, v in results.items() if not v["ok"]]
    if args.report:
        with open(args.report, "w", encoding="utf-8") as f:
            json.dump({"cells": results, "failed": failed}, f, indent=2)
    print(f"logs: {logs}")
    if failed:
        print(f"{len(failed)} of {len(results)} cells failed", file=sys.stderr)
        return 1
    print(f"all {len(results)} cells passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
