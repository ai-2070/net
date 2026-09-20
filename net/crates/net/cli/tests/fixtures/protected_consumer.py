# SPDX-License-Identifier: MIT OR Apache-2.0
"""Generated protected client over the harness's real Rust org-call adapter.

No credentials cross this socket. The harness runs one uncredentialed attempt,
then one credentialed attempt; this is not the native Python SDK binding.
"""
import asyncio
import json
import sys
from pydantic import ValidationError

sys.path.insert(0, sys.argv[1])
from generated.protected_echo import ProtectedEchoRequest, call_protected_echo


class AdmissionDenied(Exception):
    pass


class MeshAdapter:
    def __init__(self):
        self.calls = 0

    async def call(self, tool_id, input):
        self.calls += 1
        host, port = sys.argv[2].rsplit(":", 1)
        reader, writer = await asyncio.open_connection(host, int(port))
        try:
            writer.write((json.dumps({"tool_id": tool_id, "input": input}) + "\n").encode())
            await writer.drain()
            reply = json.loads(await asyncio.wait_for(reader.readline(), 10))
            if "error" in reply:
                assert reply["error"]["status"] == 9
                raise AdmissionDenied()
            return reply["result"]
        finally:
            writer.close()
            await writer.wait_closed()


async def main():
    mesh = MeshAdapter()
    try:
        ProtectedEchoRequest(text=[])
    except ValidationError:
        pass
    else:
        raise AssertionError("invalid generated request accepted")
    assert mesh.calls == 0
    request = ProtectedEchoRequest(text="generated protected consumer")
    try:
        await call_protected_echo(mesh, request)
    except AdmissionDenied:
        pass
    else:
        raise AssertionError("uncredentialed generated call succeeded")
    assert mesh.calls == 1, "denied helper must not retry"
    response = await call_protected_echo(mesh, request)
    assert mesh.calls == 2, "authorized helper must invoke exactly once"
    assert response.text == request.text
    print(response.model_dump_json())


asyncio.run(main())
