"""Test fixtures for the ``net`` Hermes plugin.

The plugin package uses relative imports (as Hermes's loader requires), and its
installed name would be ``net`` — which collides with the ``net`` wheel. So we
load it under a private name (``net_hermes_plugin``) via importlib, exactly the
way Hermes loads a directory plugin as ``hermes_plugins.<slug>``, with
``submodule_search_locations`` set so ``from . import node`` resolves.

The native ``net`` wheel is expected to be installed (``maturin develop``);
``net_sdk`` is added from the source tree so the plugin's ``from net_sdk import
...`` works without a separate install.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys

import pytest

_HERE = pathlib.Path(__file__).resolve()
_PLUGIN_DIR = _HERE.parents[1]  # integrations/hermes/
# net/crates/net/sdk-py/src — make net_sdk importable from source. parents[1]
# of integrations/hermes is the crate root net/crates/net.
_SDK_PY_SRC = _PLUGIN_DIR.parents[1] / "sdk-py" / "src"
if _SDK_PY_SRC.is_dir() and str(_SDK_PY_SRC) not in sys.path:
    sys.path.insert(0, str(_SDK_PY_SRC))

_PKG = "net_hermes_plugin"


@pytest.fixture(scope="session")
def plugin():
    """Load the plugin package under a private name (relative imports intact)."""
    if _PKG in sys.modules:
        return sys.modules[_PKG]
    spec = importlib.util.spec_from_file_location(
        _PKG,
        _PLUGIN_DIR / "__init__.py",
        submodule_search_locations=[str(_PLUGIN_DIR)],
    )
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[_PKG] = mod
    spec.loader.exec_module(mod)
    return mod


class FakeCtx:
    """A stand-in for Hermes's ``PluginContext`` capturing what the plugin
    registers, so we can assert on it without a running Hermes."""

    def __init__(self) -> None:
        self.tools: dict = {}
        self.hooks: dict = {}

    def register_tool(
        self,
        *,
        name,
        toolset,
        schema,
        handler,
        check_fn=None,
        requires_env=None,
        is_async=False,
        description="",
        emoji="",
        override=False,
    ) -> None:
        self.tools[name] = {
            "toolset": toolset,
            "schema": schema,
            "handler": handler,
            "check_fn": check_fn,
            "is_async": is_async,
            "emoji": emoji,
        }

    def register_hook(self, event, fn) -> None:
        self.hooks.setdefault(event, []).append(fn)


@pytest.fixture()
def ctx() -> FakeCtx:
    return FakeCtx()


@pytest.fixture(scope="session")
def node_ready(plugin, tmp_path_factory):
    """Build the embedded node once, isolated (no NET_MESH_PSK ⇒ no peers) and
    pointed at a session temp pin store so tests never touch the real machine
    store."""
    store = tmp_path_factory.mktemp("net-plugin") / "pins.json"
    # Save + restore the env we touch: this fixture is session-scoped, so
    # `monkeypatch` (function-scoped) can't be used — without an explicit
    # restore, popping the developer's own NET_MESH_PSK would leak for the
    # whole session.
    _keys = ("NET_MESH_PIN_STORE", "NET_MESH_PSK", "NET_MESH_PEERS")
    _saved = {k: os.environ.get(k) for k in _keys}
    os.environ["NET_MESH_PIN_STORE"] = str(store)
    os.environ.pop("NET_MESH_PSK", None)
    os.environ.pop("NET_MESH_PEERS", None)
    node = plugin.node
    try:
        assert node.check_net_available(), "isolated node should be healthy/available"
        yield node
    finally:
        node.shutdown()
        for k, v in _saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
