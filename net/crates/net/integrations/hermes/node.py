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

# The operator device-enrollment coordinator + its live serve handle, built
# lazily on first mesh-admin tool use (needs a root seed). `(operator, handle)`.
_operator: Optional[Tuple[object, object]] = None

# The device-side silent-renewal service (device-mode: this machine is an
# enrolled device keeping its own grant fresh), or None. Started at node build.
_renewal: Optional[object] = None

# The `root -> device` grant lifetime an enrolled device receives: **1 year**
# (the token ceiling). The lifecycle model:
#   * long grant so an enrolled device survives restarts without re-pairing;
#   * **silent, automatic renewal** while the device is healthy and root policy
#     permits (re-issued before expiry, re-recording the device so its expiry
#     stays fresh) — a follow-on to this slice;
#   * **manual revocation is always immediate** — bumping the device's floor
#     denies its next invocation regardless of the grant's remaining lifetime;
#   * an **expiry warning** surfaces (via `net_mesh_devices`) before the annual
#     grant lapses, so a device that renewal couldn't reach gets attention.
_GRANT_TTL_SECONDS = 365 * 24 * 60 * 60


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
    seed = None
    if cfg["identity_seed"]:
        try:
            seed = bytes.fromhex(cfg["identity_seed"])
        except ValueError as e:
            # A malformed seed otherwise raises a bare `ValueError` that surfaces
            # only as a generic "plugin unavailable" — name the env var + format
            # so the misconfiguration is obvious (like the NET_MESH_PEERS guard).
            raise RuntimeError(
                "net plugin: NET_MESH_IDENTITY_SEED must be a 32-byte identity "
                f"seed as 64 hex chars; got an unparseable value ({e})"
            ) from e

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

    # `permissive_channels=True` matches the Rust default (no strict
    # ChannelConfigRegistry on the node). Required for the mesh nRPC surface —
    # capability invoke *and* enrollment serving use reply channels whose names
    # are dynamic per-caller-origin and can't be pre-registered. Capability
    # security is unaffected (owner-scope / consent / delegation gate at the
    # capability layer, not the channel layer).
    mesh = NetMesh(cfg["bind_addr"], cfg["psk"], identity_seed=seed, permissive_channels=True)

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
                # A non-dict entry (`p["addr"]` above raised) must not raise
                # again in the logging path — `p.get` only exists on a mapping.
                addr = p.get("addr") if isinstance(p, dict) else p
                logger.warning("net plugin: connect to peer %s failed: %s", addr, e)

    # Start the receive loop / router so mesh RPC reaches connected peers. Cheap
    # and safe for an isolated node too. From here on, roll the started mesh back
    # on any failure so an init error leaves no live loop / socket behind — init
    # stays cleanly retryable rather than leaking background state.
    mesh.start()
    try:
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
    except BaseException:
        # Best-effort rollback; never mask the original error with a cleanup one.
        try:
            mesh.shutdown()
        except Exception:  # noqa: BLE001
            logger.debug(
                "net plugin: mesh shutdown during init rollback failed", exc_info=True
            )
        raise
    # Device-mode (best-effort): if this machine is an enrolled device, keep its
    # own grant silently fresh. Never let a device-renewal setup failure break
    # the node — its capability / operator features still load.
    try:
        _start_device_renewal(mesh)
    except Exception:  # noqa: BLE001
        logger.warning("net plugin: device-renewal setup failed", exc_info=True)

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


def delegation_valid_for_invoke() -> bool:
    """``True`` unless a delegation is present but no longer verifies
    (revoked / expired).

    The invoke path checks this so it never signs + sends under an invalid chain
    — the provider re-verifies revocation on every invoke and would reject it
    anyway, but this fails fast at the source (and covers callers that bypass
    the tools' ``check_fn``, e.g. promoted pinned-tool handlers). Search /
    describe don't carry the chain, so they don't consult this.
    """
    try:
        d = get_state()[3]
    except Exception:  # noqa: BLE001 — a node-build failure surfaces via gateway() instead
        return True
    if d is None:
        return True
    try:
        return d.verify()
    except Exception:  # noqa: BLE001 — a verify error is "invalid", not a crash
        return False


