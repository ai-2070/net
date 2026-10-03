"""Cross-language BlobRef describe fixture: Python side.

Loads the shared ``tests/cross_lang_blob/describe_vectors.json``. The refs in
it were produced by the Go binding (``go/blob_describe_vectors_test.go``),
which also checks its own decoding against the same ``expected`` values. Here
each ref must decode through ``BlobRef.from_encoded`` to the same normalized
description, and re-encode to the identical bytes.

Normalized form: ``hash`` only for small refs, ``tree_root_hash`` and
``tree_depth`` only for tree refs (Python's getters return zeros otherwise,
so they are read by shape), hashes as lowercase hex. ``encoding`` is in the
fixture for Go; Python has no encoding getter, so it is not compared here.

Plan: docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, S6.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

net = pytest.importorskip("net")
if not hasattr(net, "BlobRef"):
    pytest.skip("net was built without `dataforts`; BlobRef unavailable", allow_module_level=True)

from net import BlobRef  # noqa: E402

_FIXTURE_PATH = (
    Path(__file__).resolve().parents[3]
    / "tests"
    / "cross_lang_blob"
    / "describe_vectors.json"
)
with _FIXTURE_PATH.open("r", encoding="utf-8") as _fp:
    FIXTURE: dict[str, Any] = json.load(_fp)

VECTORS = FIXTURE["vectors"]


def _normalize(ref: BlobRef) -> dict[str, Any]:
    out: dict[str, Any] = {
        "version": ref.version,
        "uri": ref.uri,
        "size": ref.size,
        "is_tree": ref.is_tree,
        "is_chunked": ref.is_chunked,
    }
    if not ref.is_chunked:
        out["hash"] = bytes(ref.hash).hex()
    if ref.is_tree:
        out["tree_root_hash"] = bytes(ref.tree_root_hash).hex()
        out["tree_depth"] = ref.tree_depth
    return out


def test_fixture_covers_small_and_tree_refs() -> None:
    assert len(VECTORS) >= 4
    shapes = {v["expected"]["is_tree"] for v in VECTORS}
    assert shapes == {True, False}


@pytest.mark.parametrize("vector", VECTORS, ids=[v["name"] for v in VECTORS])
def test_python_decodes_go_refs_identically(vector: dict[str, Any]) -> None:
    encoded = bytes.fromhex(vector["encoded_hex"])
    ref = BlobRef.from_encoded(encoded)
    assert ref is not None, "a Go-encoded ref did not decode as a BlobRef"

    expected = {k: v for k, v in vector["expected"].items() if k != "encoding"}
    assert _normalize(ref) == expected


@pytest.mark.parametrize("vector", VECTORS, ids=[v["name"] for v in VECTORS])
def test_python_reencodes_go_refs_byte_for_byte(vector: dict[str, Any]) -> None:
    encoded = bytes.fromhex(vector["encoded_hex"])
    ref = BlobRef.from_encoded(encoded)
    assert ref is not None
    assert bytes(ref.encode()) == encoded
