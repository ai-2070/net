"""Diagnostic: capability-tag propagation daemon hub topology.

Holds ONE shim session open and answers two questions:
  Phase A: does an announcement made BEFORE we attached ever reach us? (60s poll)
  Phase B: does a FRESH announcement made while attached reach us? (30s poll)

Requires: gate daemon up; an existing `net-mesh wrap fixture` running (phase A
target). Spawns its own second wrap for phase B.

Run with the hermes-agent venv python (mcp SDK). Env: NET_GATE_BOOTSTRAP,
NET_GATE_IDENTITY, optional NET_MESH_BIN / NET_GATE_PSK.
"""

import asyncio
import json
import os
import subprocess
import sys
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

DEFAULT_PSK = "4242424242424242424242424242424242424242424242424242424242424242"
REPO_NET = Path(__file__).resolve().parents[2]


def attach_args() -> list[str]:
    b = json.loads(os.environ["NET_GATE_BOOTSTRAP"])
    return [
        "--identity", os.environ["NET_GATE_IDENTITY"],
        "--node-addr", b["bound_addr"],
        "--node-pubkey", b["public_key_hex"],
        "--node-id", str(b["node_id"]),
        "--psk-hex", os.environ.get("NET_GATE_PSK", DEFAULT_PSK),
    ]


BIN = os.environ.get("NET_MESH_BIN") or str(REPO_NET / "target" / "debug" / "net-mesh.exe")


async def poll_search(session: ClientSession, label: str, seconds: int) -> bool:
    for i in range(seconds // 3):
        result = await session.call_tool("net_search_capabilities", {"query": "echo"})
        text = "".join(getattr(c, "text", "") for c in result.content)
        if "No remote capabilities found" not in text:
            print(f"{label}: FOUND after {i * 3}s: {text[:200]}")
            return True
        await asyncio.sleep(3)
    print(f"{label}: not found within {seconds}s")
    return False


async def main() -> int:
    params = StdioServerParameters(command=BIN, args=["mcp", "serve", *attach_args(), "--quiet"])
    async with stdio_client(params, errlog=sys.stderr) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            a = await poll_search(session, "phase A (pre-existing announce)", 60)

            wrap = subprocess.Popen(
                [BIN, "wrap", "fixture2", *attach_args(), "--",
                 str(REPO_NET / "target" / "debug" / "net-mcp-fixture.exe")],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                b = await poll_search(session, "phase B (fresh announce while attached)", 30)
            finally:
                wrap.terminate()

            print(f"VERDICT: pre-existing={'seen' if a else 'NOT seen'}, "
                  f"fresh={'seen' if b else 'NOT seen'}")
            return 0


sys.exit(asyncio.run(main()))