def _env_int(name: str, default: int) -> int:
    raw = (os.environ.get(name) or "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("net plugin: %s is not an integer (%r); using default", name, raw)
        return default


def _probe_writable(path: str) -> None:
    """Prove `path`'s location is writable — create the parent directory and
    write + remove a probe file — so a first-run enrollment can abort *before*
    the join burns its single-use invite. Raises on failure."""
    parent = os.path.dirname(path) or "."
    os.makedirs(parent, exist_ok=True)
    probe = path + ".probe"
    try:
        with open(probe, "wb") as f:
            f.write(b"net-mesh enrollment write probe")
    finally:
        try:
            os.remove(probe)
        except OSError:
            pass


def _start_device_renewal(mesh_handle) -> None:
    """Device-mode: if this machine is an enrolled device
    (``NET_MESH_DEVICE_ENROLLMENT`` points at a persisted enrollment, or
    ``NET_MESH_INVITE`` is set for a first run), load or create the enrollment
    and start the silent-renewal loop so its `root -> device` grant stays fresh
    without re-pairing. Best-effort — invoked under a try/except by the caller,
    so a device-renewal failure never breaks the node.
    """
    global _renewal
    path = (os.environ.get("NET_MESH_DEVICE_ENROLLMENT") or "").strip() or None
    if not path:
        return  # not configured as a self-renewing device

    import time

    from net import DeviceEnrollment, Identity, InviteToken

    from .renewal import RenewalService

    enrollment = DeviceEnrollment.load(path)  # None if not enrolled yet
    if enrollment is None:
        invite = (os.environ.get("NET_MESH_INVITE") or "").strip() or None
        if not invite:
            logger.info(
                "net plugin: no device enrollment at %s and no NET_MESH_INVITE; "
                "device renewal idle",
                path,
            )
            return
        # First run: generate our own device key, join the operator's mesh, and
        # persist the enrollment so future starts skip re-pairing.
        #
        # The join burns the single-use invite and admits a key that exists
        # only in this process, so prove the enrollment path is writable
        # BEFORE committing — an unwritable path (typo, permissions, read-only
        # fs) must abort while the invite is still redeemable, not strand an
        # admitted device whose every restart replays a spent invite with a
        # fresh, discarded key.
        _probe_writable(path)
        device = Identity.generate()
        name = (os.environ.get("NET_MESH_DEVICE_NAME") or "").strip() or "device"
        chain = mesh_handle.join(device, invite, name, [])
        rendezvous = InviteToken.decode(invite).rendezvous
        enrollment = DeviceEnrollment(device, chain, rendezvous, int(time.time()))
        save_error = None
        for _ in range(3):
            try:
                enrollment.save(path)
                save_error = None
                break
            except Exception as e:  # noqa: BLE001 — retried; surfaced loudly below
                save_error = e
                time.sleep(0.2)
        if save_error is None:
            logger.info("net plugin: enrolled as a new device (persisted to %s)", path)
        else:
            # The invite is already spent and the admitted key lives only in
            # memory. Keep the session alive on the in-memory enrollment (the
            # renewal loop below re-attempts persistence on every renewal) but
            # say so loudly — a restart before a successful save needs a fresh
            # invite.
            logger.error(
                "net plugin: enrolled as a new device but persisting to %s "
                "failed (%s); the enrollment is only in memory — a restart "
                "before a successful save will need a fresh invite",
                path,
                save_error,
            )

    svc = RenewalService(
        mesh_handle,
        enrollment,
        path,
        check_interval=_env_int("NET_MESH_RENEWAL_INTERVAL", 24 * 60 * 60),
        renewal_window=_env_int("NET_MESH_RENEWAL_WINDOW", 30 * 24 * 60 * 60),
    )
    svc.start()
    _renewal = svc
    logger.info("net plugin: silent device renewal started")


def mesh():
    """The embedded :class:`net.NetMesh` — the raw node handle (for enrollment
    rendezvous / serving)."""
    return get_state()[0]


def _build_operator(mesh_handle) -> Tuple[object, object]:
    """Build the operator device-enrollment coordinator from the root seed and
    start serving enrollment (auto — the invite is the authorization) on the
    already-built `mesh_handle`, so minted invites are dialable. Returns
    ``(operator, serve_handle)``.

    Takes the mesh explicitly rather than calling ``mesh()`` because the caller
    already holds ``_lock`` — and ``mesh()`` → ``get_state()`` re-acquires it
    (``threading.Lock`` is not reentrant, so that would deadlock).
    """
    from net import Identity, OperatorEnrollment

    cfg = _config()
    if not cfg["identity_seed"]:
        raise RuntimeError(
            "net plugin: mesh device enrollment needs the user root identity; "
            "set NET_MESH_IDENTITY_SEED"
        )
    root = Identity.from_seed(bytes.fromhex(cfg["identity_seed"]))

    # Store paths: the machine-shared defaults (the same files `net wrap` /
    # `net identity revoke` use) unless overridden — tests point these at a
    # temp dir so they never touch the real inventory.
    dev = (os.environ.get("NET_MESH_DEVICE_STORE") or "").strip() or None
    rev = (os.environ.get("NET_MESH_REVOCATION_STORE") or "").strip() or None
    if dev and rev:
        operator = OperatorEnrollment(root, dev, rev)
    else:
        operator = OperatorEnrollment.with_default_paths(root)

    # Serve enrollment on the node so an operator's invite can actually be
    # redeemed. Auto-admit (invite-as-authorization) is the v1 model; an
    # operator-approval-gated serve is the follow-up.
    handle = mesh_handle.serve_enrollment_auto(operator, _GRANT_TTL_SECONDS)
    return (operator, handle)


def operator():
    """The :class:`net.OperatorEnrollment` for this node's root, serving
    enrollment on the node. Built once (needs ``NET_MESH_IDENTITY_SEED``).

    Raises :class:`RuntimeError` when no root seed is set — device enrollment
    is an operator action that requires the user root to sign delegations.
    """
    global _operator
    if _operator is not None:
        return _operator[0]
    # Fail fast (and cheaply — no node build) when there's no root to sign as.
    if not _config()["identity_seed"]:
        raise RuntimeError(
            "net plugin: mesh device enrollment needs the user root identity; "
            "set NET_MESH_IDENTITY_SEED"
        )
    # Build the node BEFORE taking our lock: `mesh()` → `get_state()` acquires
    # `_lock` itself, and `threading.Lock` is not reentrant.
    mesh_handle = mesh()
    with _lock:
        if _operator is None:
            _operator = _build_operator(mesh_handle)
        return _operator[0]


def shutdown() -> None:
    """Tear the node down (best-effort, idempotent). Called at session end."""
    global _state, _operator, _renewal
    with _lock:
        op_handle = _operator[1] if _operator is not None else None
        _operator = None
        renewal = _renewal
        _renewal = None
        mesh_handle = _state[0] if _state is not None else None
        _state = None
    if renewal is not None:
        try:
            renewal.stop()
        except Exception:  # noqa: BLE001 — cleanup must not fail session end
            logger.debug("net plugin: renewal stop failed", exc_info=True)
    if op_handle is not None:
        try:
            op_handle.stop()
        except Exception:  # noqa: BLE001 — cleanup must not fail session end
            logger.debug("net plugin: enrollment serve-handle stop failed", exc_info=True)
    if mesh_handle is not None:
        try:
            mesh_handle.shutdown()
        except Exception as e:  # noqa: BLE001 — session end must not fail on cleanup
            logger.debug("net plugin: shutdown error: %s", e)
