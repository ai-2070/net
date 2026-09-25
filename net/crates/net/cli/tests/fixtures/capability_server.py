# SPDX-License-Identifier: MIT OR Apache-2.0
"""Recorded MCP echo: invocation-log path and local harness control address."""
import json
import os
import socket
import sys
import threading

record_path, control_addr = sys.argv[1:]
host, port = control_addr.rsplit(":", 1)
control = socket.create_connection((host, int(port)), timeout=10)
control.settimeout(None)


def stop():
    control.recv(1)
    os._exit(0)


threading.Thread(target=stop, daemon=True).start()
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "journey", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "journey_echo", "description": "Recorded echo",
                            "inputSchema": {"type": "object", "properties": {
                                "message": {"type": "string"}}, "required": ["message"]}}]}
    elif method == "tools/call":
        assert request["params"]["name"] == "journey_echo"
        record = request["params"]["arguments"]
        with open(record_path, "a", encoding="utf-8") as output:
            output.write(json.dumps(record) + "\n")
            output.flush()
        result = {"content": [{"type": "text", "text": json.dumps(record)}], "isError": False}
    else:
        raise ValueError(f"unexpected method: {method}")
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
