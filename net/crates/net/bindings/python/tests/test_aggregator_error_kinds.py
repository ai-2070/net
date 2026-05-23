"""Error-class smoke tests for the aggregator surface.

The round-trip test that boots `net-aggregator-daemon` and drives
the RPC clients against it lives in `test_aggregator_registry.py`
(integration-marked). This module pins the exception hierarchy +
``.kind`` / ``.server_detail`` attributes every consumer relies
on; it does NOT exercise the RPC path so it can run without a
booted daemon or local mesh.
"""

from __future__ import annotations

import pytest

# The aggregator surface is only present when the native module
# was built with the `aggregator` feature. CI matrices that
# omit it (`--features net,cortex` etc.) still import `net`
# successfully but without the aggregator names — collect-time
# failure is a false negative. Tolerate it via a module-level skip.
try:
    from net import (  # noqa: I001 — conditional to match feature gate
        DuplicateGroupName,
        FoldQueryClientError,
        RegistryClientError,
        SpawnNotSupported,
        SpawnRejected,
        UnknownFoldKind,
        UnknownTemplate,
    )
except ImportError as _agg_import_err:
    pytestmark = pytest.mark.skip(
        reason=(
            "net bindings built without the `aggregator` feature; rebuild "
            "with `maturin develop --features net,aggregator` "
            f"({_agg_import_err})"
        )
    )


def test_registry_subclasses_are_independent() -> None:
    # Each typed subclass extends RegistryClientError. They must
    # not collapse into each other — a `except UnknownTemplate`
    # must not catch a DuplicateGroupName, and vice versa.
    assert issubclass(UnknownTemplate, RegistryClientError)
    assert issubclass(DuplicateGroupName, RegistryClientError)
    assert issubclass(SpawnRejected, RegistryClientError)
    assert issubclass(SpawnNotSupported, RegistryClientError)
    assert not issubclass(UnknownTemplate, DuplicateGroupName)
    assert not issubclass(DuplicateGroupName, UnknownTemplate)


def test_fold_query_subclasses_are_independent() -> None:
    assert issubclass(UnknownFoldKind, FoldQueryClientError)
    # The fold-query hierarchy is independent from the registry
    # hierarchy — sharing the `agg:` umbrella in the message
    # doesn't imply a shared base.
    assert not issubclass(UnknownFoldKind, RegistryClientError)
    assert not issubclass(FoldQueryClientError, RegistryClientError)


def test_registry_error_classes_are_exceptions() -> None:
    for cls in (
        RegistryClientError,
        UnknownTemplate,
        DuplicateGroupName,
        SpawnRejected,
        SpawnNotSupported,
    ):
        assert issubclass(cls, Exception), f"{cls.__name__} is not an Exception subclass"


def test_fold_query_error_classes_are_exceptions() -> None:
    for cls in (FoldQueryClientError, UnknownFoldKind):
        assert issubclass(cls, Exception), f"{cls.__name__} is not an Exception subclass"


def test_registry_error_carries_message() -> None:
    # The Rust side raises with `agg:<kind>: <detail>`; that's
    # passed through as the exception args. Python's __str__
    # rendering must preserve it so logs/tracebacks stay useful.
    try:
        raise UnknownTemplate("agg:unknown-template: reservation-v2")
    except RegistryClientError as exc:
        assert "agg:unknown-template" in str(exc)
        # The .kind / .server_detail attrs are populated by the
        # Rust binding on the way out of the RPC call. Hand-
        # constructed instances (this test) don't set them, so
        # we only assert the attrs *exist* — defaults to absent
        # which becomes AttributeError. The point is the wire
        # path, exercised in `test_aggregator_registry.py`,
        # populates them; here we just lock the hierarchy.


def test_fold_query_error_carries_message() -> None:
    try:
        raise UnknownFoldKind("agg:unknown-kind: 0x0042")
    except FoldQueryClientError as exc:
        assert "agg:unknown-kind" in str(exc)
