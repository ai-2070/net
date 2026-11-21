"""Capability sensing (``net_sdk.sensing``): the pure-SDK mirror of the
``net`` wheel's sensing surface (SENSING_OLB_COMPLETION_PLAN B3).

THIN FORWARDING ONLY. The value-backed provider, the watch, its
close-during-wait and cancel-safety semantics, and the error classification
are all the Rust SDK's. This module only unwraps a :class:`MeshNode` to its
native mesh, and adds :func:`snapshots`, a generator over ``changed()`` /
``snapshot()``.

Example::

    from net_sdk import MeshNode
    from net_sdk.sensing import provide, watch, snapshots

    readiness = provide(provider, "nrpc:gpu.infer", "ready", estimated_start_ms=40)
    readiness.set("not_ready", reason=3)

    w = watch(consumer, "nrpc:gpu.infer", within_ms=500)
    for snap in snapshots(w):
        print(snap.preferred, snap.providers)

Errors raise ``SensingError``, whose message is the Rust wire string
``sensing:<kind>: <message>``. :func:`sensing_error_class` returns Rust's
classification, ``{"kind": ..., "terminal": ...}``.
"""

from __future__ import annotations

from typing import Any, Iterator, Optional

try:
    from net import (
        ReadinessProvider,
        SensingError,
        SensingSnapshot,
        SensingWatch,
    )
    from net import sensing_error_class as _native_class
    from net import sensing_provide as _native_provide
    from net import sensing_watch as _native_watch
except ImportError as e:  # pragma: no cover - exercised by the feature-gate test
    raise ImportError(
        "net_sdk.sensing requires a `net` wheel built with the `org` feature"
    ) from e

from net_sdk.mesh import _native_mesh

__all__ = [
    "ReadinessProvider",
    "SensingError",
    "SensingSnapshot",
    "SensingWatch",
    "provide",
    "sensing_error_class",
    "snapshots",
    "watch",
]


def provide(
    mesh: Any,
    capability: str,
    state: str,
    estimated_start_ms: Optional[float] = None,
    reason: Optional[int] = None,
) -> ReadinessProvider:
    """Serve readiness for ``capability`` from a stored value. ``state`` is
    ``"ready"``, ``"not_ready"`` or ``"unevaluable"``. Requires a mesh built
    with ``enable_sensing``, a durable ``identity_seed`` and a
    ``sensing_incarnation_path``."""
    return _native_provide(
        _native_mesh(mesh), capability, state, estimated_start_ms, reason
    )


def watch(
    mesh: Any,
    capability: str,
    within_ms: Optional[float] = None,
    start_within_ms: Optional[float] = None,
) -> SensingWatch:
    """Observe the readiness of this node's authorized own-organization
    providers of ``capability``. Requires a mesh built with ``enable_sensing``
    and an installed organization authority."""
    return _native_watch(_native_mesh(mesh), capability, within_ms, start_within_ms)


def sensing_error_class(error: Any) -> Optional[dict]:
    """Rust's classification of a sensing error (an exception or its
    message), or ``None`` when it is not a sensing error this build knows."""
    return _native_class(str(error))


def snapshots(w: SensingWatch) -> Iterator[SensingSnapshot]:
    """Yield the current snapshot, then a fresh one after every wake, until
    the watch is closed. A ``close()`` (from any thread) ends the iteration
    normally; any other refusal is raised."""
    while not w.closed:
        try:
            yield w.snapshot()
            w.changed()
        except SensingError as error:
            classified = sensing_error_class(error)
            if classified is not None and classified["kind"] == "watch_closed":
                return
            raise
