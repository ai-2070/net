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

import logging
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

    Closing a persistent file runs its final fsync, so a failed close is a
    failed durability barrier and is raised, not swallowed. If the body
    itself raised, its exception propagates and carries the close failure
    as a note (Python 3.11+; logged on 3.10). Closing an already-closed
    file is not an error, so a body that closed the file itself is fine.
    """
    f = redex.open_file(name, **config)
    try:
        yield f
    except BaseException as body_error:
        try:
            f.close()
        except RedexError as close_error:
            _attach_close_failure(body_error, close_error)
        raise
    # BUG_AUDIT_2026_11_14 SDK-01: this used to swallow every RedexError
    # from close() as "idempotent close", so a with-block reported success
    # although its persistent file's final fsync had failed. Core close is
    # already idempotent; the only RedexError left here is a real failure.
    f.close()


def _attach_close_failure(body_error: BaseException, close_error: Exception) -> None:
    """Record a close failure on the body's in-flight exception without
    replacing it: the body's error is the primary one."""
    message = f"open_file_cm: closing the file also failed: {close_error!r}"
    add_note = getattr(body_error, "add_note", None)
    if add_note is not None:
        add_note(message)
    else:  # Python 3.10
        logging.getLogger(__name__).error(message)
