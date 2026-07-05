"""Consent, pins, and the native capability gateway.

Re-exports the canonical implementation from the maturin-built ``net`` wheel so
``net_sdk`` users reach the bridge's demand surface in one import — the same
shape a Hermes plugin depends on — without reaching into the raw ``net``
package:

    from net_sdk.consent import CapabilityGateway, PinStore, default_pin_store_path

The store, the lock protocol, the consent decision, and the ``search /
describe / invoke`` consent gate all live once in the Rust core (bridge-SDK
doctrine #1: no logic in bindings); this module only re-exports them.

- :class:`ConsentPolicy` / :class:`PinStore` / :class:`AsyncPinStore` /
  :class:`CapabilityId` — the consent gate + the machine-shared pin store.
- ``credential_requires_consent`` — the wire-credential trust boundary.
- ``default_pin_store_path`` — the per-user store path every consumer shares.
- ``CapabilityGateway`` — the native, consent-gated ``search`` / ``describe`` /
  ``invoke`` surface over an embedded ``NetMesh`` node (present iff the wheel
  was built with the ``net`` + ``mcp`` features — the default one is).
"""

from net import (
    AsyncPinStore,
    CapabilityId,
    ConsentPolicy,
    PinsError,
    PinStore,
    credential_requires_consent,
    default_pin_store_path,
)

__all__ = [
    "AsyncPinStore",
    "CapabilityId",
    "ConsentPolicy",
    "PinsError",
    "PinStore",
    "credential_requires_consent",
    "default_pin_store_path",
]

# The native capability gateway needs both the `net` and `mcp` features. The
# shipped wheel has both; guard the import so a minimal build still exposes the
# consent/pins surface.
try:
    from net import CapabilityGateway
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__.append("CapabilityGateway")
