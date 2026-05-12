"""Tests for the Dataforts blob surface — `BlobRef`,
`MeshBlobAdapter`, and the v0.15 external-hook adapter helpers
(`register_filesystem_blob_adapter` + `blob_publish` /
`blob_resolve`).

The native module must be built with the `dataforts` Cargo
feature for these tests to run; the wheel that ships from
`maturin develop --features dataforts` carries them in. Without
the feature, `import net` would still succeed but the symbols
this file references wouldn't exist — the import would raise at
the first failing name, which we surface as a clean skip below.
"""

from __future__ import annotations

import hashlib
import tempfile
from pathlib import Path

import pytest

# Skip the entire module when the binding was built without the
# `dataforts` feature — the v0.2 substrate-owned surface
# (`MeshBlobAdapter` specifically) doesn't exist in that build.
net = pytest.importorskip("net")
if not hasattr(net, "MeshBlobAdapter"):
    pytest.skip(
        "net was built without `dataforts`; MeshBlobAdapter unavailable",
        allow_module_level=True,
    )

from net import BlobError, BlobRef, MeshBlobAdapter, Redex


def _blake3_digest(payload: bytes) -> bytes:
    """BLAKE3-256 of `payload`. The substrate uses BLAKE3 for
    every content-address; we recompute it here so the
    `BlobRef.hash` field is correct for the round-trip path.
    Falls back to a `blake3` PyPI shim when the stdlib doesn't
    ship the algorithm (Python 3.10–3.13 mostly).
    """
    try:
        return hashlib.blake3(payload).digest()  # type: ignore[attr-defined]
    except AttributeError:
        try:
            import blake3 as blake3_mod  # type: ignore
        except ImportError:
            pytest.skip("blake3 not available (try `pip install blake3`)")
        return blake3_mod.blake3(payload).digest()


# ============================================================================
# BlobRef
# ============================================================================


def test_blob_ref_round_trips_through_encode_and_from_encoded() -> None:
    payload = b"round-trip me"
    hash_bytes = _blake3_digest(payload)
    original = BlobRef("mesh://demo", hash_bytes, len(payload))
    encoded = original.encode()
    decoded = BlobRef.from_encoded(encoded)
    assert decoded is not None
    assert decoded == original
    assert decoded.uri == "mesh://demo"
    assert decoded.hash == hash_bytes
    assert decoded.size == len(payload)


def test_blob_ref_from_encoded_returns_none_for_inline_bytes() -> None:
    # An arbitrary inline payload doesn't carry the discriminator
    # magic — `from_encoded` returns `None` rather than raising.
    assert BlobRef.from_encoded(b"just inline event bytes") is None


def test_blob_ref_rejects_hash_of_wrong_length() -> None:
    with pytest.raises(ValueError):
        BlobRef("mesh://x", b"\x00" * 16, 0)


# ============================================================================
# MeshBlobAdapter — v0.2 substrate-owned CAS
# ============================================================================


def _adapter(redex: Redex, tag: str = "py-test") -> MeshBlobAdapter:
    """Construct an in-memory adapter (no `persistent=True`) so
    each test runs against a fresh, ephemeral Redex without
    needing a temp-dir setup. Tests that need cross-invocation
    state opt into `persistent=True` explicitly with a
    `tempfile.TemporaryDirectory`.
    """
    return MeshBlobAdapter(redex, tag)


def _payload_and_ref(body: bytes = b"hello", uri: str = "mesh://demo") -> tuple[bytes, BlobRef]:
    """Build a matching `(payload, BlobRef)` pair. The substrate
    verifies `blake3(payload) == blob_ref.hash` on every store,
    so the two have to agree. `body` first so tests can call
    `_payload_and_ref(b"my bytes")` ergonomically."""
    digest = _blake3_digest(body)
    return body, BlobRef(uri, digest, len(body))


