"""Embedded Net node lifecycle for the ``net`` Hermes plugin.

Doctrine H1 (embedded first-class node): the plugin joins the mesh *in
process* via ``net-mesh-sdk`` — its own keypair, directly addressable — rather
than shelling out to a daemon or the ``net mcp serve`` shim. A single node is
built lazily on first use and shared across the session (thread-safe
singleton), then torn down at session end.

Configuration comes from the environment so a stock Hermes install can enable
the plugin with only ``plugins.enabled: [net]`` and a few vars:

* ``NET_MESH_BIND``          — UDP bind address (default ``127.0.0.1:0``).
* ``NET_MESH_PSK``           — 32-byte pre-shared key as 64 hex chars. Required
                               to join a real mesh; unset builds an isolated
                               dev node (tools still load, search is empty).
* ``NET_MESH_PEERS``         — JSON array of ``{addr, pubkey, node_id}`` peers to
                               connect to (the machines running ``net wrap``).
* ``NET_MESH_IDENTITY_SEED`` — 32-byte seed as hex for a stable node identity.
* ``NET_MESH_PIN_STORE``     — pin-store path; defaults to the machine-shared
                               file (``net_sdk.default_pin_store_path()``), the
                               same one ``net mcp pin`` uses.

The consent gate and the pin store live in the Rust SDK (bridge doctrine H2):
this module only builds the node + gateway and hands them out.
"""

from __future__ import annotations

import json
import logging
import os
import threading
from typing import Optional, Tuple

logger = logging.getLogger(__name__)

# An isolated dev node when NET_MESH_PSK is unset: the plugin still loads and
# its tools stay available (a healthy-but-isolated node), search just returns
# nothing. A real deployment sets NET_MESH_PSK to join the operator's mesh.
_DEFAULT_PSK = "00" * 32

_lock = threading.Lock()
# (mesh, gateway, pin_store_path) once built, else None.
_state: Optional[Tuple[object, object, str]] = None


def _config() -> dict:
    return {
        "bind_addr": os.environ.get("NET_MESH_BIND", "127.0.0.1:0"),
        "psk": os.environ.get("NET_MESH_PSK", _DEFAULT_PSK),
        "peers": os.environ.get("NET_MESH_PEERS", "").strip(),
        "pin_store": (os.environ.get("NET_MESH_PIN_STORE") or "").strip() or None,
        "identity_seed": (os.environ.get("NET_MESH_IDENTITY_SEED") or "").strip() or None,
    }


def _build() -> Tuple[object, object, str]:
    # Imports are deferred so this module imports even where the native wheel
    # is absent — check_fn() then reports the plugin unavailable rather than the
    # whole plugin failing to load and breaking the loader.
    from net import NetMesh
    from net_sdk import AsyncCapabilityGateway, default_pin_store_path

    cfg = _config()
    seed = bytes.fromhex(cfg["identity_seed"]) if cfg["identity_seed"] else None
    mesh = NetMesh(cfg["bind_addr"], cfg["psk"], identity_seed=seed)

    if cfg["peers"]:
        try:
            peers = json.loads(cfg["peers"])
        except json.JSONDecodeError as e:
            logger.warning("net plugin: NET_MESH_PEERS is not valid JSON (%s); no peers", e)
            peers = []
        for p in peers:
            try:
                mesh.connect(str(p["addr"]), str(p["pubkey"]), int(p["node_id"]))
            except Exception as e:  # noqa: BLE001 — one bad peer must not sink the node
                logger.warning("net plugin: connect to peer %s failed: %s", p.get("addr"), e)

    # Start the receive loop / router so mesh RPC reaches connected peers. Cheap
    # and safe for an isolated node too.
    mesh.start()

    pin_store = cfg["pin_store"] or default_pin_store_path()
    if not pin_store:
        raise RuntimeError(
            "net plugin: no pin-store path could be resolved; set NET_MESH_PIN_STORE"
        )
    gateway = AsyncCapabilityGateway(mesh, pin_store_path=pin_store)
    logger.info(
        "net plugin: node up (id=%s, bind=%s, pin_store=%s)",
        getattr(mesh, "node_id", "?"),
        cfg["bind_addr"],
        pin_store,
    )
    return (mesh, gateway, pin_store)


def get_state() -> Tuple[object, object, str]:
    """The shared ``(mesh, gateway, pin_store_path)``, built once."""
    global _state
    if _state is not None:
        return _state
    with _lock:
        if _state is None:  # re-check under the lock
            _state = _build()
        return _state


def check_net_available() -> bool:
    """Hermes ``check_fn``: ``True`` iff the local node is initialized and the
    SDK is usable — **never** "remote peers visible".

    A healthy-but-isolated node keeps its tools; remote absence surfaces as
    empty search results or a per-call mesh-unreachable error, not tool
    flicker. Only an SDK-import / node-construction failure past the registry's
    grace window removes the tools from the model-visible set.
    """
    try:
        get_state()
        return True
    except Exception as e:  # noqa: BLE001 — any failure => unavailable this turn
        logger.debug("net plugin unavailable: %s", e)
        return False


def gateway():
    """The :class:`AsyncCapabilityGateway` over the embedded node."""
    return get_state()[1]


def pin_store_path() -> str:
    """The machine-shared pin-store path the node consults."""
    return get_state()[2]


def shutdown() -> None:
    """Tear the node down (best-effort, idempotent). Called at session end."""
    global _state
    with _lock:
        if _state is None:
            return
        mesh = _state[0]
        _state = None
    try:
        mesh.shutdown()
    except Exception as e:  # noqa: BLE001 — session end must not fail on cleanup
        logger.debug("net plugin: shutdown error: %s", e)
