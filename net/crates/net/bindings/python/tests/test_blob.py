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


def test_mesh_blob_adapter_store_copies_input_under_gil() -> None:
    """Regression for the bytearray-soundness bug. The original
    binding declared `data: &[u8]` and captured a raw slice across
    `py.detach()` — fine for immutable `bytes`, but PyO3's older
    `&[u8]` binding silently accepted mutable `bytearray` whose
    backing buffer could move/reallocate while the GIL was
    released. The fix has two layers:

      1. PyO3 0.28's `&[u8]` is strict and rejects `bytearray`
         outright with a `TypeError`. The unsound code path is
         no longer reachable from Python; we pin this contract
         here so a future PyO3 upgrade that loosens the type
         check doesn't silently reopen the bug.
      2. Even for the accepted `bytes` path, the binding copies
         (`data.to_vec()`) BEFORE releasing the GIL, so a
         hypothetical relaxation can't reintroduce UB.

    The test asserts the type check fires (no silent acceptance)
    and that the `bytes` happy path round-trips cleanly.
    """
    redex = Redex()
    adapter = _adapter(redex)
    raw = b"bytearray-soundness regression"
    digest = _blake3_digest(raw)
    ref = BlobRef("mesh://bytearray", digest, len(raw))
    # Layer 1: bytearray is rejected at the FFI boundary — no UB.
    with pytest.raises(TypeError):
        adapter.store(ref, bytearray(raw))
    # Layer 2: bytes happy path still round-trips after the copy.
    adapter.store(ref, raw)
    assert adapter.fetch(ref) == raw


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


# ============================================================================
# Active-overflow surface (v0.3 P5)
#
# The push controller + tick driver themselves don't surface to Python yet
# (the controller borrows live Rust state — operator scripts typically
# don't drive ticks from Python). What does surface is the operator-toggle
# surface: the master switch + the threshold knobs + the runtime active
# gauge. These tests pin the bool / dict construction shape + the
# runtime setter contract.
# ============================================================================


def test_mesh_blob_adapter_overflow_default_off() -> None:
    # Out-of-the-box construction matches the v0.2 pull-only
    # posture: overflow disabled, runtime active flag false.
    # Existing call sites that don't pass `overflow=` see
    # unchanged behavior.
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-default")
    assert adapter.overflow_enabled is False
    assert adapter.overflow_active is False
    cfg = adapter.overflow_config
    assert cfg["enabled"] is False
    assert cfg["scope"] == "mesh"
    # The default thresholds are spelled out in the plan +
    # the Rust DEFAULT_* constants. Pin the surface here so a
    # future operator scanning `overflow_config` sees the
    # canonical values.
    assert cfg["high_water_ratio"] == 0.85
    assert cfg["low_water_ratio"] == 0.70
    assert cfg["max_pushes_per_tick"] == 16
    assert cfg["tick_interval_ms"] == 30_000


def test_mesh_blob_adapter_overflow_true_enables_with_defaults() -> None:
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-on", overflow=True)
    assert adapter.overflow_enabled is True
    cfg = adapter.overflow_config
    assert cfg["enabled"] is True
    # Thresholds inherit defaults — the simple-bool path
    # doesn't touch them.
    assert cfg["high_water_ratio"] == 0.85
    assert cfg["scope"] == "mesh"


def test_mesh_blob_adapter_overflow_false_explicit_is_a_no_op() -> None:
    # `overflow=False` is the explicit form of the default;
    # ensures the kwarg accepts the bool even when the value
    # matches the default. (PyO3 extract::<bool>() is strict
    # about type — int 0 falls through to the dict path.)
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-off", overflow=False)
    assert adapter.overflow_enabled is False


def test_mesh_blob_adapter_overflow_dict_overrides_thresholds() -> None:
    # A dict with threshold keys turns overflow on AND tunes
    # the thresholds. Missing keys inherit defaults; the
    # operator gets to override just the knobs they care
    # about.
    redex = Redex()
    adapter = MeshBlobAdapter(
        redex,
        "py-tuned",
        overflow={"high_water_ratio": 0.92, "max_pushes_per_tick": 4},
    )
    assert adapter.overflow_enabled is True
    cfg = adapter.overflow_config
    assert cfg["high_water_ratio"] == 0.92
    assert cfg["max_pushes_per_tick"] == 4
    # Untouched keys still at defaults.
    assert cfg["low_water_ratio"] == 0.70
    assert cfg["scope"] == "mesh"


