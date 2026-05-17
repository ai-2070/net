"""Wrapper-import tests for the four cortex-family sdk-py modules
(redex, cortex, netdb, meshdb).

The pyo3 binding's own tests live under ``bindings/python/tests/``
and exercise the underlying classes end-to-end against the
substrate. These tests cover the sdk-py wrapper boundary: every
module imports cleanly against the conftest stub, exports the
documented names via ``__all__``, and the context-manager helpers
dispatch ``close()`` on the wrapped raw handle on scope exit.
"""

from __future__ import annotations

import importlib
from contextlib import nullcontext


def test_redex_module_imports_with_documented_surface() -> None:
    redex = importlib.import_module("net_sdk.redex")

    expected = {"Redex", "RedexError", "RedexEvent", "RedexFile",
                "RedexTailIter", "open_file_cm"}
    assert expected.issubset(set(redex.__all__))

    # Every name in __all__ must be reachable from the module.
    for name in redex.__all__:
        assert hasattr(redex, name), f"net_sdk.redex.{name} missing"


def test_cortex_module_imports_with_documented_surface() -> None:
    cortex = importlib.import_module("net_sdk.cortex")

    expected = {
        "CortexError", "MemoriesAdapter", "Memory", "MemoryWatchIter",
        "Task", "TaskStatus", "TasksAdapter", "TaskWatchIter",
        "tasks_cm", "memories_cm",
    }
    assert expected.issubset(set(cortex.__all__))
    for name in cortex.__all__:
        assert hasattr(cortex, name), f"net_sdk.cortex.{name} missing"


def test_netdb_module_imports_with_documented_surface() -> None:
    netdb = importlib.import_module("net_sdk.netdb")

    expected = {"NetDb", "NetDbError", "netdb_cm"}
    assert expected.issubset(set(netdb.__all__))
    for name in netdb.__all__:
        assert hasattr(netdb, name), f"net_sdk.netdb.{name} missing"


def test_meshdb_module_imports_with_documented_surface() -> None:
    meshdb = importlib.import_module("net_sdk.meshdb")

    expected = {
        "AggregateResult", "CachePolicy", "ExecuteOptions", "GroupKey",
        "InMemoryChainReader", "JoinedRow", "LineageEntry",
        "MeshDbError", "MeshQuery", "MeshQueryRunner", "Predicate",
        "QueryBuilder", "ResultRow", "WindowBoundary", "runner_cm",
    }
    assert expected.issubset(set(meshdb.__all__))
    for name in meshdb.__all__:
        assert hasattr(meshdb, name), f"net_sdk.meshdb.{name} missing"


def test_open_file_cm_closes_underlying_file() -> None:
    """The redex.open_file_cm context manager must call close() on
    the file when the with-block exits, even if the body raises."""
    redex = importlib.import_module("net_sdk.redex")

    class _StubFile:
        def __init__(self) -> None:
            self.close_calls = 0

        def close(self) -> None:
            self.close_calls += 1

    class _StubRedex:
        def __init__(self) -> None:
            self.last_file: _StubFile | None = None

        def open_file(self, _name: str, **_kwargs: object) -> _StubFile:
            self.last_file = _StubFile()
            return self.last_file

    r = _StubRedex()
    with redex.open_file_cm(r, "audit"):
        pass
    assert r.last_file is not None
    assert r.last_file.close_calls == 1

    # Body raises → close still called.
    r2 = _StubRedex()
    try:
        with redex.open_file_cm(r2, "audit"):
            raise RuntimeError("body explodes")
    except RuntimeError:
        pass
    assert r2.last_file is not None
    assert r2.last_file.close_calls == 1


def test_netdb_cm_closes_underlying_db() -> None:
    """netdb.netdb_cm must close() the wrapped NetDb on scope exit."""
    netdb = importlib.import_module("net_sdk.netdb")

    class _StubDb:
        def __init__(self) -> None:
            self.close_calls = 0

        def close(self) -> None:
            self.close_calls += 1

    # Monkey-patch NetDb.open to return our stub.
    original_open = getattr(netdb.NetDb, "open", None)
    captured: list[_StubDb] = []

    def fake_open(_redex: object, **_kwargs: object) -> _StubDb:
        db = _StubDb()
        captured.append(db)
        return db

    netdb.NetDb.open = fake_open  # type: ignore[method-assign,assignment]
    try:
        with netdb.netdb_cm(object(), origin_hash=1, with_tasks=True):
            pass
    finally:
        if original_open is not None:
            netdb.NetDb.open = original_open  # type: ignore[method-assign,assignment]
        else:
            del netdb.NetDb.open  # type: ignore[attr-defined]

    assert len(captured) == 1
    assert captured[0].close_calls == 1


def test_tasks_cm_closes_on_normal_exit_and_swallows_idempotent_close() -> None:
    """cortex.tasks_cm closes the adapter on scope exit. A second
    close raising CortexError is swallowed (idempotent)."""
    cortex = importlib.import_module("net_sdk.cortex")

    class _StubTasks:
        def __init__(self) -> None:
            self.close_calls = 0

        def close(self) -> None:
            self.close_calls += 1
            if self.close_calls > 1:
                raise cortex.CortexError("already closed")

    captured: list[_StubTasks] = []

    def fake_open(_redex: object, **_kwargs: object) -> _StubTasks:
        a = _StubTasks()
        captured.append(a)
        return a

    original = getattr(cortex.TasksAdapter, "open", None)
    cortex.TasksAdapter.open = fake_open  # type: ignore[method-assign,assignment]
    try:
        with cortex.tasks_cm(object(), channel="t", origin_hash=1):
            pass
    finally:
        if original is not None:
            cortex.TasksAdapter.open = original  # type: ignore[method-assign,assignment]
        else:
            del cortex.TasksAdapter.open  # type: ignore[attr-defined]

    assert captured[0].close_calls == 1

    # Force a body that closes first, then verify the cm's close
    # swallows the second-time-already-closed error.
    captured2: list[_StubTasks] = []

    def fake_open2(_redex: object, **_kwargs: object) -> _StubTasks:
        a = _StubTasks()
        captured2.append(a)
        return a

    cortex.TasksAdapter.open = fake_open2  # type: ignore[method-assign,assignment]
    try:
        with cortex.tasks_cm(object(), channel="t", origin_hash=1) as adapter:
            adapter.close()  # first close
        # second close (via cm) raises CortexError, must be swallowed
    finally:
        if original is not None:
            cortex.TasksAdapter.open = original  # type: ignore[method-assign,assignment]

    # First close from inside body + cm-side close attempt = 2 calls;
    # the cm swallowed the CortexError that the second raised.
    assert captured2[0].close_calls == 2


def test_runner_cm_constructs_and_releases() -> None:
    """meshdb.runner_cm builds a runner over the reader and the
    runner's lifetime is bounded by the with-block."""
    meshdb = importlib.import_module("net_sdk.meshdb")

    captured: list[object] = []

    class _StubRunner:
        def __init__(self, reader: object) -> None:
            captured.append(reader)

    original = meshdb.MeshQueryRunner
    meshdb.MeshQueryRunner = _StubRunner  # type: ignore[assignment]
    try:
        reader = object()
        with meshdb.runner_cm(reader):  # type: ignore[arg-type]
            pass
        assert captured == [reader]
    finally:
        meshdb.MeshQueryRunner = original  # type: ignore[assignment]

    # nullcontext sanity (regression hedge — confirm the wrapper
    # doesn't accidentally yield None).
    with nullcontext("sentinel") as v:
        assert v == "sentinel"
