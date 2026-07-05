"""Consent + pin-store binding tests (`MCP_BRIDGE_SDK_PLAN.md` P1).

Build the extension first::

    maturin develop --features consent

The pin store is the machine-shared consent file the `net mcp pin` CLI and
a running `net mcp serve` shim use. The load-bearing assertions here are
the P1 acceptance criteria: concurrent locked mutations lose nothing, and
the on-disk format is byte-compatible with the Rust core's (same
implementation — the binding never opens the file itself).
"""

import asyncio
import json
import os
import subprocess
import threading

import pytest

pytest.importorskip("net._net")

from net import (  # noqa: E402
    AsyncPinStore,
    CapabilityId,
    ConsentPolicy,
    PinsError,
    PinStore,
    credential_requires_consent,
)


# ---------------------------------------------------------------------------
# CapabilityId
# ---------------------------------------------------------------------------


def test_capability_id_parses_on_first_slash() -> None:
    cid = CapabilityId.parse("homelab/github.create_issue")
    assert cid.provider == "homelab"
    assert cid.capability == "github.create_issue"
    assert cid.display() == "homelab/github.create_issue"
    # The capability half may itself contain `/`.
    nested = CapabilityId.parse("homelab/svc/sub")
    assert nested.provider == "homelab"
    assert nested.capability == "svc/sub"


def test_capability_id_rejects_missing_or_empty_halves() -> None:
    for bad in ("bareword", "/cap", "prov/"):
        with pytest.raises(ValueError):
            CapabilityId.parse(bad)


def test_capability_id_canonicalizes_provider_spellings() -> None:
    # A node id typed as hex or with whitespace keys the SAME consent /
    # pin records as the decimal form discovery emits.
    decimal = CapabilityId.parse("42/echo")
    for spelling in ("0x2a/echo", "0X2A/echo", " 42/echo", "42 /echo"):
        cid = CapabilityId.parse(spelling)
        assert cid == decimal, spelling
        assert cid.display() == "42/echo"
        assert hash(cid) == hash(decimal)


# ---------------------------------------------------------------------------
# Credential-status trust boundary + consent gate
# ---------------------------------------------------------------------------


def test_wire_none_is_gated_not_trusted() -> None:
    # The core trust boundary: a self-declared "none" never bypasses the
    # consent gate — it gates exactly like "unknown".
    for status in ("credentialed", "external_api", "unknown", "none", "", "bogus"):
        assert credential_requires_consent(status), status


def test_consent_policy_gates_everything_until_admitted() -> None:
    policy = ConsentPolicy()
    assert policy.decide("b/echo", "none") == "requires_approval"
    assert policy.requires_approval("b/echo", "credentialed")

    policy.allow("b/echo")
    assert policy.decide("b/echo", "credentialed") == "allowed"
    # A different capability is still gated.
    assert policy.requires_approval("b/other", "credentialed")

    policy.pin(CapabilityId.parse("b/slack.post"))
    assert policy.is_pinned("b/slack.post")
    assert policy.decide("b/slack.post", "external_api") == "allowed"
    assert policy.pinned() == ["b/slack.post"]
    policy.unpin("b/slack.post")
    assert policy.requires_approval("b/slack.post", "external_api")


def test_consent_policy_keys_on_canonical_identity() -> None:
    # A pin recorded under the hex spelling admits the decimal spelling —
    # identity canonicalization runs in the Rust core, not here.
    policy = ConsentPolicy()
    policy.pin("0x2a/echo")
    assert policy.decide("42/echo", "credentialed") == "allowed"


# ---------------------------------------------------------------------------
# PinStore — the machine-shared, lock-protocol store
# ---------------------------------------------------------------------------


def test_missing_store_reads_empty(tmp_path) -> None:
    store = PinStore(str(tmp_path / "pins.json"))
    assert store.approved() == []
    assert store.pending() == []
    assert store.list() == []
    assert store.state("b/echo") is None


def test_request_is_pending_only_and_never_upgrades(tmp_path) -> None:
    # The model-callable verb writes pending and grants nothing; approval
    # is the out-of-band operator step; a later request never disturbs it.
    store = PinStore(str(tmp_path / "pins.json"))
    assert store.request("b/echo") == "pending"
    assert not store.is_approved("b/echo")
    assert store.pending() == ["b/echo"]

    assert store.approve("b/echo")
    assert store.request("b/echo") == "approved"  # untouched, reported
    assert store.is_approved("b/echo")

    assert store.reject("b/echo")
    assert not store.reject("b/echo"), "rejecting an absent pin is a no-op"
    assert store.state("b/echo") is None


