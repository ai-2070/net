"""``fold.query.v2`` from Python (AGGREGATOR_MULTI_HOST_PLAN.md, slice Q4).

Serving ``fold.query.v2`` is the aggregator daemon's job, which no Python
program can stand up. So the live check runs against a peer in the shape
of a pre-v2 daemon: one with no channel registry, as every daemon before
Q1 ran. ``mesh_pair`` builds both nodes with ``permissive_channels=True``,
which installs none. The v2 call must come back as the typed
``FoldQueryServerTooOld`` (``.kind == "server-too-old"``). That proves the
request reached the peer and the peer's ``NotFound`` answer was mapped,
rather than a timeout or a local refusal.

Why registry-less: the reply channel is per service
(``<service>.replies.<origin>``). A peer with a registry lists only the
reply channels of services it serves, so the caller's reply-channel
subscribe for an unserved service is refused, and the call fails as
``transport`` before any ``NotFound``. Named-group and unknown-group
answers are covered by the Rust tests against real registries
(``tests/aggregator_fold_query.rs``, ``aggregator-daemon/tests``).
"""

from __future__ import annotations

import asyncio

import pytest

try:
    from net import (  # noqa: I001 — conditional to match feature gate
        AsyncFoldQueryClient,
        FoldQueryClient,
        FoldQueryClientError,
        FoldQueryServerTooOld,
    )
except ImportError as _agg_import_err:
    pytestmark = pytest.mark.skip(
        reason=(
            "net bindings built without the `aggregator` feature; rebuild "
            "with `maturin develop --features net,aggregator` "
            f"({_agg_import_err})"
        )
    )


def test_server_too_old_is_a_fold_query_error() -> None:
    assert issubclass(FoldQueryServerTooOld, FoldQueryClientError)


def test_v2_calls_against_a_peer_without_v2_are_server_too_old(mesh_pair) -> None:
    a, b = mesh_pair
    client = FoldQueryClient(a).with_ttl(0).with_deadline(3000)
    try:
        for call in (
            lambda: client.query_group_latest(b.node_id, 1, "some-group"),
            lambda: client.query_group_summarize_now(b.node_id, 1),
        ):
            with pytest.raises(FoldQueryServerTooOld) as info:
                call()
            assert info.value.kind == "server-too-old"
            assert "fold.query.v2" in info.value.server_detail
    finally:
        # The client holds a reference to the node; drop it before the
        # fixture shuts the node down.
        del client


def test_async_v2_call_against_a_peer_without_v2_is_server_too_old(mesh_pair) -> None:
    a, b = mesh_pair
    client = AsyncFoldQueryClient(FoldQueryClient(a).with_ttl(0).with_deadline(3000))

    async def run() -> None:
        with pytest.raises(FoldQueryServerTooOld) as info:
            await client.query_group_latest(b.node_id, 1)
        assert info.value.kind == "server-too-old"

    try:
        asyncio.run(run())
    finally:
        del client
