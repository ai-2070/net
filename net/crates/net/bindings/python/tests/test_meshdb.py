"""Smoke tests for the MeshDB Python binding (slice 1).

Exercises the factory AST (`MeshQuery.at` / `between` / `latest`),
the in-memory ChainReader, the sync runner, and the Phase F cache
options. Mirrors the Rust-side meshdb tests; runs via pytest after
`maturin develop --features meshdb`.
"""

import pytest

try:
    from net import (
        AggregateResult,
        CachePolicy,
        ExecuteOptions,
        GroupKey,
        InMemoryChainReader,
        JoinedRow,
        MeshDbError,
        MeshQuery,
        MeshQueryRunner,
        Predicate,
        ResultRow,
        WindowBoundary,
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


# ---------------------------------------------------------------------
# Slice 2: composite-operator factories + result decoders.
# ---------------------------------------------------------------------


def test_count_no_group_by_returns_single_aggregate_row() -> None:
    chain = 0xABCD
    reader = _reader_with([(chain, s, b"") for s in range(1, 6)])
    runner = MeshQueryRunner(reader)
    q = MeshQuery.count(MeshQuery.between(chain, 1, 10))
    rows = runner.execute(q)
    assert len(rows) == 1
    agg = rows[0].decode_aggregate()
    assert agg is not None
    assert agg.group is None
    assert agg.kind == "count"
    assert agg.count == 5
    assert agg.value == 5.0


def test_count_group_by_origin_returns_one_row_per_origin() -> None:
    reader = _reader_with(
        [
            (0xAA, 1, b""),
            (0xAA, 2, b""),
            (0xBB, 1, b""),
            (0xBB, 2, b""),
            (0xBB, 3, b""),
        ]
    )
    runner = MeshQueryRunner(reader)
    # Window over both chains, then count grouped by origin.
    # (Slice 2 doesn't ship Union; we test the group_by path by
    # feeding both chains through a `between` over a single
    # origin at a time and verifying single-bucket behavior is
    # consistent.)
    q = MeshQuery.count(MeshQuery.between(0xBB, 1, 10), group_by=["origin"])
    rows = runner.execute(q)
    decoded = [r.decode_aggregate() for r in rows]
    assert len(decoded) == 1
    assert decoded[0].kind == "count"
    assert decoded[0].group is not None
    assert decoded[0].group.kind == "origin"
    assert decoded[0].group.origin == 0xBB
    assert decoded[0].count == 3


def test_sum_avg_min_max_on_seq() -> None:
    chain = 0xAB
    reader = _reader_with([(chain, s, b"") for s in (1, 3, 7, 11)])
    runner = MeshQueryRunner(reader)
    base = MeshQuery.between(chain, 1, 20)
    rows_sum = runner.execute(MeshQuery.sum(base, "seq"))
    rows_avg = runner.execute(MeshQuery.avg(base, "seq"))
    rows_min = runner.execute(MeshQuery.min(base, "seq"))
    rows_max = runner.execute(MeshQuery.max(base, "seq"))
    assert rows_sum[0].decode_aggregate().value == 22.0
    assert rows_avg[0].decode_aggregate().value == pytest.approx(5.5)
    assert rows_min[0].decode_aggregate().value == 1.0
    assert rows_max[0].decode_aggregate().value == 11.0


def test_percentile_nearest_rank_on_seq() -> None:
    chain = 0xAB
    reader = _reader_with([(chain, s, b"") for s in range(1, 11)])
    runner = MeshQueryRunner(reader)
    # p=0.9 → floor(0.9 * 9) = 8 → 9th element (0-indexed) = 9
    q = MeshQuery.percentile(MeshQuery.between(chain, 1, 20), "seq", 0.9)
    rows = runner.execute(q)
    assert rows[0].decode_aggregate().value == 9.0


def test_percentile_rejects_out_of_range_p() -> None:
    base = MeshQuery.latest(0xAA)
    with pytest.raises(MeshDbError):
        MeshQuery.percentile(base, "seq", 1.5)
    with pytest.raises(MeshDbError):
        MeshQuery.percentile(base, "seq", -0.1)


def test_distinct_count_over_json_field() -> None:
    chain = 0xCD
    reader = _reader_with(
        [
            (chain, 1, b'{"user":"alice"}'),
            (chain, 2, b'{"user":"bob"}'),
            (chain, 3, b'{"user":"alice"}'),
            (chain, 4, b'{"user":"carol"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.distinct_count(MeshQuery.between(chain, 1, 10), "user")
    rows = runner.execute(q)
    agg = rows[0].decode_aggregate()
    assert agg.kind == "distinct_count"
    assert agg.count == 3  # alice, bob, carol


def test_window_tumbling_seq_buckets_correctly() -> None:
    chain = 0xAA
    reader = _reader_with([(chain, s, f"p-{s}".encode()) for s in range(1, 8)])
    runner = MeshQueryRunner(reader)
    # size=3 → buckets [0,3) [3,6) [6,9)
    #   bucket 0: seqs 1, 2
    #   bucket 1: seqs 3, 4, 5
    #   bucket 2: seqs 6, 7
    q = MeshQuery.window(MeshQuery.between(chain, 1, 20), size=3)
    rows = runner.execute(q)
    assert len(rows) == 3
    decoded = [r.decode_window() for r in rows]
    assert decoded[0].start == 0 and decoded[0].end == 3
    assert [r.seq for r in decoded[0].rows] == [1, 2]
    assert decoded[1].start == 3 and decoded[1].end == 6
    assert [r.seq for r in decoded[1].rows] == [3, 4, 5]
    assert decoded[2].start == 6 and decoded[2].end == 9
    assert [r.seq for r in decoded[2].rows] == [6, 7]


def test_window_size_zero_rejected_at_factory() -> None:
    with pytest.raises(MeshDbError):
        MeshQuery.window(MeshQuery.latest(0xAA), size=0)


def test_inner_join_on_seq_matches_pairs() -> None:
    a, b = 0x111, 0x222
    reader = _reader_with(
        [
            (a, 1, b"a-1"),
            (a, 2, b"a-2"),
            (a, 3, b"a-3"),
            (b, 2, b"b-2"),
            (b, 4, b"b-4"),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.join(
        MeshQuery.between(a, 1, 10),
        MeshQuery.between(b, 1, 10),
        kind="inner",
        key="seq",
    )
    rows = runner.execute(q)
    decoded = [r.decode_joined() for r in rows]
    assert len(decoded) == 1
    assert decoded[0].left is not None
    assert decoded[0].right is not None
    assert decoded[0].left.payload == b"a-2"
    assert decoded[0].right.payload == b"b-2"


def test_left_outer_join_emits_unmatched_lefts() -> None:
    a, b = 0x111, 0x222
    reader = _reader_with(
        [
            (a, 1, b"a-1"),
            (a, 2, b"a-2"),
            (a, 3, b"a-3"),
            (b, 2, b"b-2"),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.join(
        MeshQuery.between(a, 1, 10),
        MeshQuery.between(b, 1, 10),
        kind="left_outer",
        key="seq",
    )
    rows = runner.execute(q)
    decoded = [r.decode_joined() for r in rows]
    assert len(decoded) == 3
    # Exactly one matched, two unmatched lefts (right=None).
    matched = [d for d in decoded if d.right is not None]
    unmatched = [d for d in decoded if d.right is None]
    assert len(matched) == 1
    assert len(unmatched) == 2
    assert all(u.left is not None for u in unmatched)


def test_payload_keyed_inner_join_on_json_field() -> None:
    a, b = 0x111, 0x222
    reader = _reader_with(
        [
            (a, 1, b'{"request_id":"r-1"}'),
            (a, 2, b'{"request_id":"r-2"}'),
            (b, 1, b'{"request_id":"r-1"}'),
            (b, 2, b'{"request_id":"r-9"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.join(
        MeshQuery.between(a, 1, 10),
        MeshQuery.between(b, 1, 10),
        kind="inner",
        key="request_id",
    )
    rows = runner.execute(q)
    decoded = [r.decode_joined() for r in rows]
    assert len(decoded) == 1
    assert decoded[0].left.payload == b'{"request_id":"r-1"}'
    assert decoded[0].right.payload == b'{"request_id":"r-1"}'


def test_sort_merge_join_returns_same_pairs_as_hash() -> None:
    a, b = 0x111, 0x222
    reader = _reader_with(
        [
            (a, 1, b"a-1"),
            (a, 2, b"a-2"),
            (a, 5, b"a-5"),
            (b, 2, b"b-2"),
            (b, 5, b"b-5"),
        ]
    )
    runner = MeshQueryRunner(reader)
    base_left = MeshQuery.between(a, 1, 10)
    base_right = MeshQuery.between(b, 1, 10)
    hash_join = MeshQuery.join(
        base_left, base_right, kind="inner", key="seq", strategy="hash_broadcast"
    )
    sort_merge = MeshQuery.join(
        base_left, base_right, kind="inner", key="seq", strategy="sort_merge"
    )
    hash_rows = sorted(
        [r.decode_joined().left.seq for r in runner.execute(hash_join)]
    )
    sm_rows = sorted(
        [r.decode_joined().left.seq for r in runner.execute(sort_merge)]
    )
    assert hash_rows == sm_rows == [2, 5]


def test_unknown_join_kind_rejected_at_factory() -> None:
    base = MeshQuery.latest(0xAA)
    with pytest.raises(MeshDbError):
        MeshQuery.join(base, base, kind="cross", key="seq")


def test_unknown_join_strategy_rejected_at_factory() -> None:
    base = MeshQuery.latest(0xAA)
    with pytest.raises(MeshDbError):
        MeshQuery.join(base, base, kind="inner", key="seq", strategy="nested_loop")


def test_group_by_payload_field_rejected_for_now() -> None:
    base = MeshQuery.latest(0xAA)
    with pytest.raises(MeshDbError):
        MeshQuery.count(base, group_by=["payload.severity"])


def test_decode_methods_return_none_on_mismatch() -> None:
    # A plain at-row carries event bytes, not a postcard
    # aggregate / joined / window payload — every decode
    # method should return None rather than raising.
    runner = MeshQueryRunner(_reader_with([(0x01, 1, b"raw-bytes")]))
    row = runner.execute(MeshQuery.at(0x01, 1))[0]
    assert row.decode_aggregate() is None
    assert row.decode_joined() is None
    assert row.decode_window() is None


def test_aggregate_result_repr_includes_kind_and_value() -> None:
    runner = MeshQueryRunner(_reader_with([(0xAA, s, b"") for s in (1, 2, 3)]))
    rows = runner.execute(MeshQuery.count(MeshQuery.between(0xAA, 1, 10)))
    agg = rows[0].decode_aggregate()
    assert "count" in repr(agg)
    assert "3" in repr(agg)


# ---------------------------------------------------------------------
# Slice 3: Filter operator + Predicate builder.
# ---------------------------------------------------------------------


def test_filter_equals_on_synthetic_seq_keeps_matching_rows() -> None:
    chain = 0xCAFE
    reader = _reader_with([(chain, s, f"p-{s}".encode()) for s in (1, 2, 3)])
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.equals("seq", "2"),
    )
    rows = runner.execute(q)
    assert len(rows) == 1
    assert rows[0].seq == 2
    assert rows[0].payload == b"p-2"


def test_filter_numeric_at_least_on_seq_keeps_upper_rows() -> None:
    chain = 0xCAFE
    reader = _reader_with([(chain, s, b"") for s in range(1, 6)])
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.numeric_at_least("seq", 3.0),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [3, 4, 5]


def test_filter_on_json_payload_field() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"severity":"low"}'),
            (chain, 2, b'{"severity":"high"}'),
            (chain, 3, b'{"severity":"high","other":"x"}'),
            (chain, 4, b"not-json"),  # falls through; row-intrinsic only
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.equals("severity", "high"),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [2, 3]


def test_filter_and_composition() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"severity":"high","region":"us"}'),
            (chain, 2, b'{"severity":"high","region":"eu"}'),
            (chain, 3, b'{"severity":"low","region":"us"}'),
            (chain, 4, b'{"severity":"high","region":"us"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.and_(
            [
                Predicate.equals("severity", "high"),
                Predicate.equals("region", "us"),
            ]
        ),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [1, 4]


def test_filter_or_composition() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"severity":"low"}'),
            (chain, 2, b'{"severity":"medium"}'),
            (chain, 3, b'{"severity":"high"}'),
            (chain, 4, b'{"severity":"critical"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.or_(
            [
                Predicate.equals("severity", "high"),
                Predicate.equals("severity", "critical"),
            ]
        ),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [3, 4]


def test_filter_not_composition() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"severity":"low"}'),
            (chain, 2, b'{"severity":"high"}'),
            (chain, 3, b'{"severity":"low"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.not_(Predicate.equals("severity", "low")),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [2]


def test_filter_numeric_in_range() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [(chain, s, f'{{"latency_ms":{s * 10}}}'.encode()) for s in range(1, 6)]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.numeric_in_range("latency_ms", 20.0, 40.0),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [2, 3, 4]


def test_filter_numeric_in_range_rejects_inverted_bounds() -> None:
    with pytest.raises(MeshDbError):
        Predicate.numeric_in_range("x", 10.0, 5.0)


def test_filter_string_prefix() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"user":"alice"}'),
            (chain, 2, b'{"user":"bob"}'),
            (chain, 3, b'{"user":"alfred"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.string_prefix("user", "al"),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [1, 3]


def test_filter_string_matches_substring() -> None:
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"path":"/api/users/alice"}'),
            (chain, 2, b'{"path":"/api/jobs/123"}'),
            (chain, 3, b'{"path":"/healthz"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.string_matches("path", "/api/"),
    )
    seqs = [r.seq for r in runner.execute(q)]
    assert seqs == [1, 2]


def test_filter_passes_when_payload_is_not_json_and_predicate_is_row_intrinsic() -> None:
    # Predicates that key off origin / seq still work on rows
    # with non-JSON payloads — synthetic_row_view always
    # populates the row-intrinsic tags.
    chain = 0xCAFE
    reader = _reader_with(
        [
            (chain, 1, b"raw-bytes-not-json"),
            (chain, 2, b"more-raw-bytes"),
        ]
    )
    runner = MeshQueryRunner(reader)
    q = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.numeric_at_least("seq", 2.0),
    )
    rows = runner.execute(q)
    assert len(rows) == 1
    assert rows[0].seq == 2


def test_filter_pipelined_with_aggregate() -> None:
    # Filter THEN aggregate — the canonical pattern. Tests that
    # composite operators chain correctly via factory calls.
    chain = 0xC0DE
    reader = _reader_with(
        [
            (chain, 1, b'{"severity":"high"}'),
            (chain, 2, b'{"severity":"high"}'),
            (chain, 3, b'{"severity":"low"}'),
            (chain, 4, b'{"severity":"high"}'),
            (chain, 5, b'{"severity":"low"}'),
        ]
    )
    runner = MeshQueryRunner(reader)
    highs = MeshQuery.filter(
        MeshQuery.between(chain, 1, 10),
        Predicate.equals("severity", "high"),
    )
    q = MeshQuery.count(highs)
    rows = runner.execute(q)
    assert rows[0].decode_aggregate().count == 3
