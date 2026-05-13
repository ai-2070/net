"""Smoke tests for the MeshDB Python binding (slice 1).

Exercises the factory AST (`MeshQuery.at` / `between` / `latest`),
the in-memory ChainReader, the sync runner, and the Phase F cache
options. Mirrors the Rust-side meshdb tests; runs via pytest after
`maturin develop --features meshdb`.
"""

import pytest

try:
    from net import (
        CachePolicy,
        ExecuteOptions,
        InMemoryChainReader,
        MeshDbError,
        MeshQuery,
        MeshQueryRunner,
        ResultRow,
    )
except ImportError:
    pytest.skip(
        "MeshDB symbols absent — build with `maturin develop --features meshdb`",
        allow_module_level=True,
    )


def _reader_with(rows):
    """Build an InMemoryChainReader pre-populated with `rows`.

    `rows` is an iterable of `(origin, seq, payload)` tuples.
    """
    r = InMemoryChainReader()
    for origin, seq, payload in rows:
        r.append(origin, seq, payload)
    return r


def test_latest_returns_tip_row() -> None:
    reader = _reader_with([(0xAB, 1, b"v1"), (0xAB, 2, b"v2"), (0xAB, 3, b"v3")])
    runner = MeshQueryRunner(reader)
    rows = runner.execute(MeshQuery.latest(0xAB))
    assert len(rows) == 1
    assert isinstance(rows[0], ResultRow)
    assert rows[0].origin == 0xAB
    assert rows[0].seq == 3
    assert rows[0].payload == b"v3"


def test_latest_empty_chain_returns_empty_list() -> None:
    runner = MeshQueryRunner(InMemoryChainReader())
    rows = runner.execute(MeshQuery.latest(0xDEAD))
    assert rows == []


def test_at_returns_single_row() -> None:
    reader = _reader_with([(0x01, 7, b"seven")])
    runner = MeshQueryRunner(reader)
    rows = runner.execute(MeshQuery.at(0x01, 7))
    assert len(rows) == 1
    assert rows[0].seq == 7
    assert rows[0].payload == b"seven"


def test_at_missing_seq_returns_empty_list() -> None:
    runner = MeshQueryRunner(_reader_with([(0x01, 1, b"v")]))
    assert runner.execute(MeshQuery.at(0x01, 99)) == []


def test_between_half_open_range_excludes_end() -> None:
    reader = _reader_with([(0xCD, s, f"p-{s}".encode()) for s in range(1, 6)])
    runner = MeshQueryRunner(reader)
    rows = runner.execute(MeshQuery.between(0xCD, 2, 5))
    assert [r.seq for r in rows] == [2, 3, 4]


def test_between_rejects_inverted_range() -> None:
    with pytest.raises(MeshDbError) as excinfo:
        MeshQuery.between(0xCD, 5, 5)
    assert "must be <" in str(excinfo.value)


def test_repr_matches_factory_call() -> None:
    q = MeshQuery.at(0xABCDEF0123456789, 42)
    assert "MeshQuery.at" in repr(q)
    assert "seq=42" in repr(q)


def test_cache_policy_factories_round_trip() -> None:
    assert "permanent" in repr(CachePolicy.permanent())
    assert "5.000" in repr(CachePolicy.time_bound(5.0))
    assert "0.100" in repr(CachePolicy.time_bound(0.1))


def test_execute_options_constructor_defaults() -> None:
    opts = ExecuteOptions()
    assert opts.bypass_cache is False
    assert "TimeBound" not in repr(opts)  # rendered as time_bound() in repr
    assert "bypass_cache=false" in repr(opts)


def test_execute_options_with_bypass_and_permanent() -> None:
    opts = ExecuteOptions(bypass_cache=True, cache_policy=CachePolicy.permanent())
    assert opts.bypass_cache is True
    assert "permanent" in repr(opts)


def test_runner_with_cache_returns_consistent_rows_across_calls() -> None:
    reader = _reader_with([(0xEF, 1, b"x"), (0xEF, 2, b"y")])
    runner = MeshQueryRunner(reader, enable_cache=True)
    q = MeshQuery.between(0xEF, 1, 3)
    first = runner.execute(q)
    second = runner.execute(q)
    assert [(r.seq, r.payload) for r in first] == [(r.seq, r.payload) for r in second]


def test_runner_bypass_cache_returns_authoritative_results() -> None:
    reader = _reader_with([(0xEF, 1, b"x")])
    runner = MeshQueryRunner(reader, enable_cache=True)
    q = MeshQuery.latest(0xEF)
    opts = ExecuteOptions(bypass_cache=True)
    rows = runner.execute(q, opts)
    assert len(rows) == 1
    assert rows[0].payload == b"x"


def test_in_memory_reader_latest_seq_helper() -> None:
    reader = _reader_with([(0xFE, 10, b"v"), (0xFE, 20, b"v"), (0xFE, 15, b"v")])
    assert reader.latest_seq(0xFE) == 20
    assert reader.latest_seq(0xAA) is None


def test_result_row_payload_is_bytes() -> None:
    runner = MeshQueryRunner(_reader_with([(0x01, 1, b"\x00\x01\x02")]))
    rows = runner.execute(MeshQuery.at(0x01, 1))
    assert isinstance(rows[0].payload, (bytes, bytearray))
    assert bytes(rows[0].payload) == b"\x00\x01\x02"
