"""Phase 0.5 gate probe 1: Hermes's exact MCP SDK client vs `net-mesh mcp serve`.

Runs the same ClientSession.initialize() that raised `Unsupported protocol
version` before the shim's version adapter. Pass = handshake + the 5
meta-tools listed.

Environment (see README.md): NET_GATE_BOOTSTRAP, NET_GATE_IDENTITY,
optional NET_MESH_BIN / NET_GATE_PSK. Run with the hermes-agent venv python.
"""

import asyncio
import json
import os
import sys
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

DEFAULT_PSK = "4242424242424242424242424242424242424242424242424242424242424242"
REPO_NET = Path(__file__).resolve().parents[2]  # net/crates/net

EXPECTED = {
    "net_search_capabilities",
    "net_describe_capability",
    "net_invoke_capability",
    "net_list_pinned_capabilities",
    "net_request_pin",
}


def shim_args() -> tuple[str, list[str]]:
    bootstrap = json.loads(os.environ["NET_GATE_BOOTSTRAP"])
    bin_ = os.environ.get("NET_MESH_BIN") or str(REPO_NET / "target" / "debug" / "net-mesh.exe")
    args = [
        "mcp", "serve",
        "--identity", os.environ["NET_GATE_IDENTITY"],
        "--node-addr", bootstrap["bound_addr"],
        "--node-pubkey", bootstrap["public_key_hex"],
        "--node-id", str(bootstrap["node_id"]),
        "--psk-hex", os.environ.get("NET_GATE_PSK", DEFAULT_PSK),
        "--quiet",
    ]
    return bin_, args


async def main() -> int:
    command, args = shim_args()
    params = StdioServerParameters(command=command, args=args)
    async with stdio_client(params, errlog=sys.stderr) as (read, write):
        async with ClientSession(read, write) as session:
            result = await session.initialize()
            print(f"initialize OK: protocolVersion={result.protocolVersion}")
            print(f"serverInfo={result.serverInfo.name} {result.serverInfo.version}")
            tools = await session.list_tools()
            names = [t.name for t in tools.tools]
            print(f"tools ({len(names)}): {names}")
            missing = EXPECTED - set(names)
            if missing:
                print(f"MISSING META-TOOLS: {missing}")
                return 1
            print("GATE PASS")
            return 0


sys.exit(asyncio.run(main()))