def test_store_is_shared_between_handles(tmp_path) -> None:
    # Two handles on the same path see each other's decisions — the model
    # for "approved in one terminal, honored by the shim in another".
    path = str(tmp_path / "pins.json")
    a, b = PinStore(path), PinStore(path)
    a.approve("b/secret")
    assert b.is_approved("b/secret")
    assert b.list() == [("b/secret", "approved")]


def test_corrupt_store_raises_not_resets(tmp_path) -> None:
    path = tmp_path / "pins.json"
    path.write_text("{ not valid json")
    store = PinStore(str(path))
    with pytest.raises(PinsError):
        store.list()
    with pytest.raises(PinsError):
        store.approve("b/echo")


def test_on_disk_format_matches_the_rust_core(tmp_path) -> None:
    # Format compatibility both ways. A file in the exact shape the Rust
    # core (and therefore `net mcp pin`) writes is readable from Python...
    path = tmp_path / "pins.json"
    path.write_text(
        json.dumps(
            {
                "pins": [
                    {"cap_id": "42/echo", "state": "approved"},
                    {"cap_id": "42/spicy", "state": "pending"},
                ]
            }
        )
    )
    store = PinStore(str(path))
    assert store.is_approved("42/echo")
    assert store.pending() == ["42/spicy"]

    # ...and a Python-side mutation persists that same shape (it IS the
    # same implementation), so the CLI/shim read it back.
    store.approve("42/spicy")
    on_disk = json.loads(path.read_text())
    assert {(p["cap_id"], p["state"]) for p in on_disk["pins"]} == {
        ("42/echo", "approved"),
        ("42/spicy", "approved"),
    }


def test_concurrent_mutations_lose_nothing(tmp_path) -> None:
    # P1 acceptance: concurrent access, no corruption. Every mutation runs
    # under the Rust core's cross-process advisory lock with the GIL
    # released, so N threads hammering one store must not lose an update
    # to a stale-snapshot race.
    path = str(tmp_path / "pins.json")
    threads_n, per_thread = 8, 10
    errors: list[BaseException] = []

    def worker(t: int) -> None:
        try:
            store = PinStore(path)
            for i in range(per_thread):
                assert store.approve(f"node{t}/tool{i}")
        except BaseException as exc:  # noqa: BLE001 — surfaced below
            errors.append(exc)

    threads = [threading.Thread(target=worker, args=(t,)) for t in range(threads_n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert not errors, errors
    approved = PinStore(path).approved()
    assert len(approved) == threads_n * per_thread
    for t in range(threads_n):
        for i in range(per_thread):
            assert f"node{t}/tool{i}" in approved


@pytest.mark.skipif(
    not os.environ.get("NET_MESH_BIN"),
    reason="set NET_MESH_BIN to the net-mesh CLI to run the cross-process test",
)
def test_cli_approval_is_visible_from_python(tmp_path) -> None:
    # Full P1 acceptance path: a pin approved via `net mcp pin approve`
    # (separate process, same lock protocol) is visible from Python, and a
    # Python-side approval is listed by the CLI.
    path = str(tmp_path / "pins.json")
    cli = os.environ["NET_MESH_BIN"]

    subprocess.run(
        [cli, "mcp", "pin", "approve", "42/echo", "--pin-store", path],
        check=True,
        capture_output=True,
    )
    assert PinStore(path).is_approved("42/echo")

    PinStore(path).approve("42/from-python")
    listed = subprocess.run(
        [cli, "mcp", "pin", "list", "--pin-store", path, "--output", "json"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    assert "42/from-python" in listed


# ---------------------------------------------------------------------------
# Async dual
# ---------------------------------------------------------------------------


def test_async_pin_store_round_trip(tmp_path) -> None:
    path = str(tmp_path / "pins.json")

    async def _run() -> list:
        store = AsyncPinStore(path)
        assert await store.request("b/echo") == "pending"
        assert not await store.is_approved("b/echo")
        assert await store.approve("b/echo")
        assert await store.is_approved("b/echo")
        assert await store.reject("b/echo")
        await store.approve("b/kept")
        return await store.list()

    rows = asyncio.run(_run())
    assert rows == [("b/kept", "approved")]
    # The sync handle sees the async handle's writes — one store, one lock.
    assert PinStore(path).is_approved("b/kept")
