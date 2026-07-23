"""MeshDB wrapper — query layer over the substrate's causal chain.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- Re-exports of the query AST + runner classes (:class:`MeshQuery`,
  :class:`MeshQueryRunner`, :class:`QueryBuilder`, :class:`Predicate`),
  the result shapes (:class:`ResultRow`, :class:`AggregateResult`,
  :class:`JoinedRow`, :class:`LineageEntry`), config envelopes
  (:class:`CachePolicy`, :class:`ExecuteOptions`,
  :class:`WindowBoundary`, :class:`GroupKey`), the in-memory chain
  reader (:class:`InMemoryChainReader`), and :class:`MeshDbError`.
- :func:`runner_cm` — context-manager helper that frees the runner
  on scope exit.

Example::

    import net_sdk.meshdb as meshdb

    reader = meshdb.InMemoryChainReader()
    # …populate reader…
    with meshdb.runner_cm(reader) as runner:
        query = meshdb.QueryBuilder().from_origin(0).build()
        rows = runner.execute(query)
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Iterator

try:
    from net import (  # type: ignore[attr-defined]
        AggregateResult,
        CachePolicy,
        ExecuteOptions,
        GroupKey,
        InMemoryChainReader,
        JoinedRow,
        LineageEntry,
        MeshDbError,
        MeshQuery,
        MeshQueryRunner,
        Predicate,
        QueryBuilder,
        ResultRow,
        WindowBoundary,
    )
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "MeshDB SDK symbols not present in `net._net`. Rebuild the wheel "
        "with `--features meshdb`, e.g. `maturin develop --features meshdb`."
    ) from e


__all__ = [
    "AggregateResult",
    "CachePolicy",
    "ExecuteOptions",
    "GroupKey",
    "InMemoryChainReader",
    "JoinedRow",
    "LineageEntry",
    "MeshDbError",
    "MeshQuery",
    "MeshQueryRunner",
    "Predicate",
    "QueryBuilder",
    "ResultRow",
    "WindowBoundary",
    "runner_cm",
]


@contextmanager
def runner_cm(reader: InMemoryChainReader) -> Iterator[MeshQueryRunner]:
    """Build a MeshQueryRunner over ``reader`` and free it on scope
    exit. The runner clones the reader's underlying chain on
    construction so freeing the reader before the runner is sound."""
    runner = MeshQueryRunner(reader)
    try:
        yield runner
    finally:
        # MeshQueryRunner is GC-collected; the explicit `del` is a hint
        # for callers running in long-lived processes who want
        # deterministic cleanup of the underlying Tokio runtime.
        del runner
