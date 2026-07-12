"""Manual validation: load the ``net`` plugin under Hermes's REAL loader.

This is NOT part of the automated pytest suite (its filename has no ``test_``
prefix, so pytest skips it). It needs a Hermes checkout **and** a ``net-mesh``
wheel built for the SAME CPython ABI as Hermes's interpreter — infra the net
repo's CI doesn't have — so run it by hand to close the real-Hermes-loader gap.

It drives the plugin through Hermes's real ``PluginContext`` ->
``tools.registry`` (not a mock), then through ``registry.get_definitions`` (the
model-facing assembly, which runs each tool's ``check_fn`` and the schema
sanitizer). That proves the plugin registers and its tools survive assembly
exactly as they would in a running Hermes, and that the embedded node builds +
tears down cleanly in Hermes's interpreter.

Recipe (Hermes venv = CPython 3.x, e.g. 3.13):

    # 1. Build a net-mesh wheel for Hermes's interpreter and extract it (the
    #    shipped wheels are ABI-specific — a cp310 wheel won't import on 3.13):
    cd <net>/net/crates/net/bindings/python
    .venv/Scripts/python -m maturin build -i <hermes-venv-python> --out /tmp/w
    <hermes-venv-python> -m zipfile -e /tmp/w/net_mesh-*.whl /tmp/net-ext

    # 2. Run this under Hermes's interpreter, with the extracted wheel, the
    #    net_sdk source, and the Hermes checkout all on PYTHONPATH:
    HERMES_AGENT_DIR=<hermes-checkout> \
    PYTHONPATH='/tmp/net-ext;<net>/.../sdk-py/src;<hermes-checkout>' \
      <hermes-venv-python> real_hermes_loader_check.py

Exits 0 on success (or a clean skip when the setup is incomplete), non-zero on
the first failed assertion.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
import tempfile
import types

PLUGIN_DIR = pathlib.Path(__file__).resolve().parents[1]
EXPECTED = {
    "net_search_capabilities",
    "net_describe_capability",
    "net_invoke_capability",
    "net_list_pinned_capabilities",
    "net_request_pin",
}


def _load_plugin_as_hermes():
    """Load the plugin as ``hermes_plugins.net`` — the name Hermes's loader
    gives a directory plugin — so its relative imports resolve identically."""
    if "hermes_plugins" not in sys.modules:
        pkg = types.ModuleType("hermes_plugins")
        pkg.__path__ = []  # namespace package
        sys.modules["hermes_plugins"] = pkg
    spec = importlib.util.spec_from_file_location(
        "hermes_plugins.net",
        PLUGIN_DIR / "__init__.py",
        submodule_search_locations=[str(PLUGIN_DIR)],
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["hermes_plugins.net"] = module
    spec.loader.exec_module(module)
    return module


def main() -> int:
    # Isolated node (no NET_MESH_PSK) + a temp pin store, so the check never
    # joins a real mesh or touches the machine store.
    os.environ.setdefault("NET_MESH_PIN_STORE", os.path.join(tempfile.mkdtemp(), "pins.json"))
    os.environ.pop("NET_MESH_PSK", None)
    os.environ.pop("NET_MESH_PEERS", None)

    try:
        from hermes_cli.plugins import PluginContext, PluginManager, PluginManifest
        from tools.registry import registry

        import net  # noqa: F401  (the ABI-matched wheel)
        import net_sdk  # noqa: F401
    except ImportError as e:
        print(f"SKIP: setup incomplete ({e}). See this file's docstring for the recipe.")
        return 0

    plugin = _load_plugin_as_hermes()

    manager = PluginManager()
    manifest = PluginManifest(
        name="net", version="0.32.0", kind="standalone", key="net", path=str(PLUGIN_DIR)
    )
    ctx = PluginContext(manifest, manager)

    # [1] register(ctx) drives Hermes's REAL PluginContext -> tools.registry.
    plugin.register(ctx)
    assert manager._plugin_tool_names >= EXPECTED, manager._plugin_tool_names
    assert "on_session_end" in manager._hooks, "session-end hook not registered"
    for name in EXPECTED:
        entry = registry.get_entry(name)
        assert entry is not None, f"{name} missing from the registry"
        assert entry.toolset == "net", (name, entry.toolset)
        assert entry.is_async is True, name
        assert entry.check_fn is not None, name
    print("[1] register(ctx) -> real registry: 5 tools in toolset 'net', hook registered")

    # [2] Model-facing assembly: get_definitions runs each check_fn (which
    # builds the embedded node) and the schema sanitizer path.
    defs = registry.get_definitions(EXPECTED)
    got = {d["function"]["name"] for d in defs}
    assert EXPECTED <= got, ("dropped by assembly (check_fn failed?):", EXPECTED - got)
    print("[2] get_definitions -> all 5 net_* tools survive Hermes assembly (check_fn passed)")

    # [3] Session-end hook tears the embedded node down cleanly.
    for cb in manager._hooks["on_session_end"]:
        cb()
    print("[3] on_session_end hook tore the embedded node down cleanly")

    print("REAL HERMES LOADER VALIDATION PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
