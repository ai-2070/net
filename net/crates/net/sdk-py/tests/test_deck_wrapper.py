"""Wrapper-level tests for ``net_sdk.deck.DeckClient`` — verifies
``from_seed`` + ``__enter__`` / ``__exit__`` dispatch correctly
through the raw pyo3 binding without exercising the substrate.

The substrate-side flow is covered by the pyo3 binding's own
tests at ``bindings/python/tests/test_deck.py``. These tests
cover the Python wrapper wiring only (kwarg forwarding,
context-manager dunders).
"""

from __future__ import annotations

import sys
import types
from typing import Any, Optional


def _install_raw_client_stub() -> type:
    """Install a minimal stub for the ``net._net`` extension's
    ``DeckClient`` (the raw pyo3 class the wrapper composes
    against). Returns the stub class so tests can assert on
    call captures."""

    class _StubRaw:
        instances: list["_StubRaw"] = []

        def __init__(
            self,
            seed: bytes,
            meshos_config: Optional[dict[str, Any]] = None,
            deck_config: Optional[dict[str, Any]] = None,
        ) -> None:
            self.seed = seed
            self.meshos_config = meshos_config
            self.deck_config = deck_config
            self.close_calls = 0
            _StubRaw.instances.append(self)

        @classmethod
        def from_meshos(cls, *args: Any, **kwargs: Any) -> "_StubRaw":
            raise AssertionError("from_meshos should not be exercised by these tests")

        def close(self) -> None:
            self.close_calls += 1

        def identity(self) -> Any:
            return object()

    # The wrapper imports `from net import DeckClient as
    # _RawClient`. The conftest stub installs a generic `net`
    # module with an opaque-class __getattr__; replace its
    # DeckClient attribute with our capturing stub so the
    # already-imported `_RawClient` alias inside the wrapper
    # also gets updated (we delete + reimport the wrapper to
    # force the rebind).
    net_pkg = sys.modules.setdefault("net", types.ModuleType("net"))
    setattr(net_pkg, "DeckClient", _StubRaw)
    sys.modules.pop("net_sdk.deck", None)
    return _StubRaw


def test_from_seed_forwards_seed_and_configs() -> None:
    stub = _install_raw_client_stub()
    stub.instances.clear()
    from net_sdk.deck import DeckClient

    seed = b"\x5a" * 32
    meshos_cfg = {"tick_interval_ms": 100}
    deck_cfg = {"snapshot_poll_interval_ms": 250}

    client = DeckClient.from_seed(seed, meshos_config=meshos_cfg, deck_config=deck_cfg)

    assert len(stub.instances) == 1
    raw = stub.instances[0]
    assert raw.seed == seed
    assert raw.meshos_config == meshos_cfg
    assert raw.deck_config == deck_cfg
    assert client._raw is raw


def test_context_manager_calls_close_on_exit() -> None:
    stub = _install_raw_client_stub()
    stub.instances.clear()
    from net_sdk.deck import DeckClient

    seed = b"\x5b" * 32
    with DeckClient.from_seed(seed) as client:
        assert isinstance(client, DeckClient)
        raw = stub.instances[-1]
        assert raw.close_calls == 0

    assert raw.close_calls == 1


def test_context_manager_swallows_exit_returns_false() -> None:
    stub = _install_raw_client_stub()
    stub.instances.clear()
    from net_sdk.deck import DeckClient

    deck = DeckClient.from_seed(b"\x5c" * 32)
    # __exit__ must return a falsy value so any in-block
    # exception propagates rather than being silently swallowed.
    assert deck.__exit__(None, None, None) is False


def test_close_is_idempotent_at_wrapper() -> None:
    stub = _install_raw_client_stub()
    stub.instances.clear()
    from net_sdk.deck import DeckClient

    deck = DeckClient.from_seed(b"\x5d" * 32)
    deck.close()
    deck.close()
    raw = stub.instances[-1]
    # Wrapper forwards every call; the raw class's own close()
    # is the idempotency backstop (verified in the pyo3 test
    # suite). Wrapper just proves no extra layer of state.
    assert raw.close_calls == 2