def test_mesh_blob_adapter_store_then_fetch_round_trips_bytes() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    payload, ref = _payload_and_ref(b"net.MeshBlobAdapter round-trip")
    adapter.store(ref, payload)
    fetched = adapter.fetch(ref)
    assert fetched == payload


def test_mesh_blob_adapter_exists_reports_local_presence() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    payload, ref = _payload_and_ref(b"presence probe")
    assert adapter.exists(ref) is False
    adapter.store(ref, payload)
    assert adapter.exists(ref) is True


def test_mesh_blob_adapter_fetch_missing_raises_blob_error() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    # Never stored — substrate's local fetch raises NotFound,
    # surfaced as `BlobError` in Python.
    digest = _blake3_digest(b"nothing")
    ref = BlobRef("mesh://ghost", digest, 7)
    with pytest.raises(BlobError):
        adapter.fetch(ref)


def test_mesh_blob_adapter_store_rejects_hash_mismatch() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    advertised = _blake3_digest(b"truth")
    # BlobRef advertises the hash of `"truth"`, but we try to
    # store `"a lie"`. Substrate-side verify rejects.
    lying_ref = BlobRef("mesh://tamper", advertised, len(b"a lie"))
    with pytest.raises(BlobError):
        adapter.store(lying_ref, b"a lie")


def test_mesh_blob_adapter_fetch_range_returns_inner_slice() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    payload, ref = _payload_and_ref(b"0123456789abcdef")
    adapter.store(ref, payload)
    chunk = adapter.fetch_range(ref, 4, 10)  # half-open [4, 10)
    assert chunk == payload[4:10]


def test_mesh_blob_adapter_idempotent_store_of_identical_bytes() -> None:
    redex = Redex()
    adapter = _adapter(redex)
    payload, ref = _payload_and_ref(b"idempotent")
    adapter.store(ref, payload)
    adapter.store(ref, payload)  # second store of identical content — no-op
    fetched = adapter.fetch(ref)
    assert fetched == payload


def test_mesh_blob_adapter_prometheus_text_emits_dataforts_blob_metrics() -> None:
    redex = Redex()
    adapter = _adapter(redex, tag="py-prom")
    body = adapter.prometheus_text()
    assert "dataforts_blob" in body
    # `# HELP` + `# TYPE` lines are the Prometheus text-exposition
    # shape; assert both present so the bytes are readable by a
    # real scraper.
    assert "# HELP" in body
    assert "# TYPE" in body
    # The adapter id should surface on at least one label line.
    assert "py-prom" in body


def test_mesh_blob_adapter_repr_includes_adapter_id() -> None:
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-repr")
    assert "py-repr" in repr(adapter)
    assert adapter.adapter_id == "py-repr"


def test_mesh_blob_adapter_persistent_round_trips_across_calls() -> None:
    """`persistent=True` writes per-chunk RedexFiles to disk.
    The first adapter stores, the second adapter (against the
    same persistent dir) fetches. Chunk bytes persist; only the
    refcount + metrics state is per-process per the bin's
    documented model.
    """
    with tempfile.TemporaryDirectory() as raw_dir:
        dir_path = Path(raw_dir)
        payload, ref = _payload_and_ref(b"persistent across processes... well, calls")

        # First adapter: store + close.
        redex_a = Redex(persistent_dir=str(dir_path))
        adapter_a = MeshBlobAdapter(redex_a, "py-persist", persistent=True)
        adapter_a.store(ref, payload)
        # No explicit close — Python GC drops the Redex; the
        # chunk file is already flushed inside `store`.
        del adapter_a
        del redex_a

        # Second adapter against the SAME dir.
        redex_b = Redex(persistent_dir=str(dir_path))
        adapter_b = MeshBlobAdapter(redex_b, "py-persist", persistent=True)
        # The chunk file persisted on disk; fetch must succeed.
        fetched = adapter_b.fetch(ref)
        assert fetched == payload