def test_mesh_blob_adapter_overflow_dict_with_enabled_false_does_not_flip_switch() -> None:
    # Operators who want to *pre-stage* a config without
    # turning the switch on pass `enabled=False` explicitly.
    redex = Redex()
    adapter = MeshBlobAdapter(
        redex,
        "py-prestage",
        overflow={"enabled": False, "high_water_ratio": 0.95},
    )
    assert adapter.overflow_enabled is False
    cfg = adapter.overflow_config
    assert cfg["enabled"] is False
    # But the threshold override stuck.
    assert cfg["high_water_ratio"] == 0.95


def test_mesh_blob_adapter_overflow_dict_scope_parsing() -> None:
    # All four scope tokens must round-trip cleanly. The
    # parser is case-insensitive on input + lowercase on
    # output.
    redex = Redex()
    for scope in ("node", "zone", "region", "mesh"):
        adapter = MeshBlobAdapter(redex, f"py-{scope}", overflow={"scope": scope})
        assert adapter.overflow_config["scope"] == scope


def test_mesh_blob_adapter_overflow_dict_unknown_key_raises() -> None:
    # Typo-defense: an unknown key like `high_water_ration`
    # would silently fail (the override never lands; default
    # fires). Pin TypeError so the operator sees the typo.
    redex = Redex()
    with pytest.raises(TypeError) as excinfo:
        MeshBlobAdapter(
            redex,
            "py-typo",
            overflow={"high_water_ration": 0.90},
        )
    assert "high_water_ration" in str(excinfo.value)


def test_mesh_blob_adapter_overflow_dict_bad_scope_raises_valueerror() -> None:
    redex = Redex()
    with pytest.raises(ValueError) as excinfo:
        MeshBlobAdapter(redex, "py-badscope", overflow={"scope": "datacenter"})
    assert "datacenter" in str(excinfo.value)


def test_mesh_blob_adapter_overflow_wrong_type_raises_typeerror() -> None:
    # int / str / list etc. all hit the TypeError branch.
    redex = Redex()
    for bad in (1, "yes", [True]):
        with pytest.raises(TypeError):
            MeshBlobAdapter(redex, "py-badtype", overflow=bad)


def test_mesh_blob_adapter_set_overflow_enabled_runtime_toggle() -> None:
    # `set_overflow_enabled` flips the master switch without
    # rebuilding the adapter. Operators flip it live on a
    # daemon-side adapter.
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-toggle")
    assert adapter.overflow_enabled is False
    adapter.set_overflow_enabled(True)
    assert adapter.overflow_enabled is True
    # The runtime active flag stays False — no tick fired.
    assert adapter.overflow_active is False
    adapter.set_overflow_enabled(False)
    assert adapter.overflow_enabled is False


def test_mesh_blob_adapter_set_overflow_config_replaces_whole_config() -> None:
    # `set_overflow_config(dict)` atomically replaces the
    # config. Useful when enable + tune should land together.
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-replace")
    adapter.set_overflow_config({
        "enabled": True,
        "high_water_ratio": 0.88,
        "low_water_ratio": 0.66,
        "max_pushes_per_tick": 32,
        "scope": "zone",
        "tick_interval_ms": 60_000,
    })
    cfg = adapter.overflow_config
    assert cfg["enabled"] is True
    assert cfg["high_water_ratio"] == 0.88
    assert cfg["low_water_ratio"] == 0.66
    assert cfg["max_pushes_per_tick"] == 32
    assert cfg["scope"] == "zone"
    assert cfg["tick_interval_ms"] == 60_000


def test_mesh_blob_adapter_overflow_config_round_trips_through_set_then_get() -> None:
    # Compose: get a snapshot, mutate one field, set it back,
    # observe the field changed and others are unchanged.
    # Pins the (parse → render) inverse-ness contract.
    redex = Redex()
    adapter = MeshBlobAdapter(redex, "py-roundtrip", overflow=True)
    snap = adapter.overflow_config
    snap["max_pushes_per_tick"] = 7
    adapter.set_overflow_config(snap)
    assert adapter.overflow_config["max_pushes_per_tick"] == 7
    # Other fields preserved.
    assert adapter.overflow_config["high_water_ratio"] == 0.85
    assert adapter.overflow_config["scope"] == "mesh"
