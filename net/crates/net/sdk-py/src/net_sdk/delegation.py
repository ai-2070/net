"""Delegated agent identity — `root -> machine -> gateway -> subagent`.

Re-exports the canonical implementation from the maturin-built ``net`` wheel so
``net_sdk`` users reach the Phase-3 delegation surface
(`HERMES_INTEGRATION_PLAN.md`) in one import — the same shape a Hermes plugin
depends on — without reaching into the raw ``net`` package::

    from net_sdk.delegation import DelegationChain, RevocationRegistry, derive_child_identity

The derivation, the token-chain verification, and the revocation model all live
once in the Rust core (bridge-SDK doctrine H2: no logic in bindings); this
module only re-exports them. **H8 (no key material, ever):** every function
here takes and returns opaque :class:`~net.Identity` handles and *public*
entity-ids / chain bytes — private ed25519 seeds never cross into Python.

- :class:`DelegationChain` — a `root -> machine -> gateway (-> subagent)`
  chain that attributes a capability invocation to the terminal agent
  identity; ``derive_gateway`` / ``extend_to_subagent`` / ``verify``.
- :class:`RevocationRegistry` — the shared per-issuer revocation floor;
  revoking a machine identity kills its gateway chain and that gateway's
  subagents, while another machine's chain is untouched.
- ``derive_child_identity`` — deterministic child-``Identity`` derivation
  from a parent (stable across restarts, no extra persistence).
- ``GATEWAY_DELEGATION_CHANNEL`` — the channel the delegation binds to.

Present iff the wheel was built with the ``delegation`` feature (the default
one is).
"""

from net import (
    GATEWAY_DELEGATION_CHANNEL,
    DelegationChain,
    RevocationRegistry,
    default_revocation_store_path,
    derive_child_identity,
)

__all__ = [
    "GATEWAY_DELEGATION_CHANNEL",
    "DelegationChain",
    "RevocationRegistry",
    "default_revocation_store_path",
    "derive_child_identity",
]
