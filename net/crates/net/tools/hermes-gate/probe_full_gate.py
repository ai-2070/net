"""Phase 0.5 gate probe 3: full loop — wrapped capability invoked from Hermes.

Prerequisites: the gate daemon is up AND `net-mesh wrap fixture -- net-mcp-fixture`
is running under the SAME operator identity (owner-scope admits the shim).

Drives Hermes's registry dispatch through the shim: search finds the wrapped
`echo`, describe returns its schema, invoke round-trips a message. This is
the Phase 0.5 acceptance shape on one machine (two mesh attachments).

Run FROM the hermes-agent repo root with its venv python. Environment:
NET_GATE_BOOTSTRAP, NET_GATE_IDENTITY, optional NET_MESH_BIN / NET_GATE_PSK.
"""

import json
import logging
import os
import re
import sys
import time
from pathlib import Path

logging.basicConfig(level=logging.WARNING)

DEFAULT_PSK = "4242424242424242424242424242424242424242424242424242424242424242"
REPO_NET = Path(__file__).resolve().parents[2]  # net/crates/net

MESSAGE = "hermes-gate-full-loop"


def shim_config() -> dict:
    bootstrap = json.loads(os.environ["NET_GATE_BOOTSTRAP"])
    bin_ = os.environ.get("NET_MESH_BIN") or str(REPO_NET / "target" / "debug" / "net-mesh.exe")
    return {
        "net": {
            "command": bin_,
            "args": [
                "mcp", "serve",
                "--identity", os.environ["NET_GATE_IDENTITY"],
                "--node-addr", bootstrap["bound_addr"],
                "--node-pubkey", bootstrap["public_key_hex"],
                "--node-id", str(bootstrap["node_id"]),
                "--psk-hex", os.environ.get("NET_GATE_PSK", DEFAULT_PSK),
                "--quiet",
            ],
        }
    }


def find_echo_cap_id(search_result: str) -> str:
    """Pull the wrapped echo capability id out of the search result text."""
    ids = re.findall(r"[\w.-]+/[\w.-]*echo[\w.-]*", search_result)
    if not ids:
        raise SystemExit(f"no echo capability id in search result: {search_result[:400]}")
    return ids[0]


def main() -> int:
    from tools.mcp_tool import register_mcp_servers, shutdown_mcp_servers
    from tools.registry import registry

    try:
        register_mcp_servers(shim_config())

        # A freshly-joined shim node can lag the wrap node's announcement;
        # the bridge's own live tests retry search for the same reason.
        search = ""
        for attempt in range(8):
            search = registry.dispatch("mcp_net_net_search_capabilities", {"query": "echo"})
            if "No remote capabilities found" not in str(search):
                break
            time.sleep(2)
            print(f"search attempt {attempt + 1}: empty index, retrying")
        print("search:", str(search)[:400])
        cap_id = find_echo_cap_id(str(search))
        print("cap_id:", cap_id)

        describe = registry.dispatch("mcp_net_net_describe_capability", {"cap_id": cap_id})
        print("describe:", str(describe)[:400])
        if "message" not in str(describe):
            print("describe did not surface the echo input schema")
            return 1

        invoke = registry.dispatch(
            "mcp_net_net_invoke_capability",
            {"cap_id": cap_id, "arguments": {"message": MESSAGE}},
        )
        print("invoke:", str(invoke)[:400])
        if MESSAGE not in str(invoke):
            print("invoke did not round-trip the message")
            return 1

        print("TASK4 PASS")
        return 0
    finally:
        shutdown_mcp_servers()


sys.exit(main())
