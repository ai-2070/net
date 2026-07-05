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
* ``NET_MESH_IDENTITY_SEED`` — 32-byte seed as hex. This is the **user root**
                               identity; when set, the node derives a
                               ``root -> machine -> gateway`` delegation chain
                               from it (Phase 3, see ``delegation.py``). Unset ⇒
                               an un-delegated dev node.
* ``NET_MESH_MACHINE_ID``    — stable per-machine label for the delegation
                               namespace (default: hostname). Set it where the
                               hostname isn't stable (containers).
* ``NET_MESH_PIN_STORE``     — pin-store path; defaults to the machine-shared
                               file (``net_sdk.default_pin_store_path()``), the
                               same one ``net mcp pin`` uses.

The consent gate, the pin store, and the delegation chain all live in the Rust
SDK (bridge doctrine H2): this module only builds the node + gateway + chain
and hands them out.
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
# (mesh, gateway, pin_store_path, delegation_or_None) once built, else None.
# `delegation` is a `delegation.GatewayDelegation` when a root seed is set and
# the wheel has the `delegation` feature, else None (un-delegated dev node).
_state: Optional[Tuple[object, object, str, object]] = None


def _config() -> dict:
    return {
        "bind_addr": os.environ.get("NET_MESH_BIND", "127.0.0.1:0"),
        "psk": os.environ.get("NET_MESH_PSK", _DEFAULT_PSK),
        "peers": os.environ.get("NET_MESH_PEERS", "").strip(),
        "pin_store": (os.environ.get("NET_MESH_PIN_STORE") or "").strip() or None,
        "identity_seed": (os.environ.get("NET_MESH_IDENTITY_SEED") or "").strip() or None,
    }


def _build() -> Tuple[object, object, str, object]:
    # Imports are deferred so this module imports even where the native wheel
    # is absent — check_fn() then reports the plugin unavailable rather than the
    # whole plugin failing to load and breaking the loader.
    from net import NetMesh
    from net_sdk import AsyncCapabilityGateway, default_pin_store_path

    cfg = _config()
    seed = bytes.fromhex(cfg["identity_seed"]) if cfg["identity_seed"] else None

    # Phase 3 (Slice A): derive the `root -> machine -> gateway` delegation
    # chain from the root seed *before* touching the mesh, so an acquisition
    # failure short-circuits cleanly. A wheel without the `delegation` feature
    # degrades to an un-delegated node (logged, ImportError); with the feature,
    # any other derivation failure (e.g. a malformed seed) propagates so
    # check_fn reports unavailable — never silently degrade to machine identity
    # (plan Phase 3). No root seed ⇒ un-delegated dev node, tools still load.
    delegation = None
    if seed is not None:
        try:
            from .delegation import GatewayDelegation

            delegation = GatewayDelegation(seed)
        except ImportError as e:
            logger.info(
                "net plugin: delegation surface unavailable (%s); running un-delegated", e
            )

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
    # Phase 3 Slice B2: when delegated, the gateway signs + attaches the
    # delegation chain on every invoke (from the gateway leaf key held in the
    # chain), so a remote provider running a DelegationGate admits by verified
    # delegation and audits this gateway. Un-delegated ⇒ plain gateway.
    if delegation is not None:
        gateway = AsyncCapabilityGateway(
            mesh,
            pin_store_path=pin_store,
            delegation_leaf=delegation.gateway_identity,
            delegation_chain=delegation.chain_bytes(),
        )
        logger.info(
            "net plugin: gateway delegation acquired (machine=%s, gateway=0x%s)",
            delegation._machine_label,
            delegation.gateway_id.hex()[:16],
        )
    else:
        gateway = AsyncCapabilityGateway(mesh, pin_store_path=pin_store)
    logger.info(
        "net plugin: node up (id=%s, bind=%s, pin_store=%s, delegated=%s)",
        getattr(mesh, "node_id", "?"),
        cfg["bind_addr"],
        pin_store,
        delegation is not None,
    )
    return (mesh, gateway, pin_store, delegation)


def get_state() -> Tuple[object, object, str, object]:
    """The shared ``(mesh, gateway, pin_store_path, delegation)``, built once."""
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

    Phase 3: when the node is delegated, the gateway chain must also still
    verify — a revoked or expired delegation removes the tools rather than
    letting the model invoke under an invalid chain (plan: delegation
    acquisition/renewal failure ⇒ Net tools unavailable, never a silent
    degrade to the machine/root identity).
    """
    try:
        state = get_state()
    except Exception as e:  # noqa: BLE001 — any failure => unavailable this turn
        logger.debug("net plugin unavailable: %s", e)
        return False
    delegation = state[3]
    if delegation is not None:
        try:
            if not delegation.verify():
                logger.info(
                    "net plugin: gateway delegation invalid (revoked/expired); "
                    "tools unavailable"
                )
                return False
        except Exception as e:  # noqa: BLE001 — a verify error is unavailable, not a crash
            logger.debug("net plugin: delegation verify error: %s", e)
            return False
    return True


def gateway():
    """The :class:`AsyncCapabilityGateway` over the embedded node."""
    return get_state()[1]


def pin_store_path() -> str:
    """The machine-shared pin-store path the node consults."""
    return get_state()[2]


def delegation():
    """The session's :class:`delegation.GatewayDelegation`, or ``None`` when the
    node is running un-delegated (no ``NET_MESH_IDENTITY_SEED``, or a wheel
    without the ``delegation`` feature)."""
    return get_state()[3]


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
