# Hermes Phase 0.5 gate harness

Probes for `HERMES_INTEGRATION_PLAN.md` Phase 0.5: verify that Hermes (a
stock install, config only) can consume `net-mesh mcp serve`.

## Recipe

Build the binaries once (`cargo build -p net-cli -p net-aggregator-daemon`,
run from `net/crates/net`), generate an operator identity if you don't have
one (`net-mesh identity generate --out <path>`), then:

```sh
# 1. Start the gate daemon; copy the bootstrap JSON line it prints.
target/debug/net-aggregator-daemon --config tools/hermes-gate/daemon.toml --print-bootstrap

# 2. Export the gate environment (values from the bootstrap line):
export NET_GATE_BOOTSTRAP='{"bound_addr":"127.0.0.1:PORT","node_id":N,"public_key_hex":"..."}'
export NET_GATE_IDENTITY="path/to/operator.toml"
# optional overrides: NET_MESH_BIN (shim binary), NET_GATE_PSK (defaults to daemon.toml's)

# 3. SDK-client probe — Hermes's exact MCP client (mcp package) vs the shim.
#    Run with the hermes-agent venv python (needs `uv sync --extra mcp` there).
<hermes-agent>/.venv/Scripts/python tools/hermes-gate/probe_client.py

# 4. Hermes-registration probe — Hermes's real register_mcp_servers() path.
#    Must run FROM the hermes-agent repo root (top-level module imports).
cd <hermes-agent> && .venv/Scripts/python <this-repo>/tools/hermes-gate/probe_hermes_register.py
```

Pass criteria are printed by each probe (`GATE PASS` / `TASK3 PASS`).

Probe 1 proves protocol-version negotiation (the shim's version adapter,
`COMPAT_PROTOCOL_VERSIONS`) and the meta-tool list. Probe 2 proves the full
Hermes production path: suspicious-config filter, env-filtered stdio spawn,
tool discovery, description security scan, registry entry (`mcp_net_net_*`
names), and a live `registry.dispatch` call through the shim to the mesh.
