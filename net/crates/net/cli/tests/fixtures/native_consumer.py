# SPDX-License-Identifier: MIT OR Apache-2.0
"""Exercise generated models/helper through a real Rust SDK mesh-call adapter.

The adapter address is supplied by the native_contract_workflow harness.
This is not the Python SDK binding and does not itself create a mesh node.
"""
import asyncio
import json
import sys
from pydantic import ValidationError

sys.path.insert(0, sys.argv[1])
from generated.native_echo import NativeEchoRequest, call_native_echo


class MeshAdapter:
    async def call(self, tool_id, input):
        host, port = sys.argv[2].rsplit(":", 1)
        reader, writer = await asyncio.open_connection(host, int(port))
        try:
            writer.write((json.dumps({"tool_id": tool_id, "input": input}) + "\n").encode())
            await writer.drain()
            response = json.loads(await asyncio.wait_for(reader.readline(), 10))
            if "error" in response:
                raise RuntimeError(response["error"])
            return response["result"]
        finally:
            writer.close()
            await writer.wait_closed()


async def main():
    try:
        NativeEchoRequest(message=[])
    except ValidationError:
        pass
    else:
        raise AssertionError("generated request accepted an invalid message")
    result = await call_native_echo(MeshAdapter(), NativeEchoRequest(message="generated consumer"))
    assert result.message == "generated consumer"
    assert result.provider_node_id == sys.argv[3]
    print(result.model_dump_json())


asyncio.run(main())
