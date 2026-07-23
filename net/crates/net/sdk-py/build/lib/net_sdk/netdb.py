"""NetDb wrapper — unified query façade across enabled CortEX adapters.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- Re-exports of :class:`NetDb`, :class:`NetDbError`.
- :func:`netdb_cm` — context-manager helper that opens a NetDb and
  closes it on scope exit.

Example::

    import net_sdk.netdb as netdb
    import net_sdk.redex as redex

    r = redex.Redex(persistent_dir="/var/lib/net/netdb")
    with netdb.netdb_cm(r, origin_hash=0xDEADBEEF,
                        with_tasks=True, with_memories=True) as db:
        db.tasks().create(1, "write plan", now_ns=1_000_000_000)
        bundle = db.snapshot()
        # …persist `bundle.encode()`…

Snapshot bundles round-trip with the Rust + napi + Go bindings: a
bundle captured here restores cleanly in any other binding.
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import Any, Iterator

try:
    from net import (  # type: ignore[attr-defined]
        NetDb,
        NetDbError,
        Redex,
    )
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "NetDb SDK symbols not present in `net._net`. Rebuild the wheel "
        "with `--features cortex`, e.g. `maturin develop --features cortex`."
    ) from e


__all__ = [
    "NetDb",
    "NetDbError",
    "netdb_cm",
]


@contextmanager
def netdb_cm(redex: Redex, **builder_kwargs: Any) -> Iterator[NetDb]:
    """Open a NetDb and close it on scope exit. ``builder_kwargs``
    forwards to the underlying builder: ``origin_hash``, ``persistent``,
    ``with_tasks``, ``with_memories``."""
    db = NetDb.open(redex, **builder_kwargs)
    try:
        yield db
    finally:
        try:
            db.close()
        except NetDbError:
            pass
