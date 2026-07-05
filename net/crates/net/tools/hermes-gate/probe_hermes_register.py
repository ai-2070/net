"""Phase 0.5 gate probe 2: Hermes's REAL registration path vs the shim.

Drives tools.mcp_tool.register_mcp_servers() — the exact function Hermes
bootstrap calls with the config.yaml `mcp_servers` mapping — then inspects
Hermes's live tool registry and dispatches a real call through the shim.

Must run FROM the hermes-agent repo root with its venv python (top-level
module imports). Environment: NET_GATE_BOOTSTRAP, NET_GATE_IDENTITY,
optional NET_MESH_BIN / NET_GATE_PSK (see README.md).
"""

import json
import logging
import os
import sys
from pathlib import Path

logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")

DEFAULT_PSK = "4242424242424242424242424242424242424242424242424242424242424242"
REPO_NET = Path(__file__).resolve().parents[2]  # net/crates/net

EXPECTED = {
    "mcp_net_net_search_capabilities",
    "mcp_net_net_describe_capability",
    "mcp_net_net_invoke_capability",
    "mcp_net_net_list_pinned_capabilities",
    "mcp_net_net_request_pin",
}


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


def main() -> int:
    from tools.mcp_tool import register_mcp_servers, shutdown_mcp_servers
    from tools.registry import registry

    try:
        names = register_mcp_servers(shim_config())
        print("registered:", sorted(names))

        present = {n for n in registry.get_all_tool_names() if n.startswith("mcp_net_")}
        missing = EXPECTED - present
        if missing:
            print("MISSING:", sorted(missing))
            return 1

        # get_schema returns the full function-call definition:
        # {name, description, parameters: {properties, required, ...}}.
        schema = registry.get_schema("mcp_net_net_search_capabilities")
        toolset = registry.get_toolset_for_tool("mcp_net_net_search_capabilities")
        params = schema.get("parameters", {}) if isinstance(schema, dict) else {}
        props = sorted(params.get("properties", {}))
        print(f"sample entry: toolset={toolset} schema_properties={props}")
        if "query" not in props:
            print("SCHEMA LOST the 'query' property through Hermes registration")
            return 1

        # Dispatch a real call through Hermes's pipeline -> shim -> mesh.
        # Nothing is wrapped on this mesh yet, so the canonical empty-index
        # product string proves the full invoke path.
        result = registry.dispatch("mcp_net_net_search_capabilities", {"query": "github"})
        print("dispatch result:", str(result)[:300])
        if "No remote capabilities found" not in str(result):
            print("UNEXPECTED dispatch result (expected the empty-index product string)")
            return 1

        print("TASK3 PASS")
        return 0
    finally:
        shutdown_mcp_servers()


sys.exit(main())
