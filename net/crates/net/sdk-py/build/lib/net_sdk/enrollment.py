"""Device enrollment — the invite -> join -> approve handshake.

Re-exports the canonical implementation from the maturin-built ``net`` wheel so
``net_sdk`` users reach the V2 Phase-1 enrollment surface
(`HERMES_INTEGRATION_PLAN_V2.md`) in one import — the same shape a Hermes plugin
depends on — without reaching into the raw ``net`` package::

    from net_sdk.enrollment import OperatorEnrollment, InviteToken, JoinRequest

The handshake (mint / sign / verify / approve), the single-use enforcement, and
the ``root -> device`` grant all live once in the Rust core (bridge-SDK
doctrine H2: no logic in bindings); this module only re-exports them. **H8 (no
key material, ever):** :meth:`JoinRequest.create` and
:class:`OperatorEnrollment` take opaque :class:`~net.Identity` handles — private
ed25519 seeds never cross into Python; everything else is a public entity-id, an
invite string, or signed chain bytes.

- :class:`InviteToken` — a pre-authorization to *ask* to join (root anchor +
  rendezvous + single-use nonce + short TTL); ``encode`` / ``decode`` is the
  copy-paste / QR string.
- :class:`JoinRequest` — the device's signed request (its own key never
  leaves the device); ``create`` / ``verify_self_signature``.
- :class:`JoinOutcome` — the operator's admitted / rejected response;
  ``into_chain`` verifies the grant anchors at the invited mesh + this device.
- :class:`OperatorEnrollment` — the operator device-lifecycle facade:
  ``invite`` / ``approve`` / ``revoke`` / ``devices`` / ``forget``.
- :class:`DeviceRecord` — one enrolled device in the inventory.
- ``fingerprint`` — the short human-comparable mesh-root fingerprint.

Present iff the wheel was built with the ``delegation`` feature (the default
one is).
"""

from net import (
    DeviceEnrollment,
    DeviceRecord,
    InviteToken,
    JoinOutcome,
    JoinRequest,
    OperatorEnrollment,
    fingerprint,
)

__all__ = [
    "DeviceEnrollment",
    "DeviceRecord",
    "InviteToken",
    "JoinOutcome",
    "JoinRequest",
    "OperatorEnrollment",
    "fingerprint",
]
