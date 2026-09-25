# SPDX-License-Identifier: MIT OR Apache-2.0
"""Independent byte-layout check, not native-mesh interoperability."""
import json
import struct
from pathlib import Path


def test_shared_unary_response_fragment_layout():
    fixture = json.loads((Path(__file__).resolve().parents[3] / "tests" /
                          "cross_lang_nrpc" / "golden_vectors_large_response.json").read_text())
    assert fixture["request_flag"] == 1 << 6
    assert fixture["header_name"] == "nrpc-response-fragment-v1"
    assert fixture["max_response_bytes"] // fixture["chunk_bytes"] == fixture["max_fragments"]
    response = fixture["response"]
    encoded = struct.pack("<HBI", response["status"], 0, response["body_length"])
    encoded += bytes([response["body_byte"]]) * response["body_length"]
    prefix = bytes.fromhex(fixture["encoded_prefix_hex"])
    assert encoded[:len(prefix)].hex() == fixture["encoded_prefix_hex"], \
        "response byte layout drifted: encoded prefix != fixture encoded_prefix_hex"
    assert len(encoded) == fixture["encoded_response_bytes"]
    chunk = fixture["chunk_bytes"]
    assert len(fixture["fragments"]) == (len(encoded) + chunk - 1) // chunk
    assembled = bytearray()
    for piece in fixture["fragments"]:
        assert struct.pack("<IH", len(encoded), piece["index"]).hex() == piece["header_value_hex"]
        body = encoded[piece["index"] * chunk:(piece["index"] + 1) * chunk]
        assert len(body) == piece["body_bytes"]
        assembled.extend(body)
    assert assembled == encoded
