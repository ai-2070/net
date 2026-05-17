"""CortEX wrapper — folded queryable state over RedEX logs.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- Re-exports of :class:`TasksAdapter`, :class:`MemoriesAdapter`,
  :class:`Task`, :class:`Memory`, the watch iterators, and
  :class:`CortexError`.
- :class:`TaskStatus` literal alias matching the substrate-side enum.
- :func:`tasks_cm` / :func:`memories_cm` context-manager helpers
  around the open/close lifecycle so callers can pair the adapter
  with a ``with`` block.

Example::

    import net_sdk.cortex as cortex
    import net_sdk.redex as redex

    r = redex.Redex(persistent_dir="/var/lib/net/redex")
    with cortex.tasks_cm(r, channel="app/tasks", origin_hash=0xDEADBEEF,
                         persistent=True) as tasks:
        result = tasks.create(1, "first", now_ns=1_000_000_000)
        tasks.wait_for_token(result.token, deadline_ms=250)
        for snap in tasks.watch():
            ...
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Any, Iterator, Literal

try:
    from net import (  # type: ignore[attr-defined]
        CortexError,
        MemoriesAdapter,
        Memory,
        MemoryWatchIter,
        Redex,
        Task,
        TasksAdapter,
        TaskWatchIter,
    )
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "CortEX SDK symbols not present in `net._net`. Rebuild the wheel "
        "with `--features cortex`, e.g. `maturin develop --features cortex`."
    ) from e


# Substrate-side `TaskStatus` enum, mirrored as a typed literal so
# editors flag invalid string filters at lint time.
TaskStatus = Literal["pending", "completed"]


__all__ = [
    "CortexError",
    "MemoriesAdapter",
    "Memory",
    "MemoryWatchIter",
    "Task",
    "TaskStatus",
    "TasksAdapter",
    "TaskWatchIter",
    "tasks_cm",
    "memories_cm",
]


@contextmanager
def tasks_cm(
    redex: Redex, *, channel: str, origin_hash: int, **config: Any
) -> Iterator[TasksAdapter]:
    """Open a TasksAdapter and close it on scope exit. ``config`` kwargs
    mirror the underlying ``TasksAdapter.open`` signature
    (``persistent``, ``retention_max_age_secs``, ...)."""
    adapter = TasksAdapter.open(
        redex, channel=channel, origin_hash=origin_hash, **config
    )
    try:
        yield adapter
    finally:
        try:
            adapter.close()
        except CortexError:
            pass


@contextmanager
def memories_cm(
    redex: Redex, *, channel: str, origin_hash: int, **config: Any
) -> Iterator[MemoriesAdapter]:
    """Open a MemoriesAdapter and close it on scope exit. Same shape
    as :func:`tasks_cm`."""
    adapter = MemoriesAdapter.open(
        redex, channel=channel, origin_hash=origin_hash, **config
    )
    try:
        yield adapter
    finally:
        try:
            adapter.close()
        except CortexError:
            pass
