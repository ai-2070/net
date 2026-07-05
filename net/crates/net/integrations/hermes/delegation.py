"""Delegated agent identity for the ``net`` Hermes plugin (Phase 3, Slice A).

Derives the ``root -> machine -> gateway`` delegation chain at node init from
the user root seed (``NET_MESH_IDENTITY_SEED``), holds a shared
:class:`~net.RevocationRegistry`, and can extend the chain to per-task
subagents. The derivation, chain verification, and revocation model all live
once in the Rust SDK (``net_sdk.delegation`` — bridge doctrine H2); this module
only orchestrates them for the plugin and exposes a :meth:`GatewayDelegation.verify`
self-check that :func:`node.check_net_available` gates on.

**H8 (no key material, ever).** Identities are opaque :class:`~net.Identity`
handles; only public entity-ids and the (public-token) chain are held here. The
one private seed the plugin ever holds is the *root*, loaded from the
environment exactly as the node identity is today — and even the machine /
gateway seeds are derived and kept inside Rust, never surfaced.

**Scope boundary (Slice A vs Slice B).** This module derives, verifies, extends,
and revokes the chain. It does **not** yet make the embedded node *operate under*
the gateway identity, nor attach the chain to capability invocations on the wire
— that is Slice B (a cross-cutting wrap-protocol change: the provider must carry
and verify the chain, and audit its terminal subject). See
``HERMES_INTEGRATION_PLAN.md`` Phase 3.
"""

from __future__ import annotations

import logging
import os
import platform
import socket
from typing import Optional

logger = logging.getLogger(__name__)

# Chain lifetime. Short-TTL + re-derive is the SDK's documented v1 revocation
# story; a day balances "not renewing constantly" against "a leaked chain
# self-expires". Renewal (re-derive before expiry) is a Slice-B concern.
_DEFAULT_TTL_SECONDS = 24 * 60 * 60


def _machine_label() -> str:
    """A stable per-machine label for the delegation namespace.

    Overridable via ``NET_MESH_MACHINE_ID`` (set this when the hostname isn't
    stable, e.g. containers); defaults to the hostname.
    """
    override = (os.environ.get("NET_MESH_MACHINE_ID") or "").strip()
    if override:
        return override
    try:
        return socket.gethostname() or platform.node() or "unknown-host"
    except Exception:  # noqa: BLE001 — a hostname lookup must never sink the node
        return "unknown-host"


class GatewayDelegation:
    """The plugin's ``root -> machine -> gateway`` chain plus its shared
    revocation registry and the gateway identity handle (for subagent
    delegation).

    Construction is pure computation over the SDK — no mesh I/O — so it fails
    fast on a bad seed or a wheel missing the ``delegation`` feature (raising
    ``ImportError`` for the latter, which ``node`` treats as "run un-delegated"
    rather than as an acquisition failure).
    """

    def __init__(
        self,
        root_seed: bytes,
        *,
        machine_label: Optional[str] = None,
        gateway_label: str = "hermes",
        ttl_seconds: int = _DEFAULT_TTL_SECONDS,
    ) -> None:
        # Deferred so importing this module never requires the native wheel;
        # `node` catches the ImportError and degrades to an un-delegated node.
        from net import (  # noqa: PLC0415
            DelegationChain,
            Identity,
            RevocationRegistry,
            derive_child_identity,
        )

        self._machine_label = machine_label or _machine_label()
        self._gateway_label = gateway_label

        root = Identity.from_seed(root_seed)
        machine = derive_child_identity(root, f"machine:{self._machine_label}")
        gateway = derive_child_identity(
            root, f"gateway:{self._machine_label}:{gateway_label}"
        )

        # Public entity-ids (bytes) — the revocation lever is the *machine*
        # issuer id; the *gateway* id is the chain's leaf / presenter.
        self._root_id: bytes = root.entity_id
        self._machine_id: bytes = machine.entity_id
        self._gateway = gateway  # keep the handle: the gateway signs subagents

        self._registry = RevocationRegistry()
        self._chain = DelegationChain.derive_gateway(
            root, machine, gateway, ttl_seconds
        )

    # --- accessors ---------------------------------------------------------

    @property
    def chain(self):
        """The gateway :class:`~net.DelegationChain` (``root -> machine ->
        gateway``)."""
        return self._chain

    @property
    def registry(self):
        """The shared :class:`~net.RevocationRegistry` this chain (and any
        subagent chains extended from it) verify against."""
        return self._registry

    @property
    def gateway_id(self) -> bytes:
        """The gateway (leaf) entity-id — the presenter of the chain."""
        return self._gateway.entity_id

    @property
    def gateway_identity(self):
        """The gateway (leaf) :class:`~net.Identity` **handle** — used to build
        the caller-side signer that signs each invoke. The private key stays
        inside the handle (H8); this is plugin-internal plumbing, never
        model-visible."""
        return self._gateway

    def chain_bytes(self) -> bytes:
        """The serialized gateway chain, for the caller-side signer."""
        return self._chain.to_bytes()

    @property
    def root_id(self) -> bytes:
        """The user-root entity-id the chain anchors at."""
        return self._root_id

    @property
    def machine_id(self) -> bytes:
        """The machine entity-id — the issuer whose floor revokes this
        gateway (and its subagents)."""
        return self._machine_id

    # --- operations --------------------------------------------------------

    def verify(self) -> bool:
        """``True`` iff the gateway chain still verifies — anchored at the
        root, presented by the gateway, and neither expired nor revoked."""
        return self._chain.verify(self._gateway.entity_id, self._root_id, self._registry)

    def delegate_subagent(self, subagent_id: bytes):
        """Extend the chain to a subagent entity-id (``... -> gateway ->
        subagent``), signed by the gateway.

        The returned :class:`~net.DelegationChain` is what that subagent
        presents: individually attributable, and (via TTL / non-renewal)
        individually retireable. A ``delegate_task`` handler wires this to the
        spawned subagent's identity (Slice B carries it on the subagent's mesh
        calls).
        """
        return self._chain.extend_to_subagent(self._gateway, subagent_id)

    def revoke_gateway(self) -> None:
        """Revoke this machine's gateway delegation — and, transitively, its
        subagents — by bumping the machine issuer's revocation floor.

        Another machine's gateway under the same user root is untouched (its
        chain is issued by a *different* machine identity), which is exactly
        the Phase-3 acceptance: "revoking one machine's Hermes doesn't touch
        the other machine's."
        """
        self._registry.revoke(self._machine_id)
