"""RedEX wrapper — append-only per-channel log layer.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- Re-exports of :class:`Redex`, :class:`RedexFile`, :class:`RedexEvent`,
  :class:`RedexTailIter`, :class:`RedexError` so consumers can
  `from net_sdk.redex import Redex` instead of reaching into the
  raw binding.
- :func:`open_file_cm` — context-manager helper around
  ``Redex.open_file`` so a caller can ``with open_file_cm(redex,
  "audit") as f:`` and the file closes on scope exit.

Example::

    import net_sdk.redex as redex

    r = redex.Redex(persistent_dir="/var/lib/net/redex")
    with redex.open_file_cm(r, "orders/audit", persistent=True) as f:
        seq = f.append(b"hello")
        events = list(f.read_range(0, f.len()))
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Any, Iterator

# The PyO3 module exports these only when the wheel is built with the
# `cortex` Cargo feature. Importing from `net` (not `net._net`) keeps
# the public surface single-source.
try:
    from net import (  # type: ignore[attr-defined]
        Redex,
        RedexError,
        RedexEvent,
        RedexFile,
        RedexTailIter,
    )
except ImportError as e:  # pragma: no cover — surface a clean message
    raise ImportError(
        "RedEX SDK symbols not present in `net._net`. Rebuild the wheel "
        "with `--features cortex`, e.g. `maturin develop --features cortex`."
    ) from e


__all__ = [
    "Redex",
    "RedexError",
    "RedexEvent",
    "RedexFile",
    "RedexTailIter",
    "open_file_cm",
]


@contextmanager
def open_file_cm(redex: Redex, name: str, **config: Any) -> Iterator[RedexFile]:
    """Open a RedEX file and close it on scope exit.

    ``config`` keyword arguments mirror :class:`RedexFileConfig` —
    ``persistent``, ``fsync_policy``, ``retention_max_events``,
    ``retention_max_bytes``, ``retention_max_age_secs``,
    ``tail_buffer_size``, ``replication``. See ``redex.md`` in the
    `net-event-bus` skill for the full kwargs surface.
    """
    f = redex.open_file(name, **config)
    try:
        yield f
    finally:
        try:
            f.close()
        except RedexError:
            # Idempotent close — file may already be closed by another caller.
            pass
