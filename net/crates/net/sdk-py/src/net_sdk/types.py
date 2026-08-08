"""Shared value types.

Lives apart from `net_sdk.node` so `net_sdk.channel` can return the same
`Receipt` that `NetNode.emit` does without an import cycle (`node`
imports `TypedChannel` from `channel`).
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class Receipt:
    """Receipt from a successful ingestion."""

    shard_id: int
    timestamp: int
